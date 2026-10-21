use bytes::{Buf, BufMut, BytesMut};

use crate::error::{Error, Result};
use crate::identity::{PeerId, ShortId};

pub const HEADER_LEN: usize = 31;
pub const CURRENT_VERSION: u8 = 1;
pub const MAX_HOPS: u8 = 7;
pub const MAX_ENVELOPE_LEN: usize = 512;
pub const PADDING_BUCKETS: [usize; 4] = [64, 128, 256, 512];

/// Message type identifier for wire framing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MsgType {
    Announce,    // 0
    Handshake,   // 1
    Chat,        // 2
    Ack,         // 3
    Vouch,       // 4
    TrustUpdate, // 5
    Cover,       // 6
    Reserved(u8),
}

impl MsgType {
    pub fn to_u8(self) -> u8 {
        match self {
            MsgType::Announce => 0,
            MsgType::Handshake => 1,
            MsgType::Chat => 2,
            MsgType::Ack => 3,
            MsgType::Vouch => 4,
            MsgType::TrustUpdate => 5,
            MsgType::Cover => 6,
            MsgType::Reserved(val) => val,
        }
    }

    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => MsgType::Announce,
            1 => MsgType::Handshake,
            2 => MsgType::Chat,
            3 => MsgType::Ack,
            4 => MsgType::Vouch,
            5 => MsgType::TrustUpdate,
            6 => MsgType::Cover,
            other => MsgType::Reserved(other),
        }
    }
}

/// Bitflags representing delivery options and envelope metadata.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Flags {
    pub privacy_mode: bool, // bit 0 (0x01)
    pub priority: bool,     // bit 1 (0x02)
    pub padded: bool,       // bit 2 (0x04)
    pub fragment: bool,     // bit 3 (0x08)
}

impl Flags {
    pub fn to_u8(self) -> u8 {
        let mut byte = 0u8;
        if self.privacy_mode {
            byte |= 0x01;
        }
        if self.priority {
            byte |= 0x02;
        }
        if self.padded {
            byte |= 0x04;
        }
        if self.fragment {
            byte |= 0x08;
        }
        byte
    }

    pub fn from_u8(val: u8) -> Self {
        Self {
            privacy_mode: (val & 0x01) != 0,
            priority: (val & 0x02) != 0,
            padded: (val & 0x04) != 0,
            fragment: (val & 0x08) != 0,
        }
    }
}

/// Compute canonical 8-byte message ID:
/// first 8 bytes of blake3(sender_pubkey || origin_counter_u64_be || blake3(payload))
pub fn compute_msg_id(sender_pubkey: &PeerId, origin_counter: u64, payload: &[u8]) -> [u8; 8] {
    let payload_hash = blake3::hash(payload);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&sender_pubkey.0);
    hasher.update(&origin_counter.to_be_bytes());
    hasher.update(payload_hash.as_bytes());
    let hash = hasher.finalize();

    let mut msg_id = [0u8; 8];
    msg_id.copy_from_slice(&hash.as_bytes()[0..8]);
    msg_id
}

/// Helper to pad payload to standard bucket size [64, 128, 256, 512] for metadata hiding.
/// Uses 2-byte big-endian length prefix to make padding reversible.
pub fn pad_to_bucket(payload: &[u8], bucket: usize) -> Result<Vec<u8>> {
    if !PADDING_BUCKETS.contains(&bucket) {
        return Err(Error::Envelope(format!(
            "Invalid bucket size {bucket}; valid buckets are {:?}",
            PADDING_BUCKETS
        )));
    }
    let total_needed = 2 + payload.len();
    if total_needed > bucket {
        return Err(Error::Envelope(format!(
            "Payload (len {}) with prefix exceeds target bucket {bucket}",
            payload.len()
        )));
    }
    let mut out = Vec::with_capacity(bucket);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out.resize(bucket, 0u8);
    Ok(out)
}

/// Reverse bucket padding, recovering the original payload buffer.
pub fn unpad_bucket(padded: &[u8]) -> Result<Vec<u8>> {
    if padded.len() < 2 {
        return Err(Error::Envelope("Padded buffer too short".into()));
    }
    let len = u16::from_be_bytes([padded[0], padded[1]]) as usize;
    if 2 + len > padded.len() {
        return Err(Error::Envelope(format!(
            "Invalid padded length header: prefix says {len}, but buffer is {}",
            padded.len()
        )));
    }
    Ok(padded[2..2 + len].to_vec())
}

/// Binary envelope frame with fixed 31-byte header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub version: u8,
    pub msg_type: MsgType,
    pub flags: Flags,
    pub hop_count: u8, // Relay-mutable
    pub ttl_max: u8,
    pub msg_id: [u8; 8],
    pub sender_short_id: ShortId,
    pub recipient_short_id: ShortId,
    pub payload: Vec<u8>,
}

impl Envelope {
    /// Encode envelope into byte buffer. Hand-written, byte-exact, big-endian.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let total_len = HEADER_LEN + self.payload.len();
        if total_len > MAX_ENVELOPE_LEN {
            return Err(Error::Envelope(format!(
                "Envelope total length {total_len} exceeds max {MAX_ENVELOPE_LEN}"
            )));
        }
        if self.ttl_max > MAX_HOPS {
            return Err(Error::Envelope(format!(
                "ttl_max {} exceeds MAX_HOPS {MAX_HOPS}",
                self.ttl_max
            )));
        }
        if self.hop_count > self.ttl_max {
            return Err(Error::Envelope(format!(
                "hop_count {} exceeds ttl_max {}",
                self.hop_count, self.ttl_max
            )));
        }

        let mut buf = BytesMut::with_capacity(total_len);
        buf.put_u8(self.version);
        buf.put_u8(self.msg_type.to_u8());
        buf.put_u8(self.flags.to_u8());
        buf.put_u8(self.hop_count);
        buf.put_u8(self.ttl_max);
        buf.put_slice(&self.msg_id);
        buf.put_slice(&self.sender_short_id.0);
        buf.put_slice(&self.recipient_short_id.0);
        buf.put_u16(self.payload.len() as u16);
        buf.put_slice(&self.payload);

        Ok(buf.to_vec())
    }

    /// Decode envelope from byte buffer. Hand-written, byte-exact, big-endian.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN {
            return Err(Error::Envelope(format!(
                "Truncated envelope: buffer len {} is less than header len {HEADER_LEN}",
                buf.len()
            )));
        }
        if buf.len() > MAX_ENVELOPE_LEN {
            return Err(Error::Envelope(format!(
                "Envelope len {} exceeds MAX_ENVELOPE_LEN {MAX_ENVELOPE_LEN}",
                buf.len()
            )));
        }

        let mut reader = buf;

        let version = reader.get_u8();
        if version != CURRENT_VERSION {
            return Err(Error::Envelope(format!(
                "Unsupported envelope version {version}, expected {CURRENT_VERSION}"
            )));
        }

        let msg_type = MsgType::from_u8(reader.get_u8());
        let flags = Flags::from_u8(reader.get_u8());
        let hop_count = reader.get_u8();
        let ttl_max = reader.get_u8();

        if ttl_max > MAX_HOPS {
            return Err(Error::Envelope(format!(
                "ttl_max {ttl_max} exceeds MAX_HOPS {MAX_HOPS}"
            )));
        }
        if hop_count > ttl_max {
            return Err(Error::Envelope(format!(
                "hop_count {hop_count} exceeds ttl_max {ttl_max}"
            )));
        }

        let mut msg_id = [0u8; 8];
        reader.copy_to_slice(&mut msg_id);

        let mut sender_short = [0u8; 8];
        reader.copy_to_slice(&mut sender_short);

        let mut recipient_short = [0u8; 8];
        reader.copy_to_slice(&mut recipient_short);

        let payload_len = reader.get_u16() as usize;
        if reader.remaining() != payload_len {
            return Err(Error::Envelope(format!(
                "Payload len mismatch: header declared {payload_len}, remaining buffer has {}",
                reader.remaining()
            )));
        }

        let payload = reader.copy_to_bytes(payload_len).to_vec();

        Ok(Self {
            version,
            msg_type,
            flags,
            hop_count,
            ttl_max,
            msg_id,
            sender_short_id: ShortId(sender_short),
            recipient_short_id: ShortId(recipient_short),
            payload,
        })
    }

    /// Increment hop count at relay node. Rejects if TTL is reached.
    pub fn increment_hop(&mut self) -> Result<()> {
        if self.hop_count >= self.ttl_max {
            return Err(Error::Envelope(format!(
                "TTL expired: hop_count {} reached ttl_max {}",
                self.hop_count, self.ttl_max
            )));
        }
        self.hop_count += 1;
        Ok(())
    }

    /// Produce canonical bytes for signing/MAC verification.
    /// Excludes relay-mutable hop_count (zeroed out) so relays can forward without
    /// invalidating signatures or authentication tags.
    pub fn auth_bytes(&self) -> Result<Vec<u8>> {
        let mut clone = self.clone();
        clone.hop_count = 0;
        clone.encode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn valid_envelope_strategy() -> impl Strategy<Value = Envelope> {
        (
            any::<u8>(),
            any::<u8>(),
            0u8..=MAX_HOPS,
            any::<[u8; 8]>(),
            any::<[u8; 8]>(),
            any::<[u8; 8]>(),
            proptest::collection::vec(any::<u8>(), 0..=(MAX_ENVELOPE_LEN - HEADER_LEN)),
        )
            .prop_map(
                |(msg_type_raw, flags_raw, ttl_max, msg_id, sender, recipient, payload)| {
                    let hop_count = if ttl_max == 0 {
                        0
                    } else {
                        rand::random::<u8>() % (ttl_max + 1)
                    };
                    Envelope {
                        version: CURRENT_VERSION,
                        msg_type: MsgType::from_u8(msg_type_raw),
                        flags: Flags::from_u8(flags_raw),
                        hop_count,
                        ttl_max,
                        msg_id,
                        sender_short_id: ShortId(sender),
                        recipient_short_id: ShortId(recipient),
                        payload,
                    }
                },
            )
    }

    proptest! {
        #[test]
        fn proptest_envelope_encode_decode_roundtrip(env in valid_envelope_strategy()) {
            let encoded = env.encode().unwrap();
            let decoded = Envelope::decode(&encoded).unwrap();
            prop_assert_eq!(env, decoded);
        }
    }

    #[test]
    fn test_reject_wrong_version() {
        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Chat,
            flags: Flags::default(),
            hop_count: 0,
            ttl_max: 3,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![42],
        };
        let mut encoded = env.encode().unwrap();
        encoded[0] = 99; // invalid version
        assert!(Envelope::decode(&encoded).is_err());
    }

    #[test]
    fn test_reject_truncated_header() {
        let truncated = [0u8; 20];
        assert!(Envelope::decode(&truncated).is_err());
    }

    #[test]
    fn test_reject_payload_len_mismatch() {
        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Chat,
            flags: Flags::default(),
            hop_count: 0,
            ttl_max: 3,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![1, 2, 3],
        };
        let mut encoded = env.encode().unwrap();
        encoded[30] = 10; // claim 10 bytes instead of 3
        assert!(Envelope::decode(&encoded).is_err());
    }

    #[test]
    fn test_reject_hop_count_exceeds_ttl() {
        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Chat,
            flags: Flags::default(),
            hop_count: 0,
            ttl_max: 3,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![1, 2, 3],
        };
        let mut encoded = env.encode().unwrap();
        encoded[3] = 4; // hop_count = 4, ttl_max = 3
        assert!(Envelope::decode(&encoded).is_err());
    }

    #[test]
    fn test_reject_ttl_exceeds_max_hops() {
        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Chat,
            flags: Flags::default(),
            hop_count: 0,
            ttl_max: 3,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![1, 2, 3],
        };
        let mut encoded = env.encode().unwrap();
        encoded[4] = 8; // ttl_max = 8 > MAX_HOPS (7)
        assert!(Envelope::decode(&encoded).is_err());
    }

    #[test]
    fn test_reject_buffer_larger_than_max_envelope() {
        let large_buf = vec![0u8; MAX_ENVELOPE_LEN + 1];
        assert!(Envelope::decode(&large_buf).is_err());
    }

    #[test]
    fn test_relay_increment_hop_count() {
        let mut env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Chat,
            flags: Flags::default(),
            hop_count: 1,
            ttl_max: 3,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![10, 20],
        };

        env.increment_hop().unwrap();
        assert_eq!(env.hop_count, 2);
        env.increment_hop().unwrap();
        assert_eq!(env.hop_count, 3);
        assert!(env.increment_hop().is_err()); // TTL expired
    }

    #[test]
    fn test_unknown_msg_type_survives_roundtrip() {
        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type: MsgType::Reserved(244),
            flags: Flags::default(),
            hop_count: 0,
            ttl_max: 5,
            msg_id: [1; 8],
            sender_short_id: ShortId([2; 8]),
            recipient_short_id: ShortId([3; 8]),
            payload: vec![99],
        };
        let encoded = env.encode().unwrap();
        let decoded = Envelope::decode(&encoded).unwrap();
        assert_eq!(decoded.msg_type, MsgType::Reserved(244));
    }

    #[test]
    fn test_padding_helper_reversible() {
        let original = b"secret message";
        for &bucket in &PADDING_BUCKETS {
            let padded = pad_to_bucket(original, bucket).unwrap();
            assert_eq!(padded.len(), bucket);
            let unpadded = unpad_bucket(&padded).unwrap();
            assert_eq!(unpadded, original);
        }
    }
}
