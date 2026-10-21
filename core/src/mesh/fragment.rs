use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

// ponytail: link-layer 4B header fragmentation (u16 id, u8 index/total). In-memory LRU assembly bounded at 64 frames.

pub const FRAGMENT_HEADER_LEN: usize = 4;
pub const DEFAULT_MAX_PARTIAL_FRAMES: usize = 64;
pub const DEFAULT_FRAGMENT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_MAX_FRAME_BYTES: usize = 2048;

/// Link-layer fragment header preceding BLE payload chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    pub frame_id: u16,
    pub index: u8,
    pub total: u8,
}

impl FragmentHeader {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < FRAGMENT_HEADER_LEN {
            return Err(Error::Fragmentation(format!(
                "Buffer too short for fragment header: {} < {}",
                buf.len(),
                FRAGMENT_HEADER_LEN
            )));
        }

        let frame_id = u16::from_be_bytes([buf[0], buf[1]]);
        let index = buf[2];
        let total = buf[3];

        if total == 0 {
            return Err(Error::Fragmentation(
                "Invalid fragment header: total fragments cannot be 0".into(),
            ));
        }

        if index >= total {
            return Err(Error::Fragmentation(format!(
                "Invalid fragment header: index {index} >= total {total}"
            )));
        }

        Ok(Self {
            frame_id,
            index,
            total,
        })
    }

    pub fn encode(&self, dst: &mut [u8]) -> Result<()> {
        if dst.len() < FRAGMENT_HEADER_LEN {
            return Err(Error::Fragmentation(
                "Destination buffer too small for fragment header".into(),
            ));
        }
        dst[0..2].copy_from_slice(&self.frame_id.to_be_bytes());
        dst[2] = self.index;
        dst[3] = self.total;
        Ok(())
    }
}

/// Splits wire frames into MTU-bounded link-layer fragments.
pub struct Fragmenter {
    next_frame_id: AtomicU16,
}

impl Default for Fragmenter {
    fn default() -> Self {
        Self::new()
    }
}

impl Fragmenter {
    pub fn new() -> Self {
        Self {
            next_frame_id: AtomicU16::new(1),
        }
    }

    /// Split a frame into fragments bounded by `mtu`.
    pub fn split(&self, frame: &[u8], mtu: usize) -> Result<Vec<Vec<u8>>> {
        let frame_id = self.next_frame_id.fetch_add(1, Ordering::Relaxed);
        Self::split_with_id(frame_id, frame, mtu)
    }

    /// Split a frame with a predetermined `frame_id`.
    pub fn split_with_id(frame_id: u16, frame: &[u8], mtu: usize) -> Result<Vec<Vec<u8>>> {
        if mtu <= FRAGMENT_HEADER_LEN {
            return Err(Error::Fragmentation(format!(
                "MTU {mtu} must be greater than header length {FRAGMENT_HEADER_LEN}"
            )));
        }

        let chunk_size = mtu - FRAGMENT_HEADER_LEN;

        if frame.is_empty() {
            let mut frag = vec![0u8; FRAGMENT_HEADER_LEN];
            let header = FragmentHeader {
                frame_id,
                index: 0,
                total: 1,
            };
            header.encode(&mut frag)?;
            return Ok(vec![frag]);
        }

        let num_chunks = frame.len().div_ceil(chunk_size);
        if num_chunks > 255 {
            return Err(Error::Fragmentation(format!(
                "Frame size {} exceeds 255 fragments for chunk size {chunk_size}",
                frame.len()
            )));
        }

        let total = num_chunks as u8;
        let mut fragments = Vec::with_capacity(num_chunks);

        for (i, chunk) in frame.chunks(chunk_size).enumerate() {
            let index = i as u8;
            let mut frag = Vec::with_capacity(FRAGMENT_HEADER_LEN + chunk.len());
            frag.extend_from_slice(&frame_id.to_be_bytes());
            frag.push(index);
            frag.push(total);
            frag.extend_from_slice(chunk);
            fragments.push(frag);
        }

        Ok(fragments)
    }
}

/// Configuration and memory bounds for fragment reassembly.
#[derive(Debug, Clone)]
pub struct ReassemblerConfig {
    pub max_partial_frames: usize,
    pub timeout: Duration,
    pub max_frame_bytes: usize,
}

impl Default for ReassemblerConfig {
    fn default() -> Self {
        Self {
            max_partial_frames: DEFAULT_MAX_PARTIAL_FRAMES,
            timeout: DEFAULT_FRAGMENT_TIMEOUT,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

struct PartialFrame {
    total: u8,
    received_count: u8,
    total_bytes: usize,
    created_at: Instant,
    fragments: Vec<Option<Vec<u8>>>,
}

/// Bounded-memory link-layer frame reassembler.
///
/// Defends against hostile memory exhaustion via strict bounds on:
/// - Maximum concurrent partial frames (oldest evicted on overflow)
/// - Maximum frame byte size
/// - Per-frame completion timeout
pub struct Reassembler {
    config: ReassemblerConfig,
    frames: HashMap<(Option<[u8; 8]>, u16), PartialFrame>,
    order: VecDeque<(Option<[u8; 8]>, u16)>,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new(ReassemblerConfig::default())
    }
}

impl Reassembler {
    pub fn new(config: ReassemblerConfig) -> Self {
        Self {
            config,
            frames: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Process an inbound fragment from an anonymous/broadcast link.
    pub fn process(&mut self, fragment: &[u8], now: Instant) -> Result<Option<Vec<u8>>> {
        self.process_from(None, fragment, now)
    }

    /// Process an inbound fragment with an optional peer identifier.
    pub fn process_from(
        &mut self,
        peer: Option<[u8; 8]>,
        fragment: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>> {
        let header = FragmentHeader::decode(fragment)?;
        let payload = &fragment[FRAGMENT_HEADER_LEN..];

        // Fast-path: single unfragmented frame
        if header.total == 1 {
            return Ok(Some(payload.to_vec()));
        }

        self.purge_expired(now);

        let key = (peer, header.frame_id);

        if !self.frames.contains_key(&key) {
            // Check capacity limit, evict oldest if full
            while self.frames.len() >= self.config.max_partial_frames {
                if let Some(oldest_key) = self.order.pop_front() {
                    self.frames.remove(&oldest_key);
                } else {
                    break;
                }
            }

            let initial_vec = vec![None; header.total as usize];
            self.frames.insert(
                key,
                PartialFrame {
                    total: header.total,
                    received_count: 0,
                    total_bytes: 0,
                    created_at: now,
                    fragments: initial_vec,
                },
            );
            self.order.push_back(key);
        }

        if let Some(entry) = self.frames.get(&key) {
            if entry.total != header.total {
                let expected = entry.total;
                self.frames.remove(&key);
                return Err(Error::Fragmentation(format!(
                    "Inconsistent total fragments for frame {}: expected {}, got {}",
                    header.frame_id, expected, header.total
                )));
            }
        }

        let entry = match self.frames.get_mut(&key) {
            Some(e) => e,
            None => {
                return Err(Error::Fragmentation(
                    "Internal state error: partial frame missing after insert".into(),
                ))
            }
        };

        let idx = header.index as usize;
        if idx >= entry.fragments.len() {
            return Err(Error::Fragmentation(format!(
                "Fragment index {idx} out of range for total {}",
                entry.fragments.len()
            )));
        }

        // Duplicate check
        if entry.fragments[idx].is_some() {
            return Ok(None);
        }

        // Check cumulative frame byte bound
        let new_total_bytes = entry.total_bytes.saturating_add(payload.len());
        if new_total_bytes > self.config.max_frame_bytes {
            let max_bytes = self.config.max_frame_bytes;
            // Drop mutable reference before removal
            let _ = entry;
            self.frames.remove(&key);
            return Err(Error::Fragmentation(format!(
                "Frame {} exceeded maximum allowed bytes ({})",
                header.frame_id, max_bytes
            )));
        }

        entry.total_bytes = new_total_bytes;
        entry.fragments[idx] = Some(payload.to_vec());
        entry.received_count = entry.received_count.saturating_add(1);

        if entry.received_count == entry.total {
            // Reassembly complete
            if let Some(completed) = self.frames.remove(&key) {
                let mut assembled = Vec::with_capacity(completed.total_bytes);
                for piece in completed.fragments.into_iter().flatten() {
                    assembled.extend_from_slice(&piece);
                }
                return Ok(Some(assembled));
            }
        }

        Ok(None)
    }

    /// Purge partially reassembled frames that have exceeded `timeout`.
    pub fn purge_expired(&mut self, now: Instant) {
        let timeout = self.config.timeout;
        self.frames
            .retain(|_, frame| now.saturating_duration_since(frame.created_at) < timeout);
        // Prune order queue
        self.order.retain(|k| self.frames.contains_key(k));
    }

    pub fn active_frames_count(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_single_fragment_fast_path() {
        let mut reassembler = Reassembler::default();
        let payload = b"small frame";
        let frags = Fragmenter::split_with_id(42, payload, 100).unwrap();
        assert_eq!(frags.len(), 1);

        let res = reassembler.process(&frags[0], Instant::now()).unwrap();
        assert_eq!(res, Some(payload.to_vec()));
        assert_eq!(reassembler.active_frames_count(), 0);
    }

    #[test]
    fn test_empty_frame_roundtrip() {
        let mut reassembler = Reassembler::default();
        let frags = Fragmenter::split_with_id(1, b"", 20).unwrap();
        assert_eq!(frags.len(), 1);

        let res = reassembler.process(&frags[0], Instant::now()).unwrap();
        assert_eq!(res, Some(vec![]));
    }

    #[test]
    fn test_multi_fragment_in_order() {
        let mut reassembler = Reassembler::default();
        let payload = b"Hello, this is a multi-fragment test payload for Bitchat 2.0!";
        let frags = Fragmenter::split_with_id(10, payload, 14).unwrap();
        assert!(frags.len() > 1);

        let now = Instant::now();
        for (i, frag) in frags.iter().enumerate() {
            let res = reassembler.process(frag, now).unwrap();
            if i == frags.len() - 1 {
                assert_eq!(res, Some(payload.to_vec()));
            } else {
                assert_eq!(res, None);
            }
        }
        assert_eq!(reassembler.active_frames_count(), 0);
    }

    #[test]
    fn test_out_of_order_fragments() {
        let mut reassembler = Reassembler::default();
        let payload = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let frags = Fragmenter::split_with_id(100, payload, 10).unwrap();
        assert_eq!(frags.len(), 6);

        let now = Instant::now();
        // Feed in reverse order: 5, 4, 3, 2, 1, 0
        for (i, idx) in [5, 3, 1, 4, 2, 0].into_iter().enumerate() {
            let res = reassembler.process(&frags[idx], now).unwrap();
            if i == 5 {
                assert_eq!(res, Some(payload.to_vec()));
            } else {
                assert_eq!(res, None);
            }
        }
        assert_eq!(reassembler.active_frames_count(), 0);
    }

    #[test]
    fn test_duplicate_fragments_ignored() {
        let mut reassembler = Reassembler::default();
        let payload = b"Duplicate test frame data";
        let frags = Fragmenter::split_with_id(7, payload, 10).unwrap();
        let now = Instant::now();

        // Deliver frag 0 twice
        assert_eq!(reassembler.process(&frags[0], now).unwrap(), None);
        assert_eq!(reassembler.process(&frags[0], now).unwrap(), None);

        // Deliver remaining
        for frag in &frags[1..frags.len() - 1] {
            assert_eq!(reassembler.process(frag, now).unwrap(), None);
        }
        let res = reassembler.process(&frags[frags.len() - 1], now).unwrap();
        assert_eq!(res, Some(payload.to_vec()));
    }

    #[test]
    fn test_missing_fragment_timeout_and_freed() {
        let mut reassembler = Reassembler::new(ReassemblerConfig {
            max_partial_frames: 10,
            timeout: Duration::from_secs(5),
            max_frame_bytes: 1024,
        });

        let payload = b"Incomplete frame missing pieces";
        let frags = Fragmenter::split_with_id(99, payload, 10).unwrap();
        let t0 = Instant::now();

        // Feed only first fragment
        assert_eq!(reassembler.process(&frags[0], t0).unwrap(), None);
        assert_eq!(reassembler.active_frames_count(), 1);

        // Time advances past timeout
        let t1 = t0 + Duration::from_secs(6);
        reassembler.purge_expired(t1);
        assert_eq!(reassembler.active_frames_count(), 0);
    }

    #[test]
    fn test_overflow_eviction() {
        let capacity = 3;
        let mut reassembler = Reassembler::new(ReassemblerConfig {
            max_partial_frames: capacity,
            timeout: Duration::from_secs(60),
            max_frame_bytes: 1024,
        });

        let now = Instant::now();
        // Insert 3 incomplete frames (total=2, feed only index 0)
        for id in 1..=3 {
            let frags = Fragmenter::split_with_id(id, b"chunk_a_chunk_b", 10).unwrap();
            assert_eq!(reassembler.process(&frags[0], now).unwrap(), None);
        }
        assert_eq!(reassembler.active_frames_count(), 3);

        // 4th frame arrives -> frame 1 (oldest) must be evicted
        let frags4 = Fragmenter::split_with_id(4, b"chunk_a_chunk_b", 10).unwrap();
        assert_eq!(reassembler.process(&frags4[0], now).unwrap(), None);
        assert_eq!(reassembler.active_frames_count(), 3);

        // Try finishing frame 1 (now evicted) -> treated as new frame, needs all fragments
        let frags1 = Fragmenter::split_with_id(1, b"chunk_a_chunk_b", 10).unwrap();
        // Feeding only index 1 should not complete it because index 0 was evicted
        let res = reassembler.process(&frags1[1], now).unwrap();
        assert_eq!(res, None);
    }

    #[test]
    fn test_malformed_headers_rejected() {
        let mut reassembler = Reassembler::default();
        let now = Instant::now();

        // Buffer shorter than header len 4
        assert!(reassembler.process(&[1, 2, 3], now).is_err());

        // Total fragments = 0
        assert!(reassembler.process(&[0, 1, 0, 0], now).is_err());

        // Index >= total (index 2 with total 2)
        assert!(reassembler.process(&[0, 1, 2, 2], now).is_err());

        // Inconsistent total fragments for active frame
        let f1 = Fragmenter::split_with_id(50, b"data12345678", 8).unwrap();
        assert_eq!(reassembler.process(&f1[0], now).unwrap(), None);

        // Tamper total in next fragment
        let mut bad_frag = f1[1].clone();
        bad_frag[3] = 99; // change total
        assert!(reassembler.process(&bad_frag, now).is_err());
    }

    proptest! {
        #[test]
        fn proptest_split_reassemble_roundtrip(
            payload in proptest::collection::vec(any::<u8>(), 0..=512),
            mtu in 5usize..=128
        ) {
            let chunk_size = mtu - FRAGMENT_HEADER_LEN;
            let num_chunks = if payload.is_empty() { 1 } else { payload.len().div_ceil(chunk_size) };

            if num_chunks > 255 {
                prop_assert!(Fragmenter::split_with_id(1234, &payload, mtu).is_err());
            } else {
                let mut reassembler = Reassembler::default();
                let frags = Fragmenter::split_with_id(1234, &payload, mtu).unwrap();
                let now = Instant::now();

                let mut result = None;
                for frag in frags {
                    if let Some(assembled) = reassembler.process(&frag, now).unwrap() {
                        result = Some(assembled);
                    }
                }

                prop_assert_eq!(result, Some(payload));
                prop_assert_eq!(reassembler.active_frames_count(), 0);
            }
        }
    }
}
