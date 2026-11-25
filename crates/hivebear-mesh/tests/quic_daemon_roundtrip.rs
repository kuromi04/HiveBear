use std::sync::Arc;
use std::time::{Duration, Instant};

use hivebear_mesh::config::MeshSecurityMode;
use hivebear_mesh::pipeline::daemon::MeshWorkerDaemon;
use hivebear_mesh::protocol::MeshInferenceHandler;
use hivebear_mesh::transport::protocol::MeshMessage;
use hivebear_mesh::transport::quic::QuicTransport;
use hivebear_mesh::transport::MeshTransport;
use hivebear_mesh::NodeIdentity;

struct DummyHandler;

#[async_trait::async_trait]
impl MeshInferenceHandler for DummyHandler {
    async fn handle_inference(
        &self,
        model_id: &str,
        _messages_json: &str,
        max_tokens: u32,
        _temperature: f32,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<String, String>> + Send>>, String>
    {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let reply = if model_id == "__probe__" || max_tokens == 0 {
            "dummy-model".to_string()
        } else {
            "OK from worker".to_string()
        };
        let _ = tx.send(Ok(reply)).await;
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

#[tokio::test]
async fn local_quic_daemon_roundtrip() {
    let server_id = NodeIdentity::generate();
    let server_transport: Arc<dyn MeshTransport> = Arc::new(QuicTransport::new(
        server_id.node_id.clone(),
        MeshSecurityMode::default(),
        None,
    ));
    let listen_addr: std::net::SocketAddr = "127.0.0.1:17878".parse().unwrap();
    server_transport.listen(listen_addr).await.unwrap();

    let daemon = MeshWorkerDaemon::new(Arc::new(DummyHandler), server_transport.clone());
    tokio::spawn(async move {
        daemon.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client_id = NodeIdentity::generate();
    let client_transport =
        QuicTransport::new(client_id.node_id.clone(), MeshSecurityMode::default(), None);

    let t0 = Instant::now();
    let peer_id = client_transport.connect(listen_addr).await.unwrap();
    println!("LOCAL Connected in {:?}", t0.elapsed());

    client_transport
        .send(&peer_id, MeshMessage::Ping { timestamp_ms: 99 })
        .await
        .unwrap();
    let (_, pong) = tokio::time::timeout(Duration::from_secs(3), client_transport.recv())
        .await
        .expect("pong timeout")
        .unwrap();
    println!("LOCAL Pong in {:?}: {:?}", t0.elapsed(), pong);

    client_transport
        .send(
            &peer_id,
            MeshMessage::InferenceRequest {
                session_id: uuid::Uuid::new_v4(),
                model_id: "dummy".into(),
                messages_json: r#"[{"role":"user","content":"hi"}]"#.into(),
                max_tokens: 16,
                temperature: 0.2,
                top_p: 0.9,
            },
        )
        .await
        .unwrap();
    let (_, tok) = tokio::time::timeout(Duration::from_secs(3), client_transport.recv())
        .await
        .expect("chat timeout")
        .unwrap();
    println!("LOCAL Chat in {:?}: {:?}", t0.elapsed(), tok);
}
