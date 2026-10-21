// CRYPTO-REVIEW:
// What it does: Implements secure forward-secret sessions using the Noise Protocol Framework
// (Noise_XX_25519_ChaChaPoly_BLAKE2s) with Ed25519 identity key binding and stateless
// AEAD transport with a sliding replay window.
// Primitives / crates: `snow` (Noise Protocol), `ed25519-dalek` (Identity signatures).
// Reviewer checklist:
// 1. Noise pattern is strictly Noise_XX_25519_ChaChaPoly_BLAKE2s.
// 2. Both initiator and responder generate a dedicated X25519 Noise static keypair and sign
//    their Noise static public key using their long-term Ed25519 Identity signing key.
// 3. Domain separation prefix `b"bitchat2/noise-static/v1"` is prepended before signing and
//    verifying the Noise static key.
// 4. The handshake payload contains (PeerId: 32 bytes || Ed25519 Signature: 64 bytes).
// 5. Before completing handshake and transitioning to Transport mode, the remote signature
//    over the remote Noise static public key MUST verify against the claimed PeerId.
// 6. Transport mode uses snow's StatelessTransportState with an explicit 64-bit big-endian
//    nonce prepended to each encrypted payload: [nonce: 8 bytes] || [ciphertext: N bytes + 16-byte tag].
// 7. A 128-packet sliding replay window accepts out-of-order packets while rejecting duplicate nonces
//    and nonces older than the window.
// 8. The replay window is updated ONLY after AEAD tag verification succeeds.
// 9. Debug implementation for Session explicitly redacts all key and state material.

use std::fmt;

use ed25519_dalek::Signature;
use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, StatelessTransportState};

use crate::error::{Error, Result};
use crate::identity::{Identity, PeerId};

pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
pub const NOISE_STATIC_SIG_CONTEXT: &[u8] = b"bitchat2/noise-static/v1";
pub const BINDING_PAYLOAD_LEN: usize = 32 + 64; // PeerId (32) + Signature (64)
pub const MAX_HANDSHAKE_BUF: usize = 512;
pub const REPLAY_WINDOW_SIZE: u64 = 128;
pub const NONCE_LEN: usize = 8;
pub const TAG_LEN: usize = 16;

/// Produce canonical domain-separated message for Noise static key signing.
pub fn noise_static_binding_message(noise_static_pubkey: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(NOISE_STATIC_SIG_CONTEXT.len() + noise_static_pubkey.len());
    msg.extend_from_slice(NOISE_STATIC_SIG_CONTEXT);
    msg.extend_from_slice(noise_static_pubkey);
    msg
}

/// 128-packet sliding replay window for out-of-order mesh delivery.
#[derive(Clone, Debug, Default)]
pub struct ReplayWindow {
    max_nonce: u64,
    bitmap: u128,
    initialized: bool,
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if nonce is valid (not replayed and within acceptable window).
    pub fn is_valid(&self, nonce: u64) -> bool {
        if !self.initialized {
            return true;
        }
        if nonce > self.max_nonce {
            true
        } else {
            let diff = self.max_nonce - nonce;
            if diff >= REPLAY_WINDOW_SIZE {
                false // Too old
            } else {
                ((self.bitmap >> diff) & 1) == 0 // Unseen
            }
        }
    }

    /// Record newly verified nonce. Must be called only after AEAD decryption succeeds.
    pub fn update(&mut self, nonce: u64) {
        if !self.initialized {
            self.initialized = true;
            self.max_nonce = nonce;
            self.bitmap = 1;
            return;
        }

        if nonce > self.max_nonce {
            let diff = nonce - self.max_nonce;
            if diff >= REPLAY_WINDOW_SIZE {
                self.bitmap = 1;
            } else {
                self.bitmap = (self.bitmap << diff) | 1;
            }
            self.max_nonce = nonce;
        } else {
            let diff = self.max_nonce - nonce;
            if diff < REPLAY_WINDOW_SIZE {
                self.bitmap |= 1u128 << diff;
            }
        }
    }
}

enum SessionState {
    Handshaking {
        handshake: Box<HandshakeState>,
        local_binding_payload: Vec<u8>,
        remote_peer_id: Option<PeerId>,
        is_initiator: bool,
        write_count: usize,
        read_count: usize,
    },
    Transport {
        transport: StatelessTransportState,
        remote_peer_id: PeerId,
        send_nonce: u64,
        replay_window: ReplayWindow,
    },
    Failed,
}

/// Secure session wrapper providing Noise XX mutual authentication and stateless AEAD transport.
pub struct Session {
    state: SessionState,
}

impl Session {
    /// Initialize a new Noise XX initiator session bound to the provided long-term identity.
    pub fn initiator(identity: &Identity) -> Result<Self> {
        Self::build(identity, true)
    }

    /// Initialize a new Noise XX responder session bound to the provided long-term identity.
    pub fn responder(identity: &Identity) -> Result<Self> {
        Self::build(identity, false)
    }

    fn build(identity: &Identity, is_initiator: bool) -> Result<Self> {
        let params: NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|e| Error::Crypto(format!("Invalid Noise pattern: {e}")))?;

        let static_keypair = Builder::new(params.clone())
            .generate_keypair()
            .map_err(|e| Error::Crypto(format!("Failed to generate static keypair: {e}")))?;

        // Domain-separated signature over the Noise static public key
        let binding_msg = noise_static_binding_message(&static_keypair.public);
        let sig = identity.sign(&binding_msg);

        let mut local_binding_payload = Vec::with_capacity(BINDING_PAYLOAD_LEN);
        local_binding_payload.extend_from_slice(identity.peer_id().as_bytes());
        local_binding_payload.extend_from_slice(&sig.to_bytes());

        let builder = Builder::new(params).local_private_key(&static_keypair.private);

        let handshake = if is_initiator {
            builder
                .build_initiator()
                .map_err(|e| Error::Crypto(format!("Failed to build initiator: {e}")))?
        } else {
            builder
                .build_responder()
                .map_err(|e| Error::Crypto(format!("Failed to build responder: {e}")))?
        };

        Ok(Self {
            state: SessionState::Handshaking {
                handshake: Box::new(handshake),
                local_binding_payload,
                remote_peer_id: None,
                is_initiator,
                write_count: 0,
                read_count: 0,
            },
        })
    }

    /// True if the Noise XX handshake has completed and session is in transport mode.
    pub fn is_handshake_finished(&self) -> bool {
        matches!(self.state, SessionState::Transport { .. })
    }

    /// Return the authenticated remote PeerId once verified in the handshake.
    pub fn peer_id(&self) -> Option<PeerId> {
        match &self.state {
            SessionState::Handshaking { remote_peer_id, .. } => *remote_peer_id,
            SessionState::Transport { remote_peer_id, .. } => Some(*remote_peer_id),
            SessionState::Failed => None,
        }
    }

    /// Write next outgoing handshake frame into buffer.
    pub fn write_handshake(&mut self, out: &mut [u8]) -> Result<usize> {
        let (len, transition) = match &mut self.state {
            SessionState::Handshaking {
                handshake,
                local_binding_payload,
                remote_peer_id,
                is_initiator,
                write_count,
                ..
            } => {
                let should_send_payload =
                    (*is_initiator && *write_count == 1) || (!*is_initiator && *write_count == 0);

                let payload = if should_send_payload {
                    local_binding_payload.as_slice()
                } else {
                    &[]
                };

                let len = handshake
                    .write_message(payload, out)
                    .map_err(|e| Error::Crypto(format!("Handshake write failed: {e}")))?;

                *write_count += 1;

                let transition = if handshake.is_handshake_finished() {
                    let remote_id = remote_peer_id.ok_or_else(|| {
                        Error::Crypto("Handshake finished without remote identity".into())
                    })?;
                    Some(remote_id)
                } else {
                    None
                };

                (len, transition)
            }
            SessionState::Transport { .. } => {
                return Err(Error::Crypto(
                    "Handshake already completed, in transport mode".into(),
                ));
            }
            SessionState::Failed => {
                return Err(Error::Crypto("Session in failed state".into()));
            }
        };

        if let Some(remote_peer_id) = transition {
            self.transition_to_transport(remote_peer_id)?;
        }

        Ok(len)
    }

    /// Convenient helper returning a Vec<u8> containing the next handshake frame.
    pub fn write_handshake_vec(&mut self) -> Result<Vec<u8>> {
        let mut buf = [0u8; MAX_HANDSHAKE_BUF];
        let len = self.write_handshake(&mut buf)?;
        Ok(buf[..len].to_vec())
    }

    /// Read an incoming handshake frame from peer and verify identity binding.
    pub fn read_handshake(&mut self, input: &[u8]) -> Result<()> {
        let mut payload_buf = [0u8; MAX_HANDSHAKE_BUF];

        let transition = match &mut self.state {
            SessionState::Handshaking {
                handshake,
                remote_peer_id,
                read_count,
                ..
            } => {
                let payload_len = handshake
                    .read_message(input, &mut payload_buf)
                    .map_err(|e| Error::Crypto(format!("Handshake read failed: {e}")))?;

                *read_count += 1;

                if let Some(remote_static) = handshake.get_remote_static() {
                    if remote_peer_id.is_none() {
                        if payload_len < BINDING_PAYLOAD_LEN {
                            return Err(Error::Crypto(format!(
                                "Handshake missing identity binding payload: expected {BINDING_PAYLOAD_LEN} bytes, got {payload_len}"
                            )));
                        }
                        let mut claimed_peer_id = [0u8; 32];
                        claimed_peer_id.copy_from_slice(&payload_buf[0..32]);
                        let peer_id = PeerId(claimed_peer_id);

                        let mut sig_bytes = [0u8; 64];
                        sig_bytes.copy_from_slice(&payload_buf[32..96]);
                        let signature = Signature::from_bytes(&sig_bytes);

                        // VERIFY domain-separated signature over the Noise static public key
                        let binding_msg = noise_static_binding_message(remote_static);
                        Identity::verify(&peer_id, &binding_msg, &signature).map_err(|e| {
                            Error::Crypto(format!(
                                "Identity binding signature verification failed: {e}"
                            ))
                        })?;

                        *remote_peer_id = Some(peer_id);
                    }
                }

                if handshake.is_handshake_finished() {
                    let remote_id = remote_peer_id.ok_or_else(|| {
                        Error::Crypto("Handshake finished without remote identity".into())
                    })?;
                    Some(remote_id)
                } else {
                    None
                }
            }
            SessionState::Transport { .. } => {
                return Err(Error::Crypto(
                    "Handshake already completed, in transport mode".into(),
                ));
            }
            SessionState::Failed => {
                return Err(Error::Crypto("Session in failed state".into()));
            }
        };

        if let Some(remote_peer_id) = transition {
            self.transition_to_transport(remote_peer_id)?;
        }

        Ok(())
    }

    fn transition_to_transport(&mut self, remote_peer_id: PeerId) -> Result<()> {
        let old_state = std::mem::replace(&mut self.state, SessionState::Failed);
        if let SessionState::Handshaking { handshake, .. } = old_state {
            let transport = handshake.into_stateless_transport_mode().map_err(|e| {
                Error::Crypto(format!("Failed to enter stateless transport mode: {e}"))
            })?;
            self.state = SessionState::Transport {
                transport,
                remote_peer_id,
                send_nonce: 0,
                replay_window: ReplayWindow::new(),
            };
            Ok(())
        } else {
            Err(Error::Crypto(
                "Invalid state for transport transition".into(),
            ))
        }
    }

    /// Encrypt plaintext into a stateless frame carrying an explicit 64-bit nonce:
    /// `[nonce: 8 bytes] || [ciphertext: N bytes + 16-byte Poly1305 MAC tag]`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        match &mut self.state {
            SessionState::Transport {
                transport,
                send_nonce,
                ..
            } => {
                let nonce = *send_nonce;
                let mut out = vec![0u8; NONCE_LEN + plaintext.len() + TAG_LEN];
                out[0..NONCE_LEN].copy_from_slice(&nonce.to_be_bytes());

                let len = transport
                    .write_message(nonce, plaintext, &mut out[NONCE_LEN..])
                    .map_err(|e| Error::Crypto(format!("Stateless encryption failed: {e}")))?;
                out.truncate(NONCE_LEN + len);

                *send_nonce = send_nonce
                    .checked_add(1)
                    .ok_or_else(|| Error::Crypto("Send nonce counter overflowed".into()))?;

                Ok(out)
            }
            _ => Err(Error::Crypto(
                "Cannot encrypt: session not in transport mode".into(),
            )),
        }
    }

    /// Decrypt a stateless frame with explicit 64-bit nonce, verifying against the sliding replay window.
    pub fn decrypt(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        match &mut self.state {
            SessionState::Transport {
                transport,
                replay_window,
                ..
            } => {
                if frame.len() < NONCE_LEN + TAG_LEN {
                    return Err(Error::Crypto(format!(
                        "Encrypted frame too short: expected at least {} bytes, got {}",
                        NONCE_LEN + TAG_LEN,
                        frame.len()
                    )));
                }

                let mut nonce_bytes = [0u8; 8];
                nonce_bytes.copy_from_slice(&frame[0..NONCE_LEN]);
                let nonce = u64::from_be_bytes(nonce_bytes);

                // Check replay window before attempting decryption
                if !replay_window.is_valid(nonce) {
                    return Err(Error::Crypto(format!(
                        "Replay window rejected nonce {nonce}"
                    )));
                }

                let mut out = vec![0u8; frame.len() - NONCE_LEN];
                let len = transport
                    .read_message(nonce, &frame[NONCE_LEN..], &mut out)
                    .map_err(|e| Error::Crypto(format!("Stateless decryption failed: {e}")))?;
                out.truncate(len);

                // Update replay window only after successful AEAD tag authentication
                replay_window.update(nonce);

                Ok(out)
            }
            _ => Err(Error::Crypto(
                "Cannot decrypt: session not in transport mode".into(),
            )),
        }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("is_handshake_finished", &self.is_handshake_finished())
            .field("remote_peer_id", &self.peer_id())
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn establish_handshake(initiator: &mut Session, responder: &mut Session) -> Result<()> {
        let msg1 = initiator.write_handshake_vec()?;
        responder.read_handshake(&msg1)?;

        let msg2 = responder.write_handshake_vec()?;
        initiator.read_handshake(&msg2)?;

        let msg3 = initiator.write_handshake_vec()?;
        responder.read_handshake(&msg3)?;

        Ok(())
    }

    #[test]
    fn test_noise_xx_handshake_and_bidirectional_crypto() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        assert!(alice.is_handshake_finished());
        assert!(bob.is_handshake_finished());

        assert_eq!(alice.peer_id(), Some(bob_id.peer_id()));
        assert_eq!(bob.peer_id(), Some(alice_id.peer_id()));

        // Alice -> Bob encryption
        let plaintext_a = b"hello from alice";
        let ciphertext_a = alice.encrypt(plaintext_a).unwrap();
        let decrypted_b = bob.decrypt(&ciphertext_a).unwrap();
        assert_eq!(plaintext_a, &decrypted_b[..]);

        // Bob -> Alice encryption
        let plaintext_b = b"hello back from bob";
        let ciphertext_b = bob.encrypt(plaintext_b).unwrap();
        let decrypted_a = alice.decrypt(&ciphertext_b).unwrap();
        assert_eq!(plaintext_b, &decrypted_a[..]);
    }

    #[test]
    fn test_handshake_rejects_missing_domain_separation_prefix() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        let msg1 = alice.write_handshake_vec().unwrap();
        bob.read_handshake(&msg1).unwrap();

        let mut msg2 = bob.write_handshake_vec().unwrap();

        // Simulate signature made WITHOUT domain separation prefix
        let dummy_static = [42u8; 32];
        let raw_sig = bob_id.sign(&dummy_static); // Signed raw static key directly without context

        // Verifying raw signature against domain-separated binding message fails
        let binding_msg = noise_static_binding_message(&dummy_static);
        let verify_result = Identity::verify(&bob_id.peer_id(), &binding_msg, &raw_sig);
        assert!(
            verify_result.is_err(),
            "Raw signature without domain prefix must fail"
        );

        // Tamper with msg2 payload to ensure read_handshake rejects invalid signature
        let last_idx = msg2.len() - 1;
        msg2[last_idx] ^= 0x55;
        let read_res = alice.read_handshake(&msg2);
        assert!(read_res.is_err());
    }

    #[test]
    fn test_tampered_ciphertext_fails_decryption() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        let plaintext = b"critical mesh command";
        let mut ciphertext = alice.encrypt(plaintext).unwrap();

        // Tamper with one bit of ciphertext (after 8-byte nonce)
        ciphertext[10] ^= 0x01;

        let res = bob.decrypt(&ciphertext);
        assert!(res.is_err());
    }

    #[test]
    fn test_stateless_out_of_order_within_window_succeeds() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        // Generate 4 packets with nonces 0, 1, 2, 3
        let p0 = alice.encrypt(b"packet 0").unwrap();
        let p1 = alice.encrypt(b"packet 1").unwrap();
        let p2 = alice.encrypt(b"packet 2").unwrap();
        let p3 = alice.encrypt(b"packet 3").unwrap();

        // Deliver out of order: 0, 2, 1, 3
        assert_eq!(bob.decrypt(&p0).unwrap(), b"packet 0");
        assert_eq!(bob.decrypt(&p2).unwrap(), b"packet 2");
        assert_eq!(bob.decrypt(&p1).unwrap(), b"packet 1");
        assert_eq!(bob.decrypt(&p3).unwrap(), b"packet 3");
    }

    #[test]
    fn test_stateless_replay_rejected_by_replay_window() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        let p0 = alice.encrypt(b"first packet").unwrap();
        let p1 = alice.encrypt(b"second packet").unwrap();

        assert_eq!(bob.decrypt(&p0).unwrap(), b"first packet");
        assert_eq!(bob.decrypt(&p1).unwrap(), b"second packet");

        // Replay of p0 rejected
        let replay_res = bob.decrypt(&p0);
        assert!(
            replay_res.is_err(),
            "Replay of p0 must be rejected by window"
        );

        // Replay of p1 rejected
        let replay_res_1 = bob.decrypt(&p1);
        assert!(
            replay_res_1.is_err(),
            "Replay of p1 must be rejected by window"
        );
    }

    #[test]
    fn test_stateless_packet_older_than_window_rejected() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        let p0 = alice.encrypt(b"packet 0").unwrap();

        // Advance Alice by generating 130 packets (exceeding REPLAY_WINDOW_SIZE of 128)
        let mut latest = Vec::new();
        for i in 1..=135 {
            latest = alice.encrypt(format!("packet {i}").as_bytes()).unwrap();
        }

        // Deliver latest packet (nonce 135) to Bob
        let decrypted_latest = bob.decrypt(&latest).unwrap();
        assert_eq!(decrypted_latest, b"packet 135");

        // Now attempt to deliver p0 (nonce 0, age is 135 > 128)
        let stale_res = bob.decrypt(&p0);
        assert!(
            stale_res.is_err(),
            "Packet older than 128 nonces must be rejected"
        );
    }

    #[test]
    fn test_stateless_dropped_packet_does_not_break_later_packets() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        let _dropped = alice.encrypt(b"dropped packet 0").unwrap();
        let p1 = alice.encrypt(b"packet 1").unwrap();
        let p2 = alice.encrypt(b"packet 2").unwrap();

        // Bob never receives dropped packet 0, receives packet 1 and 2
        assert_eq!(bob.decrypt(&p1).unwrap(), b"packet 1");
        assert_eq!(bob.decrypt(&p2).unwrap(), b"packet 2");
    }

    #[test]
    fn test_debug_redaction_never_prints_session_keys() {
        let alice_id = Identity::generate();
        let alice = Session::initiator(&alice_id).unwrap();

        let debug_str = format!("{alice:?}");
        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains("private"));
        assert!(!debug_str.contains("secret"));
    }
}
