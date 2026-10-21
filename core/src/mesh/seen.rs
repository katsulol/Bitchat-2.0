use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::mesh::MsgId;

/// Interface for message deduplication caches in the mesh relay layer.
pub trait SeenCache: Send + Sync {
    /// Returns `true` if `id` was already seen within the active window;
    /// otherwise inserts `id` and returns `false`.
    fn check_and_insert(&mut self, id: MsgId, now: Instant) -> bool;
}

/// Exact, bounded-capacity LRU seen cache with TTL expiration.
///
/// Default deduplication engine for Bitchat 2.0. Guarantees 0% false positives,
/// ensuring genuine mesh packets are never mistakenly dropped.
pub struct LruSeenCache {
    capacity: usize,
    ttl: Duration,
    map: HashMap<MsgId, Instant>,
    queue: VecDeque<(MsgId, Instant)>,
}

impl LruSeenCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl,
            map: HashMap::with_capacity(capacity),
            queue: VecDeque::with_capacity(capacity),
        }
    }

    /// Number of active non-expired entries in the cache.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn purge_expired(&mut self, now: Instant) {
        while let Some(&(id, timestamp)) = self.queue.front() {
            if now.saturating_duration_since(timestamp) >= self.ttl {
                self.queue.pop_front();
                // Only remove from map if the timestamp in map matches the expired queue timestamp
                if let Some(&map_ts) = self.map.get(&id) {
                    if map_ts == timestamp {
                        self.map.remove(&id);
                    }
                }
            } else {
                break;
            }
        }
    }
}

impl SeenCache for LruSeenCache {
    fn check_and_insert(&mut self, id: MsgId, now: Instant) -> bool {
        self.purge_expired(now);

        if let Some(&timestamp) = self.map.get(&id) {
            if now.saturating_duration_since(timestamp) < self.ttl {
                return true;
            }
            // Expired entry found in map
            self.map.remove(&id);
        }

        // Evict oldest if capacity is reached
        while self.map.len() >= self.capacity {
            if let Some((oldest_id, old_ts)) = self.queue.pop_front() {
                if let Some(&map_ts) = self.map.get(&oldest_id) {
                    if map_ts == old_ts {
                        self.map.remove(&oldest_id);
                    }
                }
            } else {
                break;
            }
        }

        self.map.insert(id, now);
        self.queue.push_back((id, now));
        false
    }
}

/// Single generation of a space-efficient Bloom filter using double hashing.
struct SingleBloom {
    num_bits: usize,
    num_hashes: usize,
    bits: Vec<u64>,
    count: usize,
}

impl SingleBloom {
    fn new(capacity: usize, target_fpr: f64) -> Self {
        let cap = capacity.max(1) as f64;
        let fpr = target_fpr.clamp(0.0001, 0.5);

        // Optimal number of bits m = - (n * ln(p)) / (ln(2)^2)
        let ln2 = std::f64::consts::LN_2;
        let num_bits = ((-cap * fpr.ln()) / (ln2 * ln2)).ceil() as usize;
        let num_bits = num_bits.max(64);

        // Optimal number of hash functions k = (m / n) * ln(2)
        let num_hashes = ((num_bits as f64 / cap) * ln2).round() as usize;
        let num_hashes = num_hashes.clamp(1, 16);

        let num_words = num_bits.div_ceil(64);

        Self {
            num_bits,
            num_hashes,
            bits: vec![0u64; num_words],
            count: 0,
        }
    }

    fn hashes(&self, id: &MsgId) -> (u64, u64) {
        let hash = blake3::hash(id);
        let bytes = hash.as_bytes();
        let mut h1_bytes = [0u8; 8];
        let mut h2_bytes = [0u8; 8];
        h1_bytes.copy_from_slice(&bytes[0..8]);
        h2_bytes.copy_from_slice(&bytes[8..16]);
        let h1 = u64::from_le_bytes(h1_bytes);
        let mut h2 = u64::from_le_bytes(h2_bytes);
        if h2 == 0 {
            h2 = 1; // Must not be 0 for double hashing
        }
        (h1, h2)
    }

    fn insert(&mut self, id: &MsgId) {
        let (h1, h2) = self.hashes(id);
        for i in 0..self.num_hashes {
            let bit_idx = (h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize) % self.num_bits;
            let word = bit_idx / 64;
            let bit = bit_idx % 64;
            if let Some(w) = self.bits.get_mut(word) {
                *w |= 1u64 << bit;
            }
        }
        self.count += 1;
    }

    fn contains(&self, id: &MsgId) -> bool {
        let (h1, h2) = self.hashes(id);
        for i in 0..self.num_hashes {
            let bit_idx = (h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize) % self.num_bits;
            let word = bit_idx / 64;
            let bit = bit_idx % 64;
            if let Some(&w) = self.bits.get(word) {
                if (w >> bit) & 1 == 0 {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }
}

/// Dual rotating Bloom filters for compact deduplication.
///
/// Old entries age out when the active filter reaches capacity or its rotation
/// interval elapses, rotating current into previous and resetting current.
pub struct BloomSeenCache {
    capacity: usize,
    target_fpr: f64,
    rotation_interval: Duration,
    last_rotation: Instant,
    current: SingleBloom,
    previous: SingleBloom,
}

impl BloomSeenCache {
    pub fn new(capacity: usize, target_fpr: f64, rotation_interval: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            target_fpr,
            rotation_interval,
            last_rotation: Instant::now(),
            current: SingleBloom::new(capacity, target_fpr),
            previous: SingleBloom::new(capacity, target_fpr),
        }
    }

    fn rotate_if_needed(&mut self, now: Instant) {
        let time_due = now.saturating_duration_since(self.last_rotation) >= self.rotation_interval;
        let cap_due = self.current.count >= self.capacity;

        if time_due || cap_due {
            self.previous = std::mem::replace(
                &mut self.current,
                SingleBloom::new(self.capacity, self.target_fpr),
            );
            self.last_rotation = now;
        }
    }
}

impl SeenCache for BloomSeenCache {
    fn check_and_insert(&mut self, id: MsgId, now: Instant) -> bool {
        self.rotate_if_needed(now);

        if self.current.contains(&id) || self.previous.contains(&id) {
            return true;
        }

        self.current.insert(&id);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_no_false_negatives() {
        let mut cache = LruSeenCache::new(100, Duration::from_secs(60));
        let now = Instant::now();

        for i in 0..100 {
            let id = [i as u8, 0, 0, 0, 0, 0, 0, 0];
            assert!(
                !cache.check_and_insert(id, now),
                "First sight must be false"
            );
            assert!(cache.check_and_insert(id, now), "Second sight must be true");
        }
    }

    #[test]
    fn test_bloom_no_false_negatives() {
        let mut cache = BloomSeenCache::new(500, 0.01, Duration::from_secs(60));
        let now = Instant::now();

        for i in 0..200 {
            let mut id = [0u8; 8];
            id[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            assert!(
                !cache.check_and_insert(id, now),
                "First sight must be false"
            );
            assert!(cache.check_and_insert(id, now), "Second sight must be true");
        }
    }

    #[test]
    fn test_lru_expiry() {
        let mut cache = LruSeenCache::new(100, Duration::from_secs(10));
        let t0 = Instant::now();
        let id = [1, 2, 3, 4, 5, 6, 7, 8];

        assert!(!cache.check_and_insert(id, t0));
        assert!(cache.check_and_insert(id, t0 + Duration::from_secs(5)));

        // After TTL expires (t0 + 11s)
        let t1 = t0 + Duration::from_secs(11);
        assert!(
            !cache.check_and_insert(id, t1),
            "Must expire and re-insert as false"
        );
    }

    #[test]
    fn test_lru_capacity_bound() {
        let capacity = 10;
        let mut cache = LruSeenCache::new(capacity, Duration::from_secs(60));
        let now = Instant::now();

        for i in 0..25 {
            let id = [i as u8, 0, 0, 0, 0, 0, 0, 0];
            cache.check_and_insert(id, now);
            assert!(
                cache.len() <= capacity,
                "Cache len must not exceed capacity"
            );
        }

        // Oldest items (0..15) must have been evicted
        for i in 0..15 {
            let id = [i as u8, 0, 0, 0, 0, 0, 0, 0];
            assert!(
                !cache.check_and_insert(id, now),
                "Evicted item must return false on insert"
            );
        }
    }

    #[test]
    fn test_bloom_measured_false_positive_rate() {
        let capacity = 1000;
        let target_fpr = 0.01; // 1% target
        let mut cache = BloomSeenCache::new(capacity, target_fpr, Duration::from_secs(3600));
        let now = Instant::now();

        // Fill to capacity
        for i in 0..capacity {
            let mut id = [0u8; 8];
            id[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            cache.check_and_insert(id, now);
        }

        // Query 2000 non-inserted items
        let test_queries = 2000;
        let mut false_positives = 0;
        for i in capacity..(capacity + test_queries) {
            let mut id = [0u8; 8];
            id[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            // Query without inserting by checking Contains directly or calling check_and_insert on a clone
            if cache.current.contains(&id) {
                false_positives += 1;
            }
        }

        let measured_fpr = false_positives as f64 / test_queries as f64;
        println!(
            "Bloom filter measured FPR: {:.4} (target: {:.4}, false positives: {} / {})",
            measured_fpr, target_fpr, false_positives, test_queries
        );

        // Assert loose bound: measured FPR must not exceed 3x the target (0.03)
        assert!(
            measured_fpr <= 0.035,
            "Measured FPR {:.4} exceeds loose bound 0.035",
            measured_fpr
        );
    }

    #[test]
    fn test_bloom_rotation_expiry() {
        let mut cache = BloomSeenCache::new(100, 0.01, Duration::from_secs(10));
        let t0 = Instant::now();
        let id = [99u8; 8];

        assert!(!cache.check_and_insert(id, t0));

        // After 1st rotation (+11s): moves to previous, still seen
        let t1 = t0 + Duration::from_secs(11);
        assert!(cache.check_and_insert(id, t1));

        // After 2nd rotation (+22s): previous cleared, item aged out
        let t2 = t0 + Duration::from_secs(22);
        assert!(
            !cache.check_and_insert(id, t2),
            "Item must age out after two rotations"
        );
    }
}
