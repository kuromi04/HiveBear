use crate::error::{RegistryError, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

/// Progress information for download callbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_sec: f64,
}

/// Metadata stored alongside partial downloads for resume support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialMeta {
    /// The URL the `.partial` bytes came from.
    pub url: String,
    /// Full size of the file, once a response revealed it (used to show resume progress).
    pub total_bytes: Option<u64>,
    /// Expected SHA-256, when the source provided one.
    pub sha256_expected: Option<String>,
    /// Model ID owning the download.
    pub model_id: String,
    /// Final filename the partial will become.
    pub filename: String,
    /// When the download first started.
    pub started_at: DateTime<Utc>,
}

/// Total size advertised by a `Content-Range: bytes <start>-<end>/<total>` header.
///
/// Returns `None` for an unsatisfied range (`bytes */<total>` still parses) or a
/// malformed value. This is the only reliable file size on a `206` response:
/// `Content-Length` there describes just the bytes in *this* response.
fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse::<u64>().ok()
}

/// What to do with the bytes already sitting in a `.partial` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumePlan {
    /// Nothing usable on disk: start at offset 0.
    Fresh,
    /// Keep the bytes already written and continue from `offset`.
    Continue { offset: u64 },
    /// The stored partial belongs to a different URL: discard it before restarting.
    DiscardAndRestart,
}

/// Decide whether an interrupted download can be resumed.
///
/// A missing, empty or corrupt `.partial.meta` is *not* a reason to throw away
/// multi-GB of progress: we keep the bytes and rewrite the meta (the URL check is
/// impossible in that case, which is logged). Only a readable meta naming a
/// different URL proves the bytes belong to another file.
fn plan_resume(partial_len: Option<u64>, meta: Option<&PartialMeta>, url: &str) -> ResumePlan {
    let Some(len) = partial_len.filter(|len| *len > 0) else {
        return ResumePlan::Fresh;
    };

    match meta {
        Some(meta) if meta.url == url => ResumePlan::Continue { offset: len },
        Some(_) => ResumePlan::DiscardAndRestart,
        None => {
            tracing::warn!(
                "Partial download has no readable .partial.meta; resuming from {len} bytes \
                 without being able to verify the source URL"
            );
            ResumePlan::Continue { offset: len }
        }
    }
}

/// Write partial metadata atomically (fsync a sibling tmp file, then rename).
///
/// A plain truncate-then-write can leave a 0-byte `.partial.meta` behind if the
/// process is killed mid-write, which used to make the resume path skip the
/// URL check.
async fn write_meta_atomic(meta_path: &Path, meta: &PartialMeta) -> Result<()> {
    let json = serde_json::to_string(meta)?;

    let mut tmp_name = meta_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("partial.meta"))
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = meta_path.with_file_name(tmp_name);

    {
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        file.write_all(json.as_bytes()).await?;
        file.flush().await?;
        file.sync_all().await?;
    }

    tokio::fs::rename(&tmp_path, meta_path).await?;
    Ok(())
}

/// Manages model file downloads with resume support and cancellation.
pub struct DownloadManager {
    client: reqwest::Client,
    models_dir: PathBuf,
    cancellations: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    /// How many extra attempts a stream that ends short (or errors) gets.
    stream_retries: u32,
    /// Delay between those attempts.
    retry_delay: std::time::Duration,
}

impl DownloadManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self::with_retry_policy(models_dir, 5, std::time::Duration::from_secs(2))
    }

    /// Build a manager with an explicit retry policy (tests use a fast one).
    fn with_retry_policy(
        models_dir: PathBuf,
        stream_retries: u32,
        retry_delay: std::time::Duration,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("hivebear/0.2.14")
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(5)
            .build()
            .unwrap_or_default();

        Self {
            client,
            models_dir,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            stream_retries,
            retry_delay,
        }
    }

    /// Cancel an active download by model ID or filename.
    pub fn cancel_download(&self, model_id: &str) -> bool {
        let mut map = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
        let mut cancelled = false;
        if let Some(tx) = map.remove(model_id) {
            let _ = tx.send(true);
            cancelled = true;
        }
        cancelled
    }

    /// Send an HTTP GET with retry logic for transient errors (429, 5xx).
    async fn send_with_retry(
        &self,
        url: &str,
        bytes_downloaded: u64,
        max_retries: u32,
    ) -> Result<reqwest::Response> {
        let mut delay = std::time::Duration::from_secs(2);

        for attempt in 0..=max_retries {
            let mut request = self.client.get(url);
            if let Ok(token) = std::env::var("HF_TOKEN") {
                if !token.is_empty() {
                    request = request.header("Authorization", format!("Bearer {token}"));
                }
            }
            if bytes_downloaded > 0 {
                request = request.header("Range", format!("bytes={bytes_downloaded}-"));
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() || status.as_u16() == 206 {
                        return Ok(response);
                    }

                    // Transient errors: retry
                    if (status.as_u16() == 429 || status.is_server_error()) && attempt < max_retries
                    {
                        let msg = match status.as_u16() {
                            429 => "Rate limited by server".to_string(),
                            _ => format!("Server error (HTTP {})", status),
                        };
                        tracing::warn!(
                            "{msg}, retrying in {}s (attempt {}/{})",
                            delay.as_secs(),
                            attempt + 1,
                            max_retries
                        );
                        tokio::time::sleep(delay).await;
                        delay *= 2; // Exponential backoff
                        continue;
                    }

                    // Non-retryable error
                    let msg = match status.as_u16() {
                        401 | 403 => format!(
                            "Access denied (HTTP {status}). You may need to set HF_TOKEN for gated models."
                        ),
                        404 => "File not found (HTTP 404). The model file may have been moved or renamed.".to_string(),
                        416 => "Requested range not satisfiable (HTTP 416).".to_string(),
                        429 => "Rate limited (HTTP 429). Try again in a few minutes, or set HF_TOKEN for higher limits.".to_string(),
                        _ => format!("HTTP {status} for {url}"),
                    };
                    return Err(RegistryError::DownloadError(msg));
                }
                Err(e) if attempt < max_retries => {
                    tracing::warn!(
                        "Network error: {e}, retrying in {}s (attempt {}/{})",
                        delay.as_secs(),
                        attempt + 1,
                        max_retries
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(e) => {
                    return Err(RegistryError::DownloadError(format!(
                        "Download failed after {} attempts: {e}",
                        max_retries + 1
                    )));
                }
            }
        }

        unreachable!()
    }

    /// Download a file with resume support, cancellation, and optional SHA-256 verification.
    pub async fn download(
        &self,
        url: &str,
        dest_dir: &Path,
        filename: &str,
        expected_sha256: Option<&str>,
        model_id: &str,
        progress_cb: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
    ) -> Result<PathBuf> {
        let final_path = dest_dir.join(filename);

        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        } else {
            tokio::fs::create_dir_all(dest_dir).await?;
        }

        let partial_path = dest_dir.join(format!("{filename}.partial"));
        let meta_path = dest_dir.join(format!("{filename}.partial.meta"));

        // Register cancellation channel
        let (cancel_tx, cancel_rx) = watch::channel(false);
        {
            let mut map = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(model_id.to_string(), cancel_tx);
        }

        // Helper cleanup closure to unregister cancellation sender & remove partial files on error/cancel
        let cleanup_on_exit = |delete_partial: bool| {
            let mut map = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(model_id);
            if delete_partial {
                let _ = std::fs::remove_file(&meta_path);
                let _ = std::fs::remove_file(&partial_path);
            }
        };

        // Decide what to do with any bytes already on disk before requesting anything.
        let partial_len = tokio::fs::metadata(&partial_path)
            .await
            .ok()
            .map(|m| m.len());
        let previous_meta = match tokio::fs::read_to_string(&meta_path).await {
            Ok(contents) => match serde_json::from_str::<PartialMeta>(&contents) {
                Ok(meta) => Some(meta),
                Err(e) => {
                    tracing::warn!(
                        "Ignoring unreadable partial metadata at {}: {e}",
                        meta_path.display()
                    );
                    None
                }
            },
            Err(_) => None,
        };

        let mut bytes_downloaded: u64 = match plan_resume(partial_len, previous_meta.as_ref(), url)
        {
            ResumePlan::Fresh => 0,
            ResumePlan::DiscardAndRestart => {
                tracing::info!(
                    "Partial download at {} belongs to a different URL; starting over",
                    partial_path.display()
                );
                tokio::fs::remove_file(&partial_path).await.ok();
                tokio::fs::remove_file(&meta_path).await.ok();
                0
            }
            ResumePlan::Continue { offset } => {
                tracing::info!("Resuming download of {filename} from {offset} bytes");
                offset
            }
        };

        // Save partial metadata. A missing/corrupt meta was discarded above and is
        // rewritten here, so resume never depends on a usable pre-existing meta.
        let mut meta = PartialMeta {
            url: url.to_string(),
            total_bytes: previous_meta.as_ref().and_then(|m| m.total_bytes),
            sha256_expected: expected_sha256.map(String::from),
            model_id: model_id.to_string(),
            filename: filename.to_string(),
            started_at: previous_meta
                .as_ref()
                .map(|m| m.started_at)
                .unwrap_or_else(Utc::now),
        };
        write_meta_atomic(&meta_path, &meta).await?;

        let mut stream_retries_left = self.stream_retries;
        let mut known_total: Option<u64>;
        let mut last_emit = std::time::Instant::now();
        loop {
            if *cancel_rx.borrow() {
                cleanup_on_exit(true);
                return Err(RegistryError::DownloadError(
                    "Download cancelled by user".to_string(),
                ));
            }

            let response = match self.send_with_retry(url, bytes_downloaded, 3).await {
                Ok(resp) => resp,
                Err(e) => {
                    // If range not satisfiable (HTTP 416) or corrupted offset, clear partial file & retry from 0
                    if bytes_downloaded > 0 && e.to_string().contains("416") {
                        tracing::warn!("HTTP 416 Range Not Satisfiable, resetting partial file");
                        tokio::fs::remove_file(&partial_path).await.ok();
                        bytes_downloaded = 0;
                        match self.send_with_retry(url, 0, 3).await {
                            Ok(resp) => resp,
                            Err(e2) => {
                                cleanup_on_exit(true);
                                return Err(e2);
                            }
                        }
                    } else if bytes_downloaded > 0 {
                        // Keep whatever is on disk: HuggingFace supports Range, so the
                        // next attempt resumes instead of restarting a multi-GB download.
                        tracing::warn!(
                            "Request for {filename} failed after {bytes_downloaded} bytes \
                             downloaded; keeping the partial for a later resume: {e}"
                        );
                        cleanup_on_exit(false);
                        return Err(e);
                    } else {
                        cleanup_on_exit(true);
                        return Err(e);
                    }
                }
            };

            let status = response.status();
            let is_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;

            // If server returned 200 OK (Full Content), server ignored Range or redirected. Reset offset & truncate file!
            if status == reqwest::StatusCode::OK && bytes_downloaded > 0 {
                tracing::info!("Server returned 200 OK (Full Content). Resetting offset to 0.");
                bytes_downloaded = 0;
            }

            // `Content-Range` carries the real file size; `Content-Length` on a 206
            // only describes the bytes in this response, so a server closing the
            // range early would still look "complete".
            let range_total = if is_partial {
                response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_content_range_total)
            } else {
                None
            };
            let total_bytes = range_total
                .or_else(|| {
                    let content_length = response.content_length();
                    if is_partial {
                        content_length.map(|len| len + bytes_downloaded)
                    } else {
                        content_length
                    }
                })
                .or(meta.total_bytes);
            known_total = total_bytes;

            // Persist the size once it is known so the UI can show resume progress
            // for a download that gets interrupted later.
            if total_bytes.is_some() && total_bytes != meta.total_bytes {
                meta.total_bytes = total_bytes;
                if let Err(e) = write_meta_atomic(&meta_path, &meta).await {
                    tracing::warn!("Could not update {}: {e}", meta_path.display());
                }
            }

            // Open file for append if 206 Partial Content, or write/truncate if 200 OK
            let mut file = match if is_partial && bytes_downloaded > 0 {
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&partial_path)
                    .await
            } else {
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&partial_path)
                    .await
            } {
                Ok(f) => f,
                Err(e) => {
                    cleanup_on_exit(true);
                    return Err(RegistryError::from(e));
                }
            };

            let mut stream = response.bytes_stream();
            let start_time = std::time::Instant::now();
            let bytes_at_start = bytes_downloaded;
            let mut stream_error = false;

            while let Some(chunk_res) = stream.next().await {
                if *cancel_rx.borrow() {
                    cleanup_on_exit(true);
                    return Err(RegistryError::DownloadError(
                        "Download cancelled by user".to_string(),
                    ));
                }

                match chunk_res {
                    Ok(chunk) => {
                        if let Err(e) = file.write_all(&chunk).await {
                            cleanup_on_exit(true);
                            return Err(RegistryError::from(e));
                        }
                        bytes_downloaded += chunk.len() as u64;

                        if let Some(cb) = progress_cb {
                            let now = std::time::Instant::now();
                            let is_finished = total_bytes.is_some_and(|tb| bytes_downloaded >= tb);
                            // Throttle progress events to ~10 Hz (every 100ms) to prevent Android Webview IPC flooding
                            if now.duration_since(last_emit).as_millis() >= 100 || is_finished {
                                last_emit = now;
                                let elapsed = start_time.elapsed().as_secs_f64();
                                let session_bytes = bytes_downloaded.saturating_sub(bytes_at_start);
                                let bps = if elapsed > 0.0 {
                                    session_bytes as f64 / elapsed
                                } else {
                                    0.0
                                };
                                cb(DownloadProgress {
                                    bytes_downloaded,
                                    total_bytes,
                                    bytes_per_sec: bps,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Stream error during download: {e}. bytes_downloaded: {bytes_downloaded}");
                        stream_error = true;
                        break;
                    }
                }
            }

            let _ = file.flush().await;
            drop(file);

            // A stream can end "cleanly" while still being short: the server/CDN
            // closed the range early, or the body simply stopped. Never promote
            // such a file — fall through to the retry path so the resume logic can
            // fetch the rest.
            if !stream_error {
                match total_bytes {
                    Some(total) if bytes_downloaded < total => {
                        tracing::warn!(
                            "Stream for {filename} ended cleanly after {bytes_downloaded} of \
                             {total} bytes; treating it as a truncated stream"
                        );
                    }
                    _ => break, // Download completed fully!
                }
            }

            if stream_retries_left == 0 {
                // Keep the partial: HuggingFace answers Range requests, so the next
                // install attempt resumes from where this one stopped.
                tracing::error!(
                    "Download of {filename} failed after {} stream attempts; keeping {} bytes \
                     at {} to resume later",
                    self.stream_retries + 1,
                    bytes_downloaded,
                    partial_path.display()
                );
                cleanup_on_exit(false);
                return Err(RegistryError::DownloadInterrupted {
                    model_id: model_id.to_string(),
                });
            }
            stream_retries_left -= 1;

            // A retry resumes from whatever actually landed on disk.
            if partial_path.exists() {
                bytes_downloaded = tokio::fs::metadata(&partial_path)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
            } else {
                bytes_downloaded = 0;
            }

            tokio::time::sleep(self.retry_delay).await;
        }

        // Belt-and-braces: the file we are about to promote must actually have the
        // full length the source advertised.
        if let Some(total) = known_total {
            let on_disk = tokio::fs::metadata(&partial_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            if on_disk < total {
                tracing::warn!(
                    "Refusing to promote {filename}: {on_disk} of {total} bytes on disk"
                );
                cleanup_on_exit(false);
                return Err(RegistryError::DownloadInterrupted {
                    model_id: model_id.to_string(),
                });
            }
        }

        // Final verification: SHA-256 on completed partial file if expected
        if let Some(expected) = expected_sha256 {
            tracing::info!("Verifying SHA-256 checksum for {}", filename);
            let mut file = match tokio::fs::File::open(&partial_path).await {
                Ok(f) => f,
                Err(e) => {
                    cleanup_on_exit(true);
                    return Err(RegistryError::from(e));
                }
            };

            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 1024 * 1024]; // 1MB buffer

            loop {
                let n = match file.read(&mut buffer).await {
                    Ok(n) => n,
                    Err(e) => {
                        cleanup_on_exit(true);
                        return Err(RegistryError::from(e));
                    }
                };
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }

            let actual = format!("{:x}", hasher.finalize());
            if actual.to_lowercase() != expected.to_lowercase() {
                cleanup_on_exit(true);
                return Err(RegistryError::IntegrityError {
                    path: partial_path,
                    expected: expected.to_string(),
                    actual,
                });
            }
        }

        // Move partial to final
        if let Err(e) = tokio::fs::rename(&partial_path, &final_path).await {
            cleanup_on_exit(true);
            return Err(RegistryError::from(e));
        }

        tokio::fs::remove_file(&meta_path).await.ok();
        cleanup_on_exit(false);

        Ok(final_path)
    }

    /// List partial downloads that could be resumed.
    pub async fn list_partial(&self) -> Result<Vec<PartialMeta>> {
        let mut partials = Vec::new();

        if !self.models_dir.exists() {
            return Ok(partials);
        }

        let mut entries = tokio::fs::read_dir(&self.models_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let mut sub = tokio::fs::read_dir(entry.path()).await?;
                while let Some(sub_entry) = sub.next_entry().await? {
                    let name = sub_entry.file_name().to_string_lossy().to_string();
                    if name.ends_with(".partial.meta") {
                        if let Ok(contents) = tokio::fs::read_to_string(sub_entry.path()).await {
                            if let Ok(meta) = serde_json::from_str::<PartialMeta>(&contents) {
                                partials.push(meta);
                            }
                        }
                    }
                }
            }
        }

        Ok(partials)
    }
}

/// Minimal blocking HTTP/1.1 helpers shared by the download and registry tests.
/// Keeping the server here means no test needs a new dependency or the network.
#[cfg(test)]
pub(crate) mod test_http {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A tiny blocking HTTP/1.1 server for download tests.
    pub(crate) struct TestServer {
        pub(crate) url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl TestServer {
        /// `handler(attempt_index, request_head)` returns the raw response bytes.
        pub(crate) fn start<F>(handler: F) -> Self
        where
            F: Fn(usize, &str) -> Vec<u8> + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&requests);

            std::thread::spawn(move || {
                for (attempt, stream) in listener.incoming().enumerate() {
                    let Ok(mut stream) = stream else { break };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while let Ok(1) = stream.read(&mut byte) {
                        head.push(byte[0]);
                        if head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_string();
                    seen.lock().unwrap().push(head.clone());
                    let response = handler(attempt, &head);
                    let _ = stream.write_all(&response);
                    let _ = stream.flush();
                    // Drop the stream: the client sees the response end here.
                }
            });

            Self {
                url: format!("http://{addr}/model.gguf"),
                requests,
            }
        }

        pub(crate) fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// Offset requested by a `Range: bytes=<offset>-` request header.
    pub(crate) fn request_range_offset(head: &str) -> u64 {
        for line in head.lines() {
            let lower = line.to_lowercase();
            if let Some(rest) = lower.strip_prefix("range: bytes=") {
                return rest
                    .trim_end_matches('-')
                    .split('-')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
        }
        0
    }

    pub(crate) fn chunk(len: usize, fill: u8) -> Vec<u8> {
        vec![fill; len]
    }

    /// `206 Partial Content` whose `Content-Range` advertises `total`.
    pub(crate) fn response_206(offset: u64, body: &[u8], total: u64) -> Vec<u8> {
        let end = offset + body.len() as u64 - 1;
        let mut out = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
             Content-Range: bytes {offset}-{end}/{total}\r\nAccept-Ranges: bytes\r\n\
             Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_http::{chunk, request_range_offset, response_206, TestServer};
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_download_manager_creation() {
        let tmp = TempDir::new().unwrap();
        let dm = DownloadManager::new(tmp.path().to_path_buf());
        assert_eq!(dm.models_dir, tmp.path());
    }

    #[tokio::test]
    async fn test_list_partial_empty() {
        let tmp = TempDir::new().unwrap();
        let dm = DownloadManager::new(tmp.path().to_path_buf());
        let partials = dm.list_partial().await.unwrap();
        assert!(partials.is_empty());
    }

    #[test]
    fn test_parse_content_range_total() {
        assert_eq!(parse_content_range_total("bytes 0-99/1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes */1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes 0-99/*"), None);
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    #[test]
    fn test_plan_resume_keeps_progress_without_meta() {
        let url = "https://example.com/model.gguf";
        assert_eq!(plan_resume(None, None, url), ResumePlan::Fresh);
        assert_eq!(plan_resume(Some(0), None, url), ResumePlan::Fresh);
        // Corrupt/missing meta must not discard bytes that are already on disk.
        assert_eq!(
            plan_resume(Some(1024), None, url),
            ResumePlan::Continue { offset: 1024 }
        );

        let meta = PartialMeta {
            url: url.to_string(),
            total_bytes: Some(4096),
            sha256_expected: None,
            model_id: "m".into(),
            filename: "model.gguf".into(),
            started_at: Utc::now(),
        };
        assert_eq!(
            plan_resume(Some(1024), Some(&meta), url),
            ResumePlan::Continue { offset: 1024 }
        );
        assert_eq!(
            plan_resume(Some(1024), Some(&meta), "https://example.com/other.gguf"),
            ResumePlan::DiscardAndRestart
        );
    }

    /// (a) A stream that ends cleanly but short must be resumed, not promoted.
    #[tokio::test]
    async fn truncated_stream_is_resumed_not_promoted() {
        let tmp = TempDir::new().unwrap();
        const TOTAL: u64 = 1000;
        let server = TestServer::start(|attempt, head| {
            let offset = request_range_offset(head);
            if attempt == 0 {
                // Clean EOF after 50 of 1000 bytes: `Content-Length` matches the body,
                // so only `Content-Range` reveals that the file is incomplete.
                response_206(offset, &chunk(50, b'A'), TOTAL)
            } else {
                response_206(offset, &chunk((TOTAL - offset) as usize, b'B'), TOTAL)
            }
        });

        let dm = DownloadManager::with_retry_policy(
            tmp.path().to_path_buf(),
            5,
            std::time::Duration::from_millis(1),
        );
        let path = dm
            .download(&server.url, tmp.path(), "model.gguf", None, "m", None)
            .await
            .unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().len(), TOTAL);
        let requests = server.requests();
        assert_eq!(
            requests.len(),
            2,
            "the truncated first stream must trigger a resume, not a rename"
        );
        assert!(
            requests[1].to_lowercase().contains("range: bytes=50-"),
            "the retry must resume at the on-disk offset: {}",
            requests[1]
        );
    }

    /// (b) Retry exhaustion keeps a resumable `.partial` instead of deleting it.
    #[tokio::test]
    async fn retry_exhaustion_keeps_partial() {
        let tmp = TempDir::new().unwrap();
        const TOTAL: u64 = 1000;
        let server = TestServer::start(|_, head| {
            let offset = request_range_offset(head);
            response_206(offset, &chunk(50, b'A'), TOTAL)
        });

        let dm = DownloadManager::with_retry_policy(
            tmp.path().to_path_buf(),
            3,
            std::time::Duration::from_millis(1),
        );
        let err = dm
            .download(&server.url, tmp.path(), "model.gguf", None, "m", None)
            .await
            .unwrap_err();

        assert!(
            matches!(err, RegistryError::DownloadInterrupted { .. }),
            "expected a resumable interruption, got {err:?}"
        );
        assert!(
            !tmp.path().join("model.gguf").exists(),
            "a truncated stream must never become the final file"
        );
        let partial = tmp.path().join("model.gguf.partial");
        assert_eq!(
            std::fs::metadata(&partial).unwrap().len(),
            50 * 4,
            "one attempt plus three retries, 50 bytes each"
        );

        // The meta survives and reports the real size so the UI can offer Resume.
        let meta: PartialMeta = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("model.gguf.partial.meta")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta.model_id, "m");
        assert_eq!(meta.filename, "model.gguf");
        assert_eq!(meta.total_bytes, Some(TOTAL));
    }

    /// (c) A corrupt/0-byte meta is repaired and the resume continues from disk.
    #[tokio::test]
    async fn corrupt_meta_still_resumes_from_disk_length() {
        let tmp = TempDir::new().unwrap();
        const TOTAL: u64 = 1000;
        std::fs::write(tmp.path().join("model.gguf.partial"), chunk(50, b'A')).unwrap();
        std::fs::write(tmp.path().join("model.gguf.partial.meta"), b"").unwrap();

        let server = TestServer::start(|_, head| {
            let offset = request_range_offset(head);
            response_206(offset, &chunk((TOTAL - offset) as usize, b'B'), TOTAL)
        });

        let dm = DownloadManager::with_retry_policy(
            tmp.path().to_path_buf(),
            5,
            std::time::Duration::from_millis(1),
        );
        let path = dm
            .download(&server.url, tmp.path(), "model.gguf", None, "m", None)
            .await
            .unwrap();

        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), TOTAL as usize);
        assert_eq!(&data[..50], &chunk(50, b'A')[..], "existing bytes kept");
        assert_eq!(&data[50..], &chunk(950, b'B')[..]);
        assert!(
            server.requests()[0]
                .to_lowercase()
                .contains("range: bytes=50-"),
            "resume must start from the on-disk length"
        );
        assert!(!tmp.path().join("model.gguf.partial.meta").exists());
    }

    /// (c') A failed download rewrites an unreadable meta atomically.
    #[tokio::test]
    async fn corrupt_meta_is_rewritten_atomically() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("model.gguf.partial"), chunk(30, b'A')).unwrap();
        std::fs::write(tmp.path().join("model.gguf.partial.meta"), b"{not json").unwrap();

        let total = 1000u64;
        let server = TestServer::start(move |_, head| {
            let offset = request_range_offset(head);
            response_206(offset, &chunk(30, b'A'), total)
        });

        let dm = DownloadManager::with_retry_policy(
            tmp.path().to_path_buf(),
            1,
            std::time::Duration::from_millis(1),
        );
        let _ = dm
            .download(&server.url, tmp.path(), "model.gguf", None, "m", None)
            .await;

        let meta_path = tmp.path().join("model.gguf.partial.meta");
        let meta: PartialMeta =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(meta.model_id, "m");
        assert_eq!(meta.filename, "model.gguf");
        assert_eq!(meta.url, server.url);
        assert_eq!(meta.total_bytes, Some(total));
        assert!(
            !tmp.path().join("model.gguf.partial.meta.tmp").exists(),
            "the atomic write must not leave its tmp file behind"
        );
    }
}
