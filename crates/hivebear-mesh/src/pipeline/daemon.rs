//! Mesh worker daemon: listens for inference requests and serves them
//! using a caller-provided `MeshInferenceHandler` with a Karma priority queue.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;
use tokio::sync::Notify;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::discovery::server::CoordinationServerClient;
use crate::karma::KarmaTier;
use crate::peer::NodeId;
use crate::protocol::{MeshInferenceHandler, MeshPipelineHandler};
use crate::transport::protocol::MeshMessage;
use crate::transport::MeshTransport;

struct QueuedInference {
    priority: u8,
    seq: u64,
    peer_id: NodeId,
    session_id: Uuid,
    model_id: String,
    messages_json: String,
    max_tokens: u32,
    temperature: f32,
}

impl PartialEq for QueuedInference {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for QueuedInference {}

impl PartialOrd for QueuedInference {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedInference {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first; FIFO (lower seq) breaks ties within the same tier
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// A background daemon that listens on the mesh transport for incoming
/// `InferenceRequest` messages, prioritizes them by Karma tier
/// (`VIP_ALPHA > CONTRIBUTOR > COMMUNITY`), runs inference via the supplied
/// handler, and streams tokens back to the requesting peer.
///
/// Optionally also handles pipeline-parallel messages (`AssignLayers`,
/// `ActivationTensor`) when a `MeshPipelineHandler` is provided.
pub struct MeshWorkerDaemon<H: MeshInferenceHandler> {
    handler: Arc<H>,
    transport: Arc<dyn MeshTransport>,
    pipeline_handler: Option<Arc<dyn MeshPipelineHandler>>,
    coordinator: Option<Arc<CoordinationServerClient>>,
    tier_cache: Arc<DashMap<String, (KarmaTier, Instant)>>,
}

impl<H: MeshInferenceHandler + 'static> MeshWorkerDaemon<H> {
    pub fn new(handler: Arc<H>, transport: Arc<dyn MeshTransport>) -> Self {
        Self {
            handler,
            transport,
            pipeline_handler: None,
            coordinator: None,
            tier_cache: Arc::new(DashMap::new()),
        }
    }

    /// Create a daemon with pipeline-parallel support.
    pub fn with_pipeline(
        handler: Arc<H>,
        transport: Arc<dyn MeshTransport>,
        pipeline_handler: Arc<dyn MeshPipelineHandler>,
    ) -> Self {
        Self {
            handler,
            transport,
            pipeline_handler: Some(pipeline_handler),
            coordinator: None,
            tier_cache: Arc::new(DashMap::new()),
        }
    }

    /// Attach a coordination server URL so the worker can prioritize incoming
    /// requests by the requester's Karma tier (`VIP_ALPHA > CONTRIBUTOR > COMMUNITY`).
    pub fn with_coordinator(mut self, coordinator_url: impl Into<String>) -> Self {
        let url = coordinator_url.into();
        if !url.trim().is_empty() {
            self.coordinator = Some(Arc::new(CoordinationServerClient::new(url)));
        }
        self
    }

    async fn resolve_peer_tier(&self, peer_id: &NodeId) -> KarmaTier {
        let Some(ref coord) = self.coordinator else {
            return KarmaTier::Community;
        };
        let key = peer_id.to_hex();
        if let Some(entry) = self.tier_cache.get(&key) {
            if entry.1.elapsed() < Duration::from_secs(60) {
                return entry.0;
            }
        }
        let tier =
            match tokio::time::timeout(Duration::from_millis(800), coord.get_karma_balance(&key))
                .await
            {
                Ok(Ok(balance)) => KarmaTier::from_balance(balance),
                _ => KarmaTier::Community,
            };
        self.tier_cache.insert(key, (tier, Instant::now()));
        tier
    }

    /// Run the daemon loop. This will block (async) until the transport
    /// is shut down or an unrecoverable error occurs.
    pub async fn run(&self) {
        info!("MeshWorkerDaemon: listening for inference requests (Karma priority queue active)");

        let queue: Arc<Mutex<BinaryHeap<QueuedInference>>> =
            Arc::new(Mutex::new(BinaryHeap::new()));
        let notify = Arc::new(Notify::new());
        let seq_counter = AtomicU64::new(0);

        // Priority worker dispatcher: always serves highest-Karma tier next
        let q_worker = queue.clone();
        let n_worker = notify.clone();
        let h_worker = self.handler.clone();
        let t_worker = self.transport.clone();
        tokio::spawn(async move {
            loop {
                let next_job = {
                    let mut heap = q_worker.lock().unwrap_or_else(|e| e.into_inner());
                    heap.pop()
                };
                match next_job {
                    Some(job) => {
                        Self::handle_request(
                            h_worker.clone(),
                            t_worker.clone(),
                            job.peer_id,
                            job.session_id,
                            job.model_id,
                            job.messages_json,
                            job.max_tokens,
                            job.temperature,
                        )
                        .await;
                    }
                    None => {
                        n_worker.notified().await;
                    }
                }
            }
        });

        loop {
            let (peer_id, msg) = match self.transport.recv().await {
                Ok(pair) => pair,
                Err(e) => {
                    error!("MeshWorkerDaemon: transport recv error: {e}");
                    break;
                }
            };

            match msg {
                MeshMessage::InferenceRequest {
                    session_id,
                    model_id,
                    messages_json,
                    max_tokens,
                    temperature,
                    top_p: _,
                } => {
                    // Fast probe requests bypass the inference queue for instant discovery
                    if model_id == "__probe__" || max_tokens == 0 {
                        let handler = self.handler.clone();
                        let transport = self.transport.clone();
                        tokio::spawn(async move {
                            Self::handle_request(
                                handler,
                                transport,
                                peer_id,
                                session_id,
                                model_id,
                                messages_json,
                                max_tokens,
                                temperature,
                            )
                            .await;
                        });
                        continue;
                    }

                    // DoS protection: cap prompt payload size and requested max_tokens
                    const MAX_MESSAGES_JSON_BYTES: usize = 256 * 1024;
                    const MAX_ALLOWED_REMOTE_TOKENS: u32 = 4_096;
                    const MAX_QUEUED_REQUESTS: usize = 32;

                    if messages_json.len() > MAX_MESSAGES_JSON_BYTES {
                        let _ = self
                            .transport
                            .send(
                                &peer_id,
                                MeshMessage::InferenceComplete {
                                    session_id,
                                    full_text: String::new(),
                                    tokens_generated: 0,
                                    error: Some(
                                        "Prompt payload exceeds maximum allowed size (256 KiB)"
                                            .into(),
                                    ),
                                },
                            )
                            .await;
                        continue;
                    }
                    let max_tokens = max_tokens.clamp(1, MAX_ALLOWED_REMOTE_TOKENS);

                    let tier = self.resolve_peer_tier(&peer_id).await;
                    let seq = seq_counter.fetch_add(1, AtomicOrdering::Relaxed);
                    let accepted = {
                        let mut heap = queue.lock().unwrap_or_else(|e| e.into_inner());
                        if heap.len() >= MAX_QUEUED_REQUESTS {
                            false
                        } else {
                            heap.push(QueuedInference {
                                priority: tier.priority_rank(),
                                seq,
                                peer_id: peer_id.clone(),
                                session_id,
                                model_id: model_id.clone(),
                                messages_json,
                                max_tokens,
                                temperature,
                            });
                            true
                        }
                    };
                    if !accepted {
                        let _ = self
                            .transport
                            .send(
                                &peer_id,
                                MeshMessage::InferenceComplete {
                                    session_id,
                                    full_text: String::new(),
                                    tokens_generated: 0,
                                    error: Some(
                                        "Worker queue is currently full (max 32 requests); please retry"
                                            .into(),
                                    ),
                                },
                            )
                            .await;
                        continue;
                    }
                    info!(
                        "MeshWorkerDaemon: queued inference request from {peer_id} \
                         (tier={}, session {session_id}, model '{model_id}')",
                        tier.as_str()
                    );
                    notify.notify_one();
                }
                MeshMessage::Ping { timestamp_ms } => {
                    let _ = self
                        .transport
                        .send(&peer_id, MeshMessage::Pong { timestamp_ms })
                        .await;
                }
                MeshMessage::ReleaseSession { session_id } => {
                    info!("MeshWorkerDaemon: session {session_id} released by {peer_id}");
                    // Also unload pipeline layers if any were loaded
                    if let Some(ref ph) = self.pipeline_handler {
                        let ph = ph.clone();
                        tokio::spawn(async move {
                            let _ = ph.unload_layers().await;
                        });
                    }
                }
                MeshMessage::AssignLayers {
                    session_id,
                    model_id: _,
                    layer_range,
                    total_layers,
                    model_source,
                    next_peer: _,
                    initiator_peer: _,
                } => {
                    if let Some(ref ph) = self.pipeline_handler {
                        info!(
                            "MeshWorkerDaemon: layer assignment from {peer_id}: \
                             layers {}..{} of {total_layers} (session {session_id})",
                            layer_range.start, layer_range.end
                        );
                        let ph = ph.clone();
                        let transport = self.transport.clone();
                        tokio::spawn(async move {
                            let result = ph
                                .load_layers(&model_source, layer_range, total_layers)
                                .await;
                            let ack = MeshMessage::AssignLayersAck {
                                session_id,
                                ready: result.is_ok(),
                                error: result.err(),
                            };
                            let _ = transport.send(&peer_id, ack).await;
                        });
                    } else {
                        warn!(
                            "MeshWorkerDaemon: received AssignLayers but no pipeline handler set"
                        );
                    }
                }
                MeshMessage::ActivationTensor {
                    session_id,
                    token_position,
                    data,
                    shape,
                    dtype,
                } => {
                    if let Some(ref ph) = self.pipeline_handler {
                        let ph = ph.clone();
                        let transport = self.transport.clone();
                        let dtype_u8 = match dtype {
                            crate::transport::protocol::TensorDtype::F32 => 0u8,
                            crate::transport::protocol::TensorDtype::F16 => 1u8,
                            crate::transport::protocol::TensorDtype::BF16 => 2u8,
                        };
                        tokio::spawn(async move {
                            match ph
                                .forward_layers(
                                    data.to_vec(),
                                    shape.clone(),
                                    dtype_u8,
                                    token_position as usize,
                                )
                                .await
                            {
                                Ok((out_data, out_shape, out_dtype)) => {
                                    let out_tensor_dtype = match out_dtype {
                                        1 => crate::transport::protocol::TensorDtype::F16,
                                        2 => crate::transport::protocol::TensorDtype::BF16,
                                        _ => crate::transport::protocol::TensorDtype::F32,
                                    };
                                    // Send output back to the initiator (peer who sent it)
                                    let msg = MeshMessage::ActivationTensor {
                                        session_id,
                                        token_position,
                                        data: bytes::Bytes::from(out_data),
                                        shape: out_shape,
                                        dtype: out_tensor_dtype,
                                    };
                                    let _ = transport.send(&peer_id, msg).await;
                                }
                                Err(e) => {
                                    error!("MeshWorkerDaemon: forward_layers failed: {e}");
                                }
                            }
                        });
                    }
                }
                _ => {
                    // Ignore messages we don't handle (Hello, etc.)
                }
            }
        }
    }

    /// Serve a single inference request: call the handler, stream tokens
    /// back through the transport, and send an `InferenceComplete` at the end.
    #[allow(clippy::too_many_arguments)]
    async fn handle_request(
        handler: Arc<H>,
        transport: Arc<dyn MeshTransport>,
        peer_id: NodeId,
        session_id: Uuid,
        model_id: String,
        messages_json: String,
        max_tokens: u32,
        temperature: f32,
    ) {
        let stream_result = handler
            .handle_inference(&model_id, &messages_json, max_tokens, temperature)
            .await;

        let mut token_stream = match stream_result {
            Ok(s) => s,
            Err(err) => {
                warn!("MeshWorkerDaemon: handler error for session {session_id}: {err}");
                let _ = transport
                    .send(
                        &peer_id,
                        MeshMessage::InferenceComplete {
                            session_id,
                            full_text: String::new(),
                            tokens_generated: 0,
                            error: Some(err),
                        },
                    )
                    .await;
                return;
            }
        };

        let mut full_text = String::new();
        let mut token_count: u32 = 0;

        while let Some(result) = token_stream.next().await {
            match result {
                Ok(text) => {
                    full_text.push_str(&text);
                    token_count += 1;

                    let msg = MeshMessage::InferenceToken {
                        session_id,
                        text,
                        token_id: token_count,
                        is_done: false,
                    };
                    if let Err(e) = transport.send(&peer_id, msg).await {
                        warn!("MeshWorkerDaemon: failed to send token to {peer_id}: {e}");
                        return;
                    }
                }
                Err(err) => {
                    let _ = transport
                        .send(
                            &peer_id,
                            MeshMessage::InferenceComplete {
                                session_id,
                                full_text,
                                tokens_generated: token_count,
                                error: Some(err),
                            },
                        )
                        .await;
                    return;
                }
            }
        }

        // Send the final "done" token so the initiator knows streaming is over
        let _ = transport
            .send(
                &peer_id,
                MeshMessage::InferenceToken {
                    session_id,
                    text: String::new(),
                    token_id: token_count,
                    is_done: true,
                },
            )
            .await;

        // Then a complete message for good measure
        let _ = transport
            .send(
                &peer_id,
                MeshMessage::InferenceComplete {
                    session_id,
                    full_text,
                    tokens_generated: token_count,
                    error: None,
                },
            )
            .await;

        info!("MeshWorkerDaemon: completed session {session_id} — {token_count} tokens");
    }
}
