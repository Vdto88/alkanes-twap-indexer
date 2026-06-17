#![allow(unused_imports)]
//! Indexer-side TWAP (v2 prototype): per-block price accumulator + read function.
//!
//! Storage (metashrew IndexPointer):
//!   /twap/pool                       -> registered pool AlkaneId bytes (single pool)
//!   /twap/mock-reserves/<pool>       -> 32 bytes: r0 LE [0..16] | r1 LE [16..32]  (test seed)
//!   /twap/cumulative/<pool>/<height> -> u128 cumulative (Q64.64 sum)
//!   /twap/first-height/<pool>        -> u32 first recorded height
//!   /twap/last-height/<pool>         -> u32 last recorded height (= the TWAP "tip")
use alkanes_support::id::AlkaneId;
use anyhow::{anyhow, Result};
use metashrew_core::index_pointer::IndexPointer;
use metashrew_support::index_pointer::KeyValuePointer;
use std::sync::Arc;

/// Fixed-point scale for the spot price (Q64.64). Mock reserves must stay < 2^(128-64).
pub const PRICE_SCALE_BITS: u32 = 64;

/// Q64.64 spot price = (r1 << 64) / r0. Returns 0 if r0 == 0.
pub fn spot_price_q64(r0: u128, r1: u128) -> u128 {
    if r0 == 0 {
        return 0;
    }
    (r1 << PRICE_SCALE_BITS) / r0
}
