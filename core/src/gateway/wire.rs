use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::gateway::identity_binding::IdentityBinding;

pub const WIRE_VERSION: u32 = 1;
pub const MAX_WIRE_FRAME_SIZE: usize = 65536; // 64 KiB
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_PENDING_FORWARDS: usize = 64;
pub const MAX_PENDING_BYTES: usize = 1024 * 1024; // 1 MiB

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("Zero-length frame received")]
    ZeroLength,
    #[error("Frame length {0} exceeds limit of {MAX_WIRE_FRAME_SIZE} bytes")]
    FrameTooLarge(usize),
    #[error("Incomplete frame read (expected {expected} bytes, got {got})")]
    Incomplete { expected: usize, got: usize },
    #[error("Read timed out")]
    Timeout,
    #[error("IO error: {0}")]
    Io(String),
    #[error("Serialization/Deserialization error: {0}")]
    Codec(String),
    #[error("Unsupported wire protocol version: {0}")]
    UnsupportedVersion(u32),
    #[error("Loop detected: frame has already crossed an internet bridge")]
    LoopDetected,
    #[error("Forwarding hop budget exhausted")]
    BudgetExhausted,
    #[error("Gateway queue overload")]
    QueueFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    UnknownRoute,
    Overload,
    Duplicate,
    InvalidPayload,
    LoopDetected,
    BudgetExhausted,
    Unauthorized,
}

/// Initial handshake frame establishing gateway identity and authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayHello {
    pub protocol_version: u32,
    pub binding: IdentityBinding,
}

/// Mesh packet forwarded across internet gateway connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayForward {
    pub wire_version: u32,
    pub transfer_id: u64,
    pub destination_short_id_hex: String,
    pub remaining_hops: u8,
    pub bridge_crossings: u8,
    pub mesh_packet: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayMessage {
    Hello(GatewayHello),
    Forward(GatewayForward),
    Accepted {
        transfer_id: u64,
    },
    Rejected {
        transfer_id: u64,
        reason: RejectReason,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
}

impl GatewayMessage {
    /// Encode message to byte vector.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        serde_json::to_vec(self).map_err(|e| WireError::Codec(format!("Encoding error: {e}")))
    }

    /// Decode message from byte slice with strict bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.is_empty() {
            return Err(WireError::ZeroLength);
        }
        if bytes.len() > MAX_WIRE_FRAME_SIZE {
            return Err(WireError::FrameTooLarge(bytes.len()));
        }

        serde_json::from_slice(bytes).map_err(|e| WireError::Codec(format!("Decoding error: {e}")))
    }

    /// Write length-prefixed frame to async stream.
    pub async fn write_framed<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
    ) -> Result<(), WireError> {
        let payload = self.encode()?;
        let len =
            u32::try_from(payload.len()).map_err(|_| WireError::FrameTooLarge(payload.len()))?;

        if payload.len() > MAX_WIRE_FRAME_SIZE {
            return Err(WireError::FrameTooLarge(payload.len()));
        }

        writer
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| WireError::Io(e.to_string()))?;
        writer
            .write_all(&payload)
            .await
            .map_err(|e| WireError::Io(e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| WireError::Io(e.to_string()))?;

        Ok(())
    }

    /// Read length-prefixed frame from async stream with deadline and bounds.
    pub async fn read_framed<R: AsyncRead + Unpin>(
        reader: &mut R,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        tokio::select! {
            biased;
            res = Self::read_framed_inner(reader) => res,
            _ = tokio::time::sleep(timeout) => Err(WireError::Timeout),
        }
    }

    async fn read_framed_inner<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let mut len_buf = [0u8; 4];
        reader
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| WireError::Io(e.to_string()))?;

        let frame_len = u32::from_be_bytes(len_buf) as usize;
        if frame_len == 0 {
            return Err(WireError::ZeroLength);
        }
        if frame_len > MAX_WIRE_FRAME_SIZE {
            return Err(WireError::FrameTooLarge(frame_len));
        }

        let mut buf = vec![0u8; frame_len];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| WireError::Io(e.to_string()))?;

        Self::decode(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn test_framed_message_roundtrip() {
        let (mut client, mut server) = duplex(1024);

        let msg = GatewayMessage::Accepted { transfer_id: 42 };

        tokio::spawn(async move {
            msg.write_framed(&mut client).await.unwrap();
        });

        let received = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(received, GatewayMessage::Accepted { transfer_id: 42 });
    }

    #[tokio::test]
    async fn test_concatenated_frames_handled_correctly() {
        let (mut client, mut server) = duplex(1024);

        let msg1 = GatewayMessage::Ping { nonce: 101 };
        let msg2 = GatewayMessage::Accepted { transfer_id: 202 };

        tokio::spawn(async move {
            msg1.write_framed(&mut client).await.unwrap();
            msg2.write_framed(&mut client).await.unwrap();
        });

        let r1 = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        let r2 = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(r1, GatewayMessage::Ping { nonce: 101 });
        assert_eq!(r2, GatewayMessage::Accepted { transfer_id: 202 });
    }

    #[tokio::test]
    async fn test_oversized_length_prefix_rejected() {
        let (mut client, mut server) = duplex(1024);

        tokio::spawn(async move {
            // Write length exceeding MAX_WIRE_FRAME_SIZE
            let huge_len = (MAX_WIRE_FRAME_SIZE + 1) as u32;
            client.write_all(&huge_len.to_be_bytes()).await.unwrap();
        });

        let err = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(err, WireError::FrameTooLarge(MAX_WIRE_FRAME_SIZE + 1));
    }

    #[tokio::test]
    async fn test_zero_length_frame_rejected() {
        let (mut client, mut server) = duplex(1024);

        tokio::spawn(async move {
            let zero_len = 0u32;
            client.write_all(&zero_len.to_be_bytes()).await.unwrap();
        });

        let err = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(err, WireError::ZeroLength);
    }

    #[tokio::test]
    async fn test_read_timeout() {
        let (_client, mut server) = duplex(1024);

        let err = GatewayMessage::read_framed(&mut server, Duration::from_millis(50))
            .await
            .unwrap_err();

        assert_eq!(err, WireError::Timeout);
    }

    #[tokio::test]
    async fn test_malformed_json_body_rejected() {
        let (mut client, mut server) = duplex(1024);

        tokio::spawn(async move {
            let garbage = b"{not:valid:json}";
            let len = garbage.len() as u32;
            client.write_all(&len.to_be_bytes()).await.unwrap();
            client.write_all(garbage).await.unwrap();
        });

        let err = GatewayMessage::read_framed(&mut server, Duration::from_secs(1))
            .await
            .unwrap_err();

        assert!(matches!(err, WireError::Codec(_)));
    }
}
