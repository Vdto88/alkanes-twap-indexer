#![allow(unused_imports)]
//! Indexer-side TWAP (v2 prototype): per-block price accumulator + read function.
//!
//! Storage (metashrew IndexPointer):
//!   /twap/pool                       -> registered pool AlkaneId bytes (single pool)
//!   /twap/token0, /twap/token1       -> registered pool's token ids (denom, numer)
//!   reserves read live from /alkanes/<token>/balances/<pool> (VM balance-sheet)
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
// global, paired with pool_registry_ptr (single tracked pool)
fn token0_ptr() -> IndexPointer {
    IndexPointer::from_keyword("/twap/token0")
}
fn token1_ptr() -> IndexPointer {
    IndexPointer::from_keyword("/twap/token1")
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

/// Balance-sheet key: the balance of `token` held by `pool`. Byte-for-byte the
/// same key the VM's `balance_pointer` (utils.rs) builds, minus the inventory
/// side-effect (we only read).
fn pool_balance_ptr(token: &AlkaneId, pool: &AlkaneId) -> IndexPointer {
    IndexPointer::from_keyword("/alkanes/")
        .select(&token.clone().into())
        .keyword("/balances/")
        .select(&pool.clone().into())
}
fn pool_balance(token: &AlkaneId, pool: &AlkaneId) -> u128 {
    pool_balance_ptr(token, pool).get_value::<u128>()
}

// ---- registry (single tracked pool) ----------------------------------------

pub fn register_pool(pool: &AlkaneId, token0: &AlkaneId, token1: &AlkaneId) {
    let mut p = pool_registry_ptr();
    p.set(Arc::new(pool.clone().into()));
    let mut t0 = token0_ptr();
    t0.set(Arc::new(token0.clone().into()));
    let mut t1 = token1_ptr();
    t1.set(Arc::new(token1.clone().into()));
}
pub fn unregister_pool() {
    let mut p = pool_registry_ptr();
    p.set(Arc::new(Vec::new()));
    let mut t0 = token0_ptr();
    t0.set(Arc::new(Vec::new()));
    let mut t1 = token1_ptr();
    t1.set(Arc::new(Vec::new()));
}
fn registered_pool() -> Option<AlkaneId> {
    let bytes = pool_registry_ptr().get().as_ref().clone();
    if bytes.is_empty() {
        None
    } else {
        AlkaneId::try_from(bytes).ok()
    }
}
fn registered_tokens() -> Option<(AlkaneId, AlkaneId)> {
    let t0 = token0_ptr().get().as_ref().clone();
    let t1 = token1_ptr().get().as_ref().clone();
    if t0.is_empty() || t1.is_empty() {
        return None;
    }
    Some((AlkaneId::try_from(t0).ok()?, AlkaneId::try_from(t1).ok()?))
}

// ---- test seed helpers (write the REAL balance-sheet keyspace) --------------

pub fn seed_pool_balance(token: &AlkaneId, pool: &AlkaneId, amount: u128) {
    let mut p = pool_balance_ptr(token, pool);
    p.set_value::<u128>(amount);
}
pub fn seed_reserves(pool: &AlkaneId, r0: u128, r1: u128) {
    if let Some((token0, token1)) = registered_tokens() {
        seed_pool_balance(&token0, pool, r0);
        seed_pool_balance(&token1, pool, r1);
    }
}

// ---- reserve read (production: live balance-sheet) --------------------------

fn read_reserves(pool: &AlkaneId) -> Option<(u128, u128)> {
    let (token0, token1) = registered_tokens()?;
    let r0 = pool_balance(&token0, pool);
    let r1 = pool_balance(&token1, pool);
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
