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

// ---- storage pointers -------------------------------------------------------

fn pool_registry_ptr() -> IndexPointer {
    IndexPointer::from_keyword("/twap/pool")
}
fn reserves_ptr(pool: &AlkaneId) -> IndexPointer {
    IndexPointer::from_keyword("/twap/mock-reserves/").select(&pool.clone().into())
}
fn cumulative_ptr(pool: &AlkaneId) -> IndexPointer {
    IndexPointer::from_keyword("/twap/cumulative/").select(&pool.clone().into())
}
fn first_height_ptr(pool: &AlkaneId) -> IndexPointer {
    IndexPointer::from_keyword("/twap/first-height/").select(&pool.clone().into())
}
fn last_height_ptr(pool: &AlkaneId) -> IndexPointer {
    IndexPointer::from_keyword("/twap/last-height/").select(&pool.clone().into())
}

// ---- registry (single tracked pool) ----------------------------------------

pub fn register_pool(pool: &AlkaneId) {
    let mut p = pool_registry_ptr();
    p.set(Arc::new(pool.clone().into()));
}
pub fn unregister_pool() {
    let mut p = pool_registry_ptr();
    p.set(Arc::new(Vec::new()));
}
fn registered_pool() -> Option<AlkaneId> {
    let bytes = pool_registry_ptr().get().as_ref().clone();
    if bytes.is_empty() {
        None
    } else {
        AlkaneId::try_from(bytes).ok()
    }
}

// ---- mock reserves (test seed; production swaps this for a real pool read) --

pub fn seed_reserves(pool: &AlkaneId, r0: u128, r1: u128) {
    let mut bytes = vec![0u8; 32];
    bytes[0..16].copy_from_slice(&r0.to_le_bytes());
    bytes[16..32].copy_from_slice(&r1.to_le_bytes());
    let mut p = reserves_ptr(pool);
    p.set(Arc::new(bytes));
}
fn read_reserves(pool: &AlkaneId) -> Option<(u128, u128)> {
    let bytes = reserves_ptr(pool).get().as_ref().clone();
    if bytes.len() < 32 {
        return None;
    }
    let r0 = u128::from_le_bytes(bytes[0..16].try_into().ok()?);
    let r1 = u128::from_le_bytes(bytes[16..32].try_into().ok()?);
    Some((r0, r1))
}

// ---- per-block record hook --------------------------------------------------

/// Called once per block from `index_block`. No-op if no pool is registered.
/// Records cum[height] = cum[last] + spot_price(reserves), updates first/last.
pub fn record_observation(height: u32) -> Result<()> {
    let pool = match registered_pool() {
        Some(p) => p,
        None => return Ok(()),
    };
    let (r0, r1) = match read_reserves(&pool) {
        Some(v) => v,
        None => return Ok(()),
    };
    let price = spot_price_q64(r0, r1);
    let last = last_height_ptr(&pool).get_value::<u32>();
    let prev_cum = if last == 0 {
        let mut fh = first_height_ptr(&pool);
        fh.set_value::<u32>(height);
        0u128
    } else {
        cumulative_ptr(&pool).select_value(last).get_value::<u128>()
    };
    let cum = prev_cum.wrapping_add(price);
    let mut cp = cumulative_ptr(&pool).select_value(height);
    cp.set_value::<u128>(cum);
    let mut lh = last_height_ptr(&pool);
    lh.set_value::<u32>(height);
    Ok(())
}

// ---- read fn used by the precompile ----------------------------------------

/// TWAP over the last `window` blocks for `pool` (Q64.64). Reverts (Err) on
/// window==0, no observations, or insufficient history.
pub fn twap(pool: &AlkaneId, window: u32) -> Result<u128> {
    if window == 0 {
        return Err(anyhow!("twap: window must be > 0"));
    }
    let tip = last_height_ptr(pool).get_value::<u32>();
    if tip == 0 {
        return Err(anyhow!("twap: no observations for pool"));
    }
    let first = first_height_ptr(pool).get_value::<u32>();
    if tip < window || (tip - window) < first {
        return Err(anyhow!("twap: insufficient history"));
    }
    let base = tip - window;
    let cum_tip = cumulative_ptr(pool).select_value(tip).get_value::<u128>();
    let cum_base = cumulative_ptr(pool).select_value(base).get_value::<u128>();
    Ok(cum_tip.wrapping_sub(cum_base) / (window as u128))
}

// ---- test-visible getters ---------------------------------------------------

pub fn cumulative_at(pool: &AlkaneId, height: u32) -> u128 {
    cumulative_ptr(pool).select_value(height).get_value::<u128>()
}
pub fn last_height(pool: &AlkaneId) -> u32 {
    last_height_ptr(pool).get_value::<u32>()
}
