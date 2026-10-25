// CRYPTO-REVIEW:
// What it does: Multi-hop End-to-End Encryption (E2E) over mesh relays using Noise XX
// mutual handshake and stateless ChaChaPoly AEAD transport with hardened session framing.
// Primitives / crates: `snow`, `ed25519-dalek`, `blake3`.
// Reviewer checklist:
// 1. Relays strictly forward envelopes as opaque byte buffers without access to plaintext or keys.
// 2. Handshake envelopes carry typed versioned HandshakeFrame wrappers (version, step, attempt_id, session_context).
// 3. Remote PeerId is cryptographically verified before any session enters Transport mode.
// 4. Stale attempts, corrupted steps, and unauthenticated identity mismatches are rejected immediately.
// 5. Simultaneous initiator collisions are deterministically resolved via ShortId tie-break.
// 6. Consumed Noise transitions are never re-executed; flights are cached and retransmitted safely.
// 7. Chat messages carry 8-byte explicit nonces and 16-byte Poly1305 authentication tags.
// 8. Tampered ciphertext in transit is rejected by Poly1305 MAC verification before delivery.

use std::collections::HashMap;

use rand::Rng;

use crate::crypto::{HandshakeFrame, Session};
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

/// Metadata and cached flights for a managed peer session.
pub struct ManagedSession {
    pub session: Session,
    pub attempt_id: u64,
    pub session_context: [u8; 8],
    pub cached_flight: Option<Vec<u8>>,
    pub cached_flight3: Option<Vec<u8>>,
    pub verified_peer_id: Option<PeerId>,
    pub intended_peer: ShortId,
}

/// End-to-end cryptographic session manager for mesh nodes.
pub struct PeerSessionManager {
    sessions: HashMap<ShortId, ManagedSession>,
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
        self.sessions.get(peer).map(|m| &m.session)
    }

    pub fn get_session_mut(&mut self, peer: &ShortId) -> Option<&mut Session> {
        self.sessions.get_mut(peer).map(|m| &mut m.session)
    }

    pub fn has_session(&self, peer: &ShortId) -> bool {
        self.sessions.contains_key(peer)
    }

    pub fn is_session_ready(&self, peer: &ShortId) -> bool {
        self.sessions
            .get(peer)
            .map(|m| m.session.is_handshake_finished())
            .unwrap_or(false)
    }

    pub fn verified_peer_id(&self, peer: &ShortId) -> Option<PeerId> {
        self.sessions
            .get(peer)
            .and_then(|m| m.verified_peer_id.or_else(|| m.session.peer_id()))
    }

    /// Initiate a Noise XX handshake with a remote peer across the mesh.
    pub async fn initiate_handshake<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        peer: ShortId,
    ) -> Result<MsgId> {
        let mut session = Session::initiator(node.identity())?;
        let noise_payload = session.write_handshake_vec()?;

        let mut rng = rand::thread_rng();
        let attempt_id: u64 = rng.gen::<u64>() | 1;

        let mut session_context = [0u8; 8];
        let h = blake3::hash(&node.identity().peer_id().0);
        session_context.copy_from_slice(&h.as_bytes()[0..8]);

        let frame = HandshakeFrame::new(1, attempt_id, session_context, noise_payload);
        let encoded = frame.encode();

        self.sessions.insert(
            peer,
            ManagedSession {
                session,
                attempt_id,
                session_context,
                cached_flight: Some(encoded.clone()),
                cached_flight3: None,
                verified_peer_id: None,
                intended_peer: peer,
            },
        );

        node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), encoded)
            .await
    }

    /// Process an inbound Handshake envelope and advance the mutual Noise XX state machine.
    pub async fn handle_handshake<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        msg: &InboundMessage,
    ) -> Result<bool> {
        if msg.msg_type != MsgType::Handshake {
            return Ok(false);
        }

        let peer = msg.origin_sender;
        let frame = match HandshakeFrame::decode(&msg.payload) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Rejecting malformed handshake frame from {peer}: {e}");
                return Err(e);
            }
        };

        if let Some(managed) = self.sessions.get_mut(&peer) {
            // Case 1: Session is already in Transport mode (handshake complete locally)
            if managed.session.is_handshake_finished() {
                if frame.step == 1 {
                    if frame.attempt_id != managed.attempt_id {
                        // Remote initiated a fresh session attempt
                        self.sessions.remove(&peer);
                        // Fall through to responder initiation below
                    } else {
                        // Duplicate Flight 1: resend cached flight if available
                        if let Some(flight) = &managed.cached_flight {
                            node.send_to_with_type(
                                peer,
                                MsgType::Handshake,
                                Flags::default(),
                                flight.clone(),
                            )
                            .await?;
                        }
                        return Ok(true);
                    }
                } else if frame.step == 2 {
                    // Duplicate Flight 2 arriving at Initiator: resend Flight 3 so remote can complete
                    if let Some(flight3) = &managed.cached_flight3 {
                        node.send_to_with_type(
                            peer,
                            MsgType::Handshake,
                            Flags::default(),
                            flight3.clone(),
                        )
                        .await?;
                    }
                    return Ok(true);
                } else {
                    return Ok(true); // Duplicate Flight 3 at Responder: safely ignore
                }
            } else if managed.session.is_failed() {
                if frame.step == 1 {
                    self.sessions.remove(&peer);
                } else {
                    return Ok(false);
                }
            } else {
                // Session is in Handshaking mode
                if frame.step == 1 {
                    // Simultaneous initiation collision!
                    if managed.session.is_initiator() == Some(true) {
                        if peer < node.identity().short_id() {
                            // Remote has lower ShortId: local node yields as initiator and acts as responder
                            self.sessions.remove(&peer);
                            // Fall through to responder initiation below
                        } else {
                            // Local node has lower ShortId: local node is winning initiator.
                            return Ok(true);
                        }
                    } else {
                        // Responder re-receiving Flight 1: resend cached Flight 2 without re-running state
                        if let Some(flight2) = &managed.cached_flight {
                            node.send_to_with_type(
                                peer,
                                MsgType::Handshake,
                                Flags::default(),
                                flight2.clone(),
                            )
                            .await?;
                        }
                        return Ok(true);
                    }
                } else if frame.step == 2 {
                    if managed.session.is_initiator() != Some(true) {
                        return Err(Error::Crypto(
                            "Responder received unexpected Flight 2".into(),
                        ));
                    }

                    if frame.attempt_id != managed.attempt_id {
                        tracing::warn!(
                            "Dropping stale handshake flight 2: attempt {} != {}",
                            frame.attempt_id,
                            managed.attempt_id
                        );
                        return Ok(false);
                    }

                    // Check if already processed (duplicate flight 2)
                    if let Some((write_count, _)) = managed.session.handshake_counts() {
                        if write_count >= 2 {
                            if let Some(flight3) = &managed.cached_flight3 {
                                node.send_to_with_type(
                                    peer,
                                    MsgType::Handshake,
                                    Flags::default(),
                                    flight3.clone(),
                                )
                                .await?;
                            }
                            return Ok(true);
                        }
                    }

                    if let Err(e) = managed.session.read_handshake(&frame.noise_payload) {
                        tracing::warn!("Handshake read failed from {peer}: {e}; resetting session");
                        self.sessions.remove(&peer);
                        return Err(e);
                    }

                    // Strict remote identity verification
                    let remote_peer_id = managed.session.peer_id().ok_or_else(|| {
                        Error::Crypto("Handshake flight 2 missing remote peer identity".into())
                    })?;
                    if remote_peer_id.short_id() != peer {
                        self.sessions.remove(&peer);
                        return Err(Error::Crypto(format!(
                            "Remote peer identity {remote_peer_id:?} (ShortId {}) does not match intended recipient {peer}",
                            remote_peer_id.short_id()
                        )));
                    }
                    managed.verified_peer_id = Some(remote_peer_id);

                    // Write Flight 3
                    let flight3_payload = managed.session.write_handshake_vec()?;
                    let frame3 = HandshakeFrame::new(
                        3,
                        frame.attempt_id,
                        frame.session_context,
                        flight3_payload,
                    );
                    let encoded3 = frame3.encode();
                    managed.cached_flight = Some(encoded3.clone());
                    managed.cached_flight3 = Some(encoded3.clone());

                    node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), encoded3)
                        .await?;
                    return Ok(true);
                } else if frame.step == 3 {
                    if managed.session.is_initiator() != Some(false) {
                        return Err(Error::Crypto(
                            "Initiator received unexpected Flight 3".into(),
                        ));
                    }

                    if frame.attempt_id != managed.attempt_id {
                        tracing::warn!(
                            "Dropping stale handshake flight 3: attempt {} != {}",
                            frame.attempt_id,
                            managed.attempt_id
                        );
                        return Ok(false);
                    }

                    if let Err(e) = managed.session.read_handshake(&frame.noise_payload) {
                        tracing::warn!("Handshake read failed from {peer}: {e}; resetting session");
                        self.sessions.remove(&peer);
                        return Err(e);
                    }

                    let remote_peer_id = managed.session.peer_id().ok_or_else(|| {
                        Error::Crypto("Handshake flight 3 missing remote peer identity".into())
                    })?;
                    if remote_peer_id.short_id() != peer {
                        self.sessions.remove(&peer);
                        return Err(Error::Crypto(format!(
                            "Remote peer identity {remote_peer_id:?} does not match intended sender {peer}"
                        )));
                    }
                    managed.verified_peer_id = Some(remote_peer_id);
                    return Ok(true);
                }
            }
        }

        // New incoming handshake from peer (local node acts as responder)
        if frame.step == 1 {
            let mut session = Session::responder(node.identity())?;
            session.read_handshake(&frame.noise_payload)?;
            let reply_payload = session.write_handshake_vec()?;

            let frame2 =
                HandshakeFrame::new(2, frame.attempt_id, frame.session_context, reply_payload);
            let encoded2 = frame2.encode();

            node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), encoded2.clone())
                .await?;

            self.sessions.insert(
                peer,
                ManagedSession {
                    session,
                    attempt_id: frame.attempt_id,
                    session_context: frame.session_context,
                    cached_flight: Some(encoded2),
                    cached_flight3: None,
                    verified_peer_id: None,
                    intended_peer: peer,
                },
            );
            return Ok(true);
        }

        // Orphaned flight 2 or 3 received without prior handshake state: drop
        Ok(false)
    }

    /// Resend cached handshake flight if remote side missed it or needs recovery.
    pub async fn resend_handshake_flight<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        peer: ShortId,
    ) -> Result<bool> {
        if let Some(managed) = self.sessions.get(&peer) {
            if let Some(flight) = managed
                .cached_flight3
                .as_ref()
                .or(managed.cached_flight.as_ref())
            {
                node.send_to_with_type(peer, MsgType::Handshake, Flags::default(), flight.clone())
                    .await?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Encrypt plaintext with peer's established session and transmit across the mesh.
    pub async fn send_encrypted<T: Transport>(
        &mut self,
        node: &mut MeshNode<T>,
        recipient: ShortId,
        plaintext: &[u8],
    ) -> Result<MsgId> {
        let managed = self.sessions.get_mut(&recipient).ok_or_else(|| {
            Error::Crypto(format!("No established session with peer {recipient}"))
        })?;

        if !managed.session.is_handshake_finished() {
            return Err(Error::Crypto(format!(
                "Session with peer {recipient} has not completed handshake"
            )));
        }

        let ciphertext = managed.session.encrypt(plaintext)?;
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
        let managed = self
            .sessions
            .get_mut(&peer)
            .ok_or_else(|| Error::Crypto(format!("No active session for sender {peer}")))?;

        if !managed.session.is_handshake_finished() {
            return Err(Error::Crypto(format!(
                "Session for sender {peer} is unready; handshake incomplete"
            )));
        }

        let remote_peer_id = managed
            .verified_peer_id
            .or_else(|| managed.session.peer_id())
            .ok_or_else(|| Error::Crypto("Authenticated peer identity missing".into()))?;

        let plaintext = managed.session.decrypt(&msg.payload)?;

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

    #[tokio::test]
    async fn test_simultaneous_handshake_collision() {
        tokio::time::pause();

        let net = SimNetwork::new(808);
        let id_a = Identity::generate();
        let id_b = Identity::generate();

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();

        // Direct bidirectional link: A <-> B
        net.set_bidirectional_link(s_a, s_b, Duration::from_millis(5), 0.0);

        let t_a = net.create_node(s_a);
        let t_b = net.create_node(s_b);

        let mut node_a = MeshNode::new(id_a, t_a, Box::new(NaiveFlood));
        let mut node_b = MeshNode::new(id_b, t_b, Box::new(NaiveFlood));

        let mut mgr_a = PeerSessionManager::new();
        let mut mgr_b = PeerSessionManager::new();

        // Both nodes simultaneously initiate handshake
        mgr_a.initiate_handshake(&mut node_a, s_b).await.unwrap();
        mgr_b.initiate_handshake(&mut node_b, s_a).await.unwrap();

        // Step simulation forward until all handshake packets are exchanged
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            let now = Instant::now();
            while let Ok(true) = node_a.poll_step(now).await {}
            while let Ok(true) = node_b.poll_step(now).await {}

            while let Some(msg) = node_a.recv_inbound() {
                if msg.msg_type == MsgType::Handshake {
                    let _ = mgr_a.handle_handshake(&mut node_a, &msg).await;
                }
            }
            while let Some(msg) = node_b.recv_inbound() {
                if msg.msg_type == MsgType::Handshake {
                    let _ = mgr_b.handle_handshake(&mut node_b, &msg).await;
                }
            }
        }

        // Both nodes must have completed the handshake into transport mode
        assert!(
            mgr_a.is_session_ready(&s_b),
            "Node A must reach ready session"
        );
        assert!(
            mgr_b.is_session_ready(&s_a),
            "Node B must reach ready session"
        );
    }

    #[tokio::test]
    async fn test_handshake_loss_and_flight_caching() {
        tokio::time::pause();

        let net = SimNetwork::new(909);
        let id_a = Identity::generate();
        let id_b = Identity::generate();

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();

        net.set_bidirectional_link(s_a, s_b, Duration::from_millis(5), 0.0);

        let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood));
        let mut node_b = MeshNode::new(id_b, net.create_node(s_b), Box::new(NaiveFlood));

        let mut mgr_a = PeerSessionManager::new();
        let mut mgr_b = PeerSessionManager::new();

        // Step 1: A initiates handshake
        mgr_a.initiate_handshake(&mut node_a, s_b).await.unwrap();

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        // B receives Flight 1 and responds with Flight 2
        let b_msg1 = node_b.recv_inbound().unwrap();
        mgr_b.handle_handshake(&mut node_b, &b_msg1).await.unwrap();

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        // Simulate LOSS of Flight 2! (We drop the message before node_a receives it)
        while let Some(_lost_flight2) = node_a.recv_inbound() {
            // Intentionally dropped
        }
        assert!(!mgr_a.is_session_ready(&s_b));

        // Node A retransmits Flight 1 using cached flight
        mgr_a
            .resend_handshake_flight(&mut node_a, s_b)
            .await
            .unwrap();

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        // Node B hears duplicate Flight 1: must resend cached Flight 2 WITHOUT failing Noise state!
        let b_msg1_dup = node_b.recv_inbound().unwrap();
        let res = mgr_b.handle_handshake(&mut node_b, &b_msg1_dup).await;
        assert!(
            res.is_ok(),
            "Duplicate Flight 1 must resend cached Flight 2 without failure"
        );

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}

        // Node A now receives Flight 2 and responds with Flight 3
        let a_msg2 = node_a.recv_inbound().unwrap();
        mgr_a.handle_handshake(&mut node_a, &a_msg2).await.unwrap();
        assert!(
            mgr_a.is_session_ready(&s_b),
            "A must enter Transport mode after Flight 3"
        );

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        // Simulate LOSS of Flight 3! (Drain and discard before mgr_b can process it)
        while let Some(_lost_flight3) = node_b.recv_inbound() {
            // Intentionally dropped
        }
        assert!(
            !mgr_b.is_session_ready(&s_a),
            "B has not processed Flight 3 yet"
        );

        // A sends confidential message, but B is not ready yet
        let chat_id = mgr_a
            .send_encrypted(&mut node_a, s_b, b"early data")
            .await
            .unwrap();
        assert_ne!(chat_id, [0u8; 8]);

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        let b_early_chat = node_b.recv_inbound().unwrap();
        assert_eq!(b_early_chat.msg_type, MsgType::Chat);
        let dec_err = mgr_b.decrypt_inbound(&b_early_chat);
        assert!(
            dec_err.is_err(),
            "B cannot decrypt chat before handshake completes"
        );

        // A resends cached Flight 3 to unblock B
        mgr_a
            .resend_handshake_flight(&mut node_a, s_b)
            .await
            .unwrap();

        tokio::time::advance(Duration::from_millis(10)).await;
        while let Ok(true) = node_a.poll_step(Instant::now()).await {}
        while let Ok(true) = node_b.poll_step(Instant::now()).await {}

        let b_msg3 = node_b.recv_inbound().unwrap();
        mgr_b.handle_handshake(&mut node_b, &b_msg3).await.unwrap();
        assert!(
            mgr_b.is_session_ready(&s_a),
            "B must now be ready in Transport mode"
        );

        // Now B can decrypt chat traffic from A
        let dec = mgr_b.decrypt_inbound(&b_early_chat).unwrap();
        assert_eq!(dec.plaintext, b"early data");
    }

    #[tokio::test]
    async fn test_stale_handshake_attempt_rejected() {
        tokio::time::pause();

        let net = SimNetwork::new(1010);
        let id_a = Identity::generate();
        let id_b = Identity::generate();

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();

        let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood));
        let mut mgr_a = PeerSessionManager::new();

        mgr_a.initiate_handshake(&mut node_a, s_b).await.unwrap();

        // Inject stale Flight 2 with wrong attempt_id
        let stale_frame = HandshakeFrame::new(2, 999999, [0u8; 8], vec![1, 2, 3]);
        let stale_inbound = InboundMessage {
            origin_sender: s_b,
            recipient: s_a,
            msg_type: MsgType::Handshake,
            flags: Flags::default(),
            msg_id: [1u8; 8],
            hop_count: 0,
            payload: stale_frame.encode(),
        };

        let handled = mgr_a
            .handle_handshake(&mut node_a, &stale_inbound)
            .await
            .unwrap();
        assert!(!handled, "Stale handshake attempt must be dropped");
        assert!(
            mgr_a.has_session(&s_b),
            "Current handshake session must NOT be erased by stale attempt"
        );
    }

    #[tokio::test]
    async fn test_wrong_remote_identity_rejected() {
        tokio::time::pause();

        let net = SimNetwork::new(1111);
        let id_a = Identity::generate();
        let id_b = Identity::generate(); // Intended peer
        let id_eve = Identity::generate(); // Impersonator / wrong peer

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();
        let s_eve = id_eve.short_id();

        let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood));
        let node_eve = MeshNode::new(id_eve, net.create_node(s_eve), Box::new(NaiveFlood));

        let mut mgr_a = PeerSessionManager::new();

        // Alice initiates handshake intended for Bob (s_b)
        mgr_a.initiate_handshake(&mut node_a, s_b).await.unwrap();

        // Eve crafts Flight 2 signed by Eve's key, pretending to be s_b
        let eve_session = Session::responder(node_eve.identity()).unwrap();
        let mut eve_managed = ManagedSession {
            session: eve_session,
            attempt_id: 1,
            session_context: [0u8; 8],
            cached_flight: None,
            cached_flight3: None,
            verified_peer_id: None,
            intended_peer: s_a,
        };
        // Eve reads Alice's flight 1
        let a_flight1_payload = {
            let m = mgr_a.sessions.get(&s_b).unwrap();
            let f = HandshakeFrame::decode(m.cached_flight.as_ref().unwrap()).unwrap();
            f.noise_payload
        };
        eve_managed
            .session
            .read_handshake(&a_flight1_payload)
            .unwrap();
        let eve_flight2_noise = eve_managed.session.write_handshake_vec().unwrap();

        let eve_frame2 = HandshakeFrame::new(
            2,
            mgr_a.sessions.get(&s_b).unwrap().attempt_id,
            [0u8; 8],
            eve_flight2_noise,
        );

        let spoofed_msg = InboundMessage {
            origin_sender: s_b, // Eve spoofed origin_sender to s_b
            recipient: s_a,
            msg_type: MsgType::Handshake,
            flags: Flags::default(),
            msg_id: [2u8; 8],
            hop_count: 0,
            payload: eve_frame2.encode(),
        };

        // Alice processes the spoofed message
        let res = mgr_a.handle_handshake(&mut node_a, &spoofed_msg).await;
        assert!(
            res.is_err(),
            "Handshake must reject remote identity that does not match intended recipient"
        );
        assert!(
            !mgr_a.has_session(&s_b),
            "Session must be purged after identity verification failure"
        );
    }

    #[tokio::test]
    async fn test_malformed_handshake_rejected() {
        tokio::time::pause();

        let net = SimNetwork::new(1212);
        let id_a = Identity::generate();
        let s_a = id_a.short_id();
        let s_b = ShortId([99u8; 8]);

        let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood));
        let mut mgr_a = PeerSessionManager::new();

        let malformed_msg = InboundMessage {
            origin_sender: s_b,
            recipient: s_a,
            msg_type: MsgType::Handshake,
            flags: Flags::default(),
            msg_id: [3u8; 8],
            hop_count: 0,
            payload: vec![0xFF, 0x00], // Truncated / invalid version
        };

        let res = mgr_a.handle_handshake(&mut node_a, &malformed_msg).await;
        assert!(res.is_err(), "Malformed handshake frame must be rejected");
    }
}
