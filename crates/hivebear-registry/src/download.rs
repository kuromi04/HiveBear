use crate::error::{RegistryError, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Progress information for download callbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_sec: f64,
}

/// Metadata stored alongside partial downloads for resume support.
#[derive(Debug, Serialize, Deserialize)]
pub struct PartialMeta {
    url: String,
    total_bytes: Option<u64>,
    sha256_expected: Option<String>,
    model_id: String,
    filename: String,
    started_at: DateTime<Utc>,
}

/// Manages model file downloads with resume support.
pub struct DownloadManager {
    client: reqwest::Client,
    models_dir: PathBuf,
}

impl DownloadManager {
    pub fn new(models_dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("hivebear/0.2.14")
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(5)
            .build()
            .unwrap_or_default();

        Self { client, models_dir }
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

    /// Download a file with resume support and optional SHA-256 verification.
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

        // Check for existing partial download
        let mut bytes_downloaded: u64;

        if partial_path.exists() && meta_path.exists() {
            // Read partial metadata
            if let Ok(meta_contents) = tokio::fs::read_to_string(&meta_path).await {
                if let Ok(meta) = serde_json::from_str::<PartialMeta>(&meta_contents) {
                    if meta.url == url {
                        let bytes = tokio::fs::metadata(&partial_path)
                            .await
                            .map(|m| m.len())
                            .unwrap_or(0);
                        tracing::info!("Resuming download from {} bytes", bytes);
                    } else {
                        // URL changed, start fresh
                        tokio::fs::remove_file(&partial_path).await.ok();
                        tokio::fs::remove_file(&meta_path).await.ok();
                    }
                }
            }
        }

        // Save partial metadata
        let meta = PartialMeta {
            url: url.to_string(),
            total_bytes: None,
            sha256_expected: expected_sha256.map(String::from),
            model_id: model_id.to_string(),
            filename: filename.to_string(),
            started_at: Utc::now(),
        };
        let meta_json = serde_json::to_string(&meta)?;
        tokio::fs::write(&meta_path, &meta_json).await?;

        let mut max_stream_retries = 5;
        let mut last_emit = std::time::Instant::now();

        loop {
            // Sync bytes_downloaded with actual file length on disk before requesting
            if partial_path.exists() {
                bytes_downloaded = tokio::fs::metadata(&partial_path)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
            } else {
                bytes_downloaded = 0;
            }

            let response = match self.send_with_retry(url, bytes_downloaded, 3).await {
                Ok(resp) => resp,
                Err(e) => {
                    // If range not satisfiable (HTTP 416) or corrupted offset, clear partial file & retry from 0
                    if bytes_downloaded > 0 && e.to_string().contains("416") {
                        tracing::warn!("HTTP 416 Range Not Satisfiable, resetting partial file");
                        tokio::fs::remove_file(&partial_path).await.ok();
                        bytes_downloaded = 0;
                        self.send_with_retry(url, 0, 3).await?
                    } else {
                        tokio::fs::remove_file(&meta_path).await.ok();
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

            let total_bytes = if is_partial {
                response.content_length().map(|cl| cl + bytes_downloaded)
            } else {
                response.content_length()
            };

            // Open file for append if 206 Partial Content, or write/truncate if 200 OK
            let mut file = if is_partial && bytes_downloaded > 0 {
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&partial_path)
                    .await?
            } else {
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&partial_path)
                    .await?
            };

            let mut stream = response.bytes_stream();
            let start_time = std::time::Instant::now();
            let bytes_at_start = bytes_downloaded;
            let mut stream_error = false;

            while let Some(chunk_res) = stream.next().await {
                match chunk_res {
                    Ok(chunk) => {
                        file.write_all(&chunk).await?;
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

            file.flush().await?;
            drop(file);

            if !stream_error {
                break; // Download completed fully!
            }

            max_stream_retries -= 1;
            if max_stream_retries == 0 {
                return Err(RegistryError::DownloadError(
                    "Max stream retries exceeded".to_string(),
                ));
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Final verification: SHA-256 on completed partial file if expected
        if let Some(expected) = expected_sha256 {
            tracing::info!("Verifying SHA-256 checksum for {}", filename);
            let mut file = tokio::fs::File::open(&partial_path).await?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 1024 * 1024]; // 1MB buffer

            loop {
                let n = file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }

            let actual = format!("{:x}", hasher.finalize());
            if actual.to_lowercase() != expected.to_lowercase() {
                tokio::fs::remove_file(&partial_path).await.ok();
                tokio::fs::remove_file(&meta_path).await.ok();
                return Err(RegistryError::IntegrityError {
                    path: partial_path,
                    expected: expected.to_string(),
                    actual,
                });
            }
        }

        // Move partial to final
        tokio::fs::rename(&partial_path, &final_path).await?;
        tokio::fs::remove_file(&meta_path).await.ok();

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

#[cfg(test)]
mod tests {
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
}
