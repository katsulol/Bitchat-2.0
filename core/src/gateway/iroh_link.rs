use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};

pub const GATEWAY_ALPN: &[u8] = b"bitchat2/gateway/1";

/// Build an Iroh endpoint using the pinned 1.4.1 API.
pub async fn create_endpoint(
    secret_key: Option<SecretKey>,
    relay_mode: RelayMode,
) -> Result<Endpoint, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = Endpoint::builder(presets::N0);
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    let endpoint = builder
        .relay_mode(relay_mode)
        .alpns(vec![GATEWAY_ALPN.to_vec()])
        .bind()
        .await?;
    Ok(endpoint)
}

/// Connect to a remote endpoint address using the gateway ALPN.
pub async fn dial_gateway(
    endpoint: &Endpoint,
    remote_addr: EndpointAddr,
) -> Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
    dial_with_alpn(endpoint, remote_addr, GATEWAY_ALPN).await
}

/// Connect to a remote endpoint address with explicit ALPN.
pub async fn dial_with_alpn(
    endpoint: &Endpoint,
    remote_addr: EndpointAddr,
    alpn: &[u8],
) -> Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
    let conn = endpoint.connect(remote_addr, alpn).await?;
    Ok(conn)
}

/// Accept next incoming connection from the endpoint.
pub async fn accept_gateway_conn(
    endpoint: &Endpoint,
) -> Result<(EndpointId, Connection), Box<dyn std::error::Error + Send + Sync>> {
    let incoming = endpoint
        .accept()
        .await
        .ok_or("Endpoint closed while waiting for incoming connection")?;
    let conn = incoming.await?;
    let remote_node_id = conn.remote_id();
    Ok((remote_node_id, conn))
}

/// Send a length-prefixed frame over a QUIC send stream.
pub async fn send_length_prefixed(
    send: &mut SendStream,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let len = u32::try_from(data.len()).map_err(|_| "Frame size exceeds 32-bit limit")?;
    let len_bytes = len.to_be_bytes();
    send.write_all(&len_bytes).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Read a length-prefixed frame from a QUIC recv stream with size bounds.
pub async fn recv_length_prefixed(
    recv: &mut RecvStream,
    max_len: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let frame_len = u32::from_be_bytes(len_buf) as usize;
    if frame_len == 0 {
        return Err("Zero-length frame received".into());
    }
    if frame_len > max_len {
        return Err(format!("Frame len {frame_len} exceeds maximum allowed {max_len}").into());
    }

    let mut buf = vec![0u8; frame_len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_iroh_isolated_connection_and_payload_exchange() {
        // Build Endpoint A (Acceptor)
        let ep_a = create_endpoint(None, RelayMode::Disabled)
            .await
            .expect("Failed to create endpoint A");
        let addr_a = ep_a.addr();

        // Build Endpoint B (Initiator)
        let ep_b = create_endpoint(None, RelayMode::Disabled)
            .await
            .expect("Failed to create endpoint B");

        // Spawn acceptor task
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        // Spawn acceptor task
        let accept_handle = tokio::spawn(async move {
            let (remote_id, conn) = accept_gateway_conn(&ep_a)
                .await
                .expect("Failed to accept connection on A");
            let (mut send, mut recv) = conn.accept_bi().await.expect("Failed to accept bi on A");
            let payload = recv_length_prefixed(&mut recv, 65536)
                .await
                .expect("Failed to read payload on A");

            // Echo with prefix "ACK:"
            let mut reply = b"ACK:".to_vec();
            reply.extend_from_slice(&payload);
            send_length_prefixed(&mut send, &reply)
                .await
                .expect("Failed to send ack on A");
            send.finish().expect("Failed to finish send on A");

            // Keep connection open until B finishes reading
            let _ = done_rx.await;

            (remote_id, payload, ep_a)
        });

        // Initiator B connects to A
        let conn_b = dial_gateway(&ep_b, addr_a)
            .await
            .expect("Failed to dial A from B");
        let (mut send_b, mut recv_b) = conn_b.open_bi().await.expect("Failed to open bi on B");

        let test_message = b"hello from node B over isolated iroh QUIC stream";
        send_length_prefixed(&mut send_b, test_message)
            .await
            .expect("Failed to write on B");
        send_b.finish().expect("Failed to finish send on B");

        let response = recv_length_prefixed(&mut recv_b, 65536)
            .await
            .expect("Failed to read response on B");
        assert_eq!(response, [b"ACK:".as_slice(), test_message].concat());

        // Notify A that we read the response
        let _ = done_tx.send(());

        let (accepted_remote_id, accepted_payload, ep_a) =
            accept_handle.await.expect("Accept task panicked");
        assert_eq!(accepted_remote_id, ep_b.id());
        assert_eq!(accepted_payload, test_message);

        // Clean shutdown
        ep_a.close().await;
        ep_b.close().await;
    }

    #[tokio::test]
    async fn test_iroh_rejects_wrong_alpn() {
        // Acceptor with GATEWAY_ALPN
        let ep_a = create_endpoint(None, RelayMode::Disabled)
            .await
            .expect("Failed to create endpoint A");
        let addr_a = ep_a.addr();

        // Initiator with wrong ALPN
        let ep_b = Endpoint::builder(presets::N0)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![b"wrong/unsupported/alpn".to_vec()])
            .bind()
            .await
            .expect("Failed to bind B");

        // QUIC handshake with wrong ALPN must either fail immediately or timeout
        let dial_res = tokio::time::timeout(
            Duration::from_millis(1500),
            dial_with_alpn(&ep_b, addr_a, b"wrong/unsupported/alpn"),
        )
        .await;

        match dial_res {
            Ok(Err(_)) => {} // Expected connection failure
            Err(_) => {}     // Expected timeout rejection
            Ok(Ok(_)) => panic!("Connection with wrong ALPN must not succeed"),
        }

        ep_a.close().await;
        ep_b.close().await;
    }

    #[tokio::test]
    async fn test_iroh_clean_shutdown() {
        let ep = create_endpoint(None, RelayMode::Disabled)
            .await
            .expect("Failed to create endpoint");
        ep.close().await;
    }
}
