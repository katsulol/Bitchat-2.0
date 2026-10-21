pub mod sim;

use crate::error::Result;
use crate::identity::ShortId;

/// Asynchronous transport interface for frame transmission and neighbor discovery.
///
/// In Phase 1, this is backed by `SimNetwork` (in-memory simulator).
/// In Phase 2, real Bluetooth Low Energy (BLE) radios implement this trait.
#[allow(async_fn_in_trait)]
pub trait Transport: Send + Sync {
    /// Send an addressed frame directly to a single neighbor in radio range.
    async fn send(&self, to: ShortId, frame: Vec<u8>) -> Result<()>;

    /// Broadcast a frame to all peers currently in radio range.
    async fn broadcast(&self, frame: Vec<u8>) -> Result<()>;

    /// Receive the next incoming frame: returns (sender_short_id, frame_bytes).
    async fn recv(&mut self) -> Option<(ShortId, Vec<u8>)>;

    /// Return list of currently reachable direct neighbors.
    fn neighbors(&self) -> Vec<ShortId>;
}
