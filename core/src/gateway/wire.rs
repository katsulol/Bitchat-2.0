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

/// Recipient entry in a gateway reachability snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRecipient {
    pub short_id_hex: String,
    pub public_key_hex: Option<String>,
}

pub const ROUTE_SNAPSHOT_DOMAIN: &[u8] = b"bitchat2/gateway-route-snapshot/v1";
pub const MAX_SNAPSHOT_RECIPIENTS: usize = 128;
pub const MAX_LEASE_SECONDS: u32 = 3600;

/// Full snapshot of verified local reachability announced across an Iroh link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSnapshot {
    pub protocol_version: u32,
    pub session_epoch: u64,
    pub sequence_number: u64,
    pub lease_seconds: u32,
    pub recipients: Vec<RouteRecipient>,
    #[serde(default)]
    pub signature: Vec<u8>,
}

impl RouteSnapshot {
    pub fn canonical_sign_bytes(&self, gateway_pubkey: &[u8; 32]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 + self.recipients.len() * 40);
        buf.extend_from_slice(ROUTE_SNAPSHOT_DOMAIN);
        buf.extend_from_slice(&self.protocol_version.to_be_bytes());
        buf.extend_from_slice(gateway_pubkey);
        buf.extend_from_slice(&self.session_epoch.to_be_bytes());
        buf.extend_from_slice(&self.sequence_number.to_be_bytes());
        buf.extend_from_slice(&self.lease_seconds.to_be_bytes());
        buf.extend_from_slice(&(self.recipients.len() as u32).to_be_bytes());
        for r in &self.recipients {
            buf.extend_from_slice(r.short_id_hex.as_bytes());
            if let Some(pk) = &r.public_key_hex {
                buf.extend_from_slice(pk.as_bytes());
            }
        }
        buf
    }

    pub fn sign(&mut self, identity: &crate::identity::Identity) {
        let sign_bytes = self.canonical_sign_bytes(identity.peer_id().as_bytes());
        let sig = identity.sign(&sign_bytes);
        self.signature = sig.to_bytes().to_vec();
    }

    pub fn verify_author(
        &self,
        gateway_peer_id: &crate::identity::PeerId,
    ) -> crate::error::Result<()> {
        if self.signature.len() != 64 {
            return Err(crate::error::Error::Crypto(format!(
                "Invalid route snapshot signature length: expected 64, got {}",
                self.signature.len()
            )));
        }
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(&self.signature);
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        let sign_bytes = self.canonical_sign_bytes(gateway_peer_id.as_bytes());
        crate::identity::Identity::verify(gateway_peer_id, &sign_bytes, &sig)
    }

    pub fn validate_bounds(&self) -> Result<(), WireError> {
        if self.recipients.len() > MAX_SNAPSHOT_RECIPIENTS {
            return Err(WireError::Codec(format!(
                "Snapshot recipients {} exceeds maximum {}",
                self.recipients.len(),
                MAX_SNAPSHOT_RECIPIENTS
            )));
        }
        if self.lease_seconds > MAX_LEASE_SECONDS {
            return Err(WireError::Codec(format!(
                "Snapshot lease {}s exceeds maximum {}s",
                self.lease_seconds, MAX_LEASE_SECONDS
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayMessage {
    Hello(GatewayHello),
    Forward(GatewayForward),
    RouteSync(RouteSnapshot),
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

    #[tokio::test]
    async fn test_route_sync_frame_roundtrip() {
        let id = crate::identity::Identity::generate();
        let mut snapshot = RouteSnapshot {
            protocol_version: WIRE_VERSION,
            session_epoch: 123456789,
            sequence_number: 42,
            lease_seconds: 300,
            recipients: vec![
                RouteRecipient {
                    short_id_hex: "0102030405060708".to_string(),
                    public_key_hex: Some("00".repeat(32)),
                },
                RouteRecipient {
                    short_id_hex: "A1B2C3D4E5F60718".to_string(),
                    public_key_hex: None,
                },
            ],
            signature: Vec::new(),
        };
        snapshot.sign(&id);
        assert!(snapshot.verify_author(&id.peer_id()).is_ok());

        // Tampered signature fails
        let mut tampered = snapshot.clone();
        tampered.signature[0] ^= 0xFF;
        assert!(tampered.verify_author(&id.peer_id()).is_err());

        // Wrong author peer_id fails
        let wrong_id = crate::identity::Identity::generate();
        assert!(snapshot.verify_author(&wrong_id.peer_id()).is_err());

        // Wire roundtrip
        let msg = GatewayMessage::RouteSync(snapshot.clone());
        let encoded = msg.encode().unwrap();
        let decoded = GatewayMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, GatewayMessage::RouteSync(snapshot));
    }

    #[test]
    fn test_route_snapshot_bounds_validation() {
        // Oversized recipients list
        let mut bad_recipients = Vec::new();
        for i in 0..=MAX_SNAPSHOT_RECIPIENTS {
            bad_recipients.push(RouteRecipient {
                short_id_hex: format!("{i:016x}"),
                public_key_hex: None,
            });
        }
        let snap_huge = RouteSnapshot {
            protocol_version: WIRE_VERSION,
            session_epoch: 1,
            sequence_number: 1,
            lease_seconds: 60,
            recipients: bad_recipients,
            signature: Vec::new(),
        };
        assert!(snap_huge.validate_bounds().is_err());

        // Excessive lease
        let snap_huge_lease = RouteSnapshot {
            protocol_version: WIRE_VERSION,
            session_epoch: 1,
            sequence_number: 1,
            lease_seconds: MAX_LEASE_SECONDS + 1,
            recipients: vec![],
            signature: Vec::new(),
        };
        assert!(snap_huge_lease.validate_bounds().is_err());
    }
}
