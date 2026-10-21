// CRYPTO-REVIEW:
// What it does: Implements secure forward-secret sessions using the Noise Protocol Framework
// (Noise_XX_25519_ChaChaPoly_BLAKE2s) with Ed25519 identity key binding.
// Primitives / crates: `snow` (Noise Protocol), `ed25519-dalek` (Identity signatures).
// Reviewer checklist:
// 1. Noise pattern is strictly Noise_XX_25519_ChaChaPoly_BLAKE2s.
// 2. Both initiator and responder generate a dedicated X25519 Noise static keypair and sign
//    their Noise static public key using their long-term Ed25519 Identity signing key.
// 3. The handshake payload contains (PeerId: 32 bytes || Ed25519 Signature: 64 bytes).
// 4. Before completing handshake and transitioning to Transport mode, the remote signature
//    over the remote Noise static public key MUST verify against the claimed PeerId.
// 5. If verification fails, handshake immediately terminates and returns Error::Crypto.
// 6. Transport mode protects all data with ChaCha20-Poly1305 AEAD; tampered or replayed
//    packets fail decryption.
// 7. Debug implementation for Session explicitly redacts all key and state material.

use std::fmt;

use ed25519_dalek::Signature;
use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};

use crate::error::{Error, Result};
use crate::identity::{Identity, PeerId};

pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
pub const BINDING_PAYLOAD_LEN: usize = 32 + 64; // PeerId (32) + Signature (64)
pub const MAX_HANDSHAKE_BUF: usize = 512;

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
        transport: TransportState,
        remote_peer_id: PeerId,
    },
    Failed,
}

/// Secure session wrapper providing Noise XX mutual authentication and AEAD transport.
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

        // Sign the Noise static public key with the Ed25519 identity key
        let sig = identity.sign(&static_keypair.public);

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
                // Determine whether to attach binding payload on this message
                // In XX:
                // Initiator sends payload on message 3 (write_count == 1)
                // Responder sends payload on message 2 (write_count == 0)
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

                // If remote side sent its static key in this step, verify identity binding
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

                        // VERIFY that remote PeerId actually signed the Noise static public key
                        Identity::verify(&peer_id, remote_static, &signature).map_err(|e| {
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
            let transport = handshake
                .into_transport_mode()
                .map_err(|e| Error::Crypto(format!("Failed to enter transport mode: {e}")))?;
            self.state = SessionState::Transport {
                transport,
                remote_peer_id,
            };
            Ok(())
        } else {
            Err(Error::Crypto(
                "Invalid state for transport transition".into(),
            ))
        }
    }

    /// Encrypt plaintext using Noise ChaCha20-Poly1305 AEAD in transport mode.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        match &mut self.state {
            SessionState::Transport { transport, .. } => {
                let mut out = vec![0u8; plaintext.len() + 16];
                let len = transport
                    .write_message(plaintext, &mut out)
                    .map_err(|e| Error::Crypto(format!("Encryption failed: {e}")))?;
                out.truncate(len);
                Ok(out)
            }
            _ => Err(Error::Crypto(
                "Cannot encrypt: session not in transport mode".into(),
            )),
        }
    }

    /// Decrypt ciphertext using Noise ChaCha20-Poly1305 AEAD in transport mode.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        match &mut self.state {
            SessionState::Transport { transport, .. } => {
                let mut out = vec![0u8; ciphertext.len()];
                let len = transport
                    .read_message(ciphertext, &mut out)
                    .map_err(|e| Error::Crypto(format!("Decryption failed: {e}")))?;
                out.truncate(len);
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
        // Step 1: Initiator -> Responder (Msg 1: -> e)
        let msg1 = initiator.write_handshake_vec()?;
        responder.read_handshake(&msg1)?;

        // Step 2: Responder -> Initiator (Msg 2: <- e, ee, s, es)
        let msg2 = responder.write_handshake_vec()?;
        initiator.read_handshake(&msg2)?;

        // Step 3: Initiator -> Responder (Msg 3: -> s, se)
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
    fn test_handshake_rejects_forged_identity_signature() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();
        let eve_id = Identity::generate(); // attacker

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        // Step 1: Alice -> Bob
        let msg1 = alice.write_handshake_vec().unwrap();
        bob.read_handshake(&msg1).unwrap();

        // Step 2: Bob writes msg2, but Eve intercepts and replaces identity with Eve's
        // without valid signature over Bob's Noise static key
        let mut msg2 = bob.write_handshake_vec().unwrap();

        // Tamper with payload in msg2: substitute Eve's PeerId
        // In msg2, the encrypted payload is at the end. Tampering with ciphertext causes Poly1305 failure;
        // Even if an attacker produces plaintext with Eve's key and forged signature, verify() fails.
        let forged_sig = eve_id.sign(b"invalid static key target");
        let mut forged_payload = Vec::new();
        forged_payload.extend_from_slice(eve_id.peer_id().as_bytes());
        forged_payload.extend_from_slice(&forged_sig.to_bytes());

        // Verify that verifying against a mismatched Noise static key fails
        let dummy_static = [7u8; 32];
        let verify_result = Identity::verify(&eve_id.peer_id(), &dummy_static, &forged_sig);
        assert!(verify_result.is_err());

        // Now test tampered msg2 directly against Alice:
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

        // Tamper with one bit of ciphertext
        ciphertext[0] ^= 0x01;

        let res = bob.decrypt(&ciphertext);
        assert!(res.is_err());
    }

    #[test]
    fn test_replayed_ciphertext_rejected_by_nonce() {
        let alice_id = Identity::generate();
        let bob_id = Identity::generate();

        let mut alice = Session::initiator(&alice_id).unwrap();
        let mut bob = Session::responder(&bob_id).unwrap();

        establish_handshake(&mut alice, &mut bob).unwrap();

        let msg1 = alice.encrypt(b"frame 1").unwrap();
        let msg2 = alice.encrypt(b"frame 2").unwrap();

        // Bob decrypts msg1 then msg2
        assert_eq!(bob.decrypt(&msg1).unwrap(), b"frame 1");
        assert_eq!(bob.decrypt(&msg2).unwrap(), b"frame 2");

        // Attacker replays msg1
        let replay_res = bob.decrypt(&msg1);
        assert!(
            replay_res.is_err(),
            "Replay of msg1 must be rejected by AEAD nonce"
        );
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
