//! bitchat2_core: Foundation, identity, wire format, crypto, simulated transport.

#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod api;
pub mod crypto;
pub mod envelope;
pub mod error;
pub mod ffi;
pub mod identity;
pub mod mesh;
pub mod transport;
