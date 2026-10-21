// CRYPTO-REVIEW:
// What it does: Multi-hop End-to-End Encryption (E2E) over mesh relays using Noise XX
// mutual handshake and stateless ChaChaPoly AEAD transport.
// Primitives / crates: `snow`, `ed25519-dalek`, `blake3`.
// Reviewer checklist:
// 1. Relays strictly forward envelopes as opaque byte buffers without access to plaintext or keys.
// 2. Handshake envelopes carry mutual Ed25519 identity signatures over ephemeral Noise static keys.
// 3. Remote PeerId is cryptographically verified before any session enters Transport mode.
// 4. Chat messages carry 8-byte explicit nonces and 16-byte Poly1305 authentication tags.
// 5. Tampered ciphertext in transit is rejected by Poly1305 MAC verification before delivery.

use std::collections::HashMap;

use crate::crypto::Session;
use crate::envelope::{Flags, MsgType};
use crate::error::{Error, Result};
use crate::identity::{PeerId, ShortId};
use crate::mesh::relay::{InboundMessage, MeshNode};
use crate::mesh::MsgId;
use crate::transport::Transport;

// ponytail: end-to-end encrypted session coordinator over multi-hop mesh node.

/// Authenticated and decrypted payload received from a remote mesh peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedMessage {
    pub origin_sender: ShortId,
    pub remote_peer_id: PeerId,
    pub plaintext: Vec<u8>,
    pub hop_count: u8,
}

/// End-to-end cryptographic session manager for mesh nodes.
pub struct PeerSessionManager {
    sessions: HashMap<ShortId, Session>,
}

impl Default for PeerSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    pub fn get_session(&self, peer: &ShortId) -> Option<&Session> {
        self.sessions.get(peer)
    }

    pub fn get_session_mut(&mut self, peer: &ShortId) -> Option<&mut Session> {
        self.sessions.get_mut(peer)
    }

    /// Initiate a Noise XX handshake with a remote peer across the mesh.
    pub async fn initiate_handshake<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        peer: ShortId,
    ) -> Result<MsgId> {
        let mut session = Session::initiator(node.identity())?;
        let hs_payload = session.write_handshake_vec()?;
        self.sessions.insert(peer, session);

        node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), hs_payload)
            .await
    }

    /// Process an inbound Handshake envelope and advance the mutual Noise XX state machine.
    #[allow(clippy::map_entry)]
    pub async fn handle_handshake<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        msg: &InboundMessage,
    ) -> Result<bool> {
        if msg.msg_type != MsgType::Handshake {
            return Ok(false);
        }

        let peer = msg.origin_sender;

        if !self.sessions.contains_key(&peer) {
            // New incoming handshake from peer: act as responder
            let mut session = Session::responder(node.identity())?;
            session.read_handshake(&msg.payload)?;
            let reply_payload = session.write_handshake_vec()?;

            node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), reply_payload)
                .await?;

            self.sessions.insert(peer, session);
            return Ok(true);
        }

        // Advance existing handshake
        let reply_payload = {
            let session = match self.sessions.get_mut(&peer) {
                Some(s) => s,
                None => return Ok(false),
            };

            session.read_handshake(&msg.payload)?;

            if !session.is_handshake_finished() {
                Some(session.write_handshake_vec()?)
            } else {
                None
            }
        };

        if let Some(payload) = reply_payload {
            node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), payload)
                .await?;
        }

        Ok(true)
    }

    /// Encrypt plaintext with peer's established session and transmit across the mesh.
    pub async fn send_encrypted<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        recipient: ShortId,
        plaintext: &[u8],
    ) -> Result<MsgId> {
        let session = self.sessions.get_mut(&recipient).ok_or_else(|| {
            Error::Crypto(format!("No established session with peer {recipient}"))
        })?;

        if !session.is_handshake_finished() {
            return Err(Error::Crypto(format!(
                "Session with peer {recipient} has not completed handshake"
            )));
        }

        let ciphertext = session.encrypt(plaintext)?;
        node.send_to_with_type(recipient, MsgType::Chat, Flags::default(), ciphertext)
            .await
    }

    /// Decrypt an inbound Chat envelope using the sender's active session.
    pub fn decrypt_inbound(&mut self, msg: &InboundMessage) -> Result<DecryptedMessage> {
        if msg.msg_type != MsgType::Chat {
            return Err(Error::Crypto(format!(
                "Cannot decrypt non-Chat message type {:?}",
                msg.msg_type
            )));
        }

        let peer = msg.origin_sender;
        let session = self
            .sessions
            .get_mut(&peer)
            .ok_or_else(|| Error::Crypto(format!("No active session for sender {peer}")))?;

        let remote_peer_id = session
            .peer_id()
            .ok_or_else(|| Error::Crypto("Authenticated peer identity missing".into()))?;

        let plaintext = session.decrypt(&msg.payload)?;

        Ok(DecryptedMessage {
            origin_sender: peer,
            remote_peer_id,
            plaintext,
            hop_count: msg.hop_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::mesh::policy::NaiveFlood;
    use crate::transport::sim::SimNetwork;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn test_relayed_e2e_encryption_over_line_topology() {
        tokio::time::pause();

        let net = SimNetwork::new(707);
        let id_a = Identity::generate();
        let id_b = Identity::generate(); // Intermediate relay node
        let id_c = Identity::generate();

        let peer_a = id_a.peer_id();
        let peer_c = id_c.peer_id();

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();
        let s_c = id_c.short_id();

        // Line topology: A <-> B <-> C (A cannot reach C directly)
        net.set_bidirectional_link(s_a, s_b, Duration::ZERO, 0.0);
        net.set_bidirectional_link(s_b, s_c, Duration::ZERO, 0.0);

        let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood));
        let mut node_b = MeshNode::new(id_b, net.create_node(s_b), Box::new(NaiveFlood));
        let mut node_c = MeshNode::new(id_c, net.create_node(s_c), Box::new(NaiveFlood));

        let mut mgr_a = PeerSessionManager::new();
        let mut mgr_c = PeerSessionManager::new();

        // 1. A initiates handshake with C across mesh
        mgr_a.initiate_handshake(&mut node_a, s_c).await.unwrap();

        // B relays msg 1: A -> B -> C
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}
        // C receives msg 1 from B, responds with msg 2: C -> B -> A
        while let Ok(true) = node_c.poll_step(Instant::now()).await {}
        let c_in1 = node_c.recv_inbound().unwrap();
        mgr_c.handle_handshake(&mut node_c, &c_in1).await.unwrap();

        // B relays msg 2: C -> B -> A
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}
        // A receives msg 2 from B (draining reflected msg 1 first), responds with msg 3: A -> B -> C
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        let a_in1 = node_a.recv_inbound().unwrap();
        mgr_a.handle_handshake(&mut node_a, &a_in1).await.unwrap();

        // B relays msg 3: A -> B -> C
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}
        // C receives msg 3 from B: handshake completes!
        while let Ok(true) = node_c.poll_step(Instant::now()).await {}
        let c_in2 = node_c.recv_inbound().unwrap();
        mgr_c.handle_handshake(&mut node_c, &c_in2).await.unwrap();

        // Verify mutual authentication and PeerId binding
        assert!(mgr_a.get_session(&s_c).unwrap().is_handshake_finished());
        assert!(mgr_c.get_session(&s_a).unwrap().is_handshake_finished());
        assert_eq!(mgr_a.get_session(&s_c).unwrap().peer_id(), Some(peer_c));
        assert_eq!(mgr_c.get_session(&s_a).unwrap().peer_id(), Some(peer_a));

        // 2. A sends confidential message to C
        let secret_text = b"Classified mesh communication: operation successful".to_vec();
        mgr_a
            .send_encrypted(&mut node_a, s_c, &secret_text)
            .await
            .unwrap();

        // 3. Inspect Relay B's state: B never sees plaintext!
        // Step B forward: B forwards the encrypted Chat frame
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}
        // B should have 0 deliveries because message was addressed to C, not B
        assert_eq!(node_b.stats().deliveries, 0);
        assert_eq!(node_b.inbound_count(), 0);

        // 4. C receives and decrypts message
        while let Ok(true) = node_c.poll_step(Instant::now()).await {}
        // Total deliveries at C: Handshake Msg 1, Handshake Msg 3, and Chat Msg = 3
        assert_eq!(node_c.stats().deliveries, 3);
        let c_chat = node_c.recv_inbound().unwrap();

        // Plaintext is NOT visible on wire
        assert_ne!(c_chat.payload, secret_text);

        // Decrypt at C
        let decrypted = mgr_c.decrypt_inbound(&c_chat).unwrap();
        assert_eq!(decrypted.origin_sender, s_a);
        assert_eq!(decrypted.remote_peer_id, peer_a);
        assert_eq!(decrypted.plaintext, secret_text);
        assert_eq!(decrypted.hop_count, 1); // Relayed through B

        // 5. Tampering verification: tampering in transit fails AEAD MAC at receiver
        let mut tampered_chat = c_chat.clone();
        if let Some(byte) = tampered_chat.payload.last_mut() {
            *byte ^= 0xFF; // Invert last MAC byte
        }
        let decrypt_err = mgr_c.decrypt_inbound(&tampered_chat);
        assert!(
            decrypt_err.is_err(),
            "Tampered payload must fail Poly1305 MAC verification"
        );
    }
}
