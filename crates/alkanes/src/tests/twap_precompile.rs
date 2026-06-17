#[allow(unused_imports)]
use crate::tests::helpers::clear;
#[allow(unused_imports)]
use alkanes_support::id::AlkaneId;
use anyhow::Result;
#[allow(unused_imports)]
use metashrew_core::{println, stdio::{stdout, Write}};
use wasm_bindgen_test::wasm_bindgen_test;
use crate::index_block;
use protorune::test_helpers::create_block_with_coinbase_tx;
use crate::tests::helpers::{self as alkane_helpers, assert_return_context, assert_revert_context};
use crate::tests::std::alkanes_std_test_build;
use alkanes_support::cellpack::Cellpack;
use bitcoin::OutPoint;
use crate::utils::balance_pointer;
use metashrew_core::index_pointer::AtomicPointer;
use metashrew_support::index_pointer::KeyValuePointer;

/// DIESEL (token0, denominator) and frBTC (token1, numerator) ids for tests.
/// Arbitrary distinct ids; only their distinctness from the pool and each other matters.
fn test_tokens() -> (AlkaneId, AlkaneId) {
    (AlkaneId { block: 2, tx: 0 }, AlkaneId { block: 32, tx: 0 })
}

#[wasm_bindgen_test]
fn test_spot_price_q64() -> Result<()> {
    // equal reserves -> price == 1.0 in Q64.64 == 2^64
    assert_eq!(crate::twap::spot_price_q64(1_000_000, 1_000_000), 1u128 << 64);
    // r1/r0 = 1.5 -> 1.5 * 2^64
    assert_eq!(crate::twap::spot_price_q64(2_000_000, 3_000_000), (3u128 << 64) / 2);
    // r0 == 0 -> 0 (guard)
    assert_eq!(crate::twap::spot_price_q64(0, 5), 0);
    Ok(())
}

#[wasm_bindgen_test]
fn test_record_and_twap_happy_path() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);

    // Price series over heights 1..=6 (reserves kept small; r1 varies the price).
    let r0 = 1_000_000u128;
    let r1s = [1_000_000u128, 1_100_000, 1_200_000, 1_300_000, 1_400_000, 1_500_000];

    // Reference accumulator using the SAME integer formula.
    let mut ref_cum = [0u128; 7]; // ref_cum[h] for h in 1..=6
    let mut acc = 0u128;
    for (i, &r1) in r1s.iter().enumerate() {
        let h = (i as u32) + 1;
        crate::twap::seed_reserves(&pool, r0, r1);
        crate::twap::record_observation(h)?;
        acc = acc.wrapping_add((r1 << 64) / r0);
        ref_cum[h as usize] = acc;
    }

    // tip = 6, window = 5 -> average over heights 2..=6 = (cum[6]-cum[1]) / 5
    let window = 5u32;
    let expected = (ref_cum[6].wrapping_sub(ref_cum[1])) / (window as u128);
    let got = crate::twap::twap(&pool, window)?;
    assert_eq!(got, expected);

    // window = 1 -> just the last block's price = (cum[6]-cum[5]) / 1
    let expected1 = ref_cum[6].wrapping_sub(ref_cum[5]);
    assert_eq!(crate::twap::twap(&pool, 1)?, expected1);

    crate::twap::unregister_pool();
    Ok(())
}

// Proves twap's balance-sheet key matches the VM's real key. Writes balances
// via the production `balance_pointer` path (committed through an AtomicPointer),
// then drives the hook and checks it read exactly those reserves. Without this,
// seed_reserves + read_reserves would be self-consistent even on a wrong key.
#[wasm_bindgen_test]
fn test_read_reserves_matches_real_balance_pointer_key() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);

    let r0 = 1_234_567u128;
    let r1 = 7_654_321u128;

    // Write reserves through the REAL VM balance key, then commit to the base store.
    let mut atomic = AtomicPointer::default();
    balance_pointer(&mut atomic, &pool, &t0).set_value::<u128>(r0);
    balance_pointer(&mut atomic, &pool, &t1).set_value::<u128>(r1);
    atomic.commit();

    // The hook must read exactly those balances.
    crate::twap::record_observation(1)?;
    assert_eq!(
        crate::twap::cumulative_at(&pool, 1),
        crate::twap::spot_price_q64(r0, r1)
    );
    assert_eq!(crate::twap::last_height(&pool), 1);

    crate::twap::unregister_pool();
    Ok(())
}

#[wasm_bindgen_test]
fn test_twap_insufficient_history_errors() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);
    crate::twap::seed_reserves(&pool, 1_000_000, 1_000_000);
    crate::twap::record_observation(1)?; // only one observation (first=last=1)

    // window 0 -> error; window 5 (> available) -> error; no-tip pool -> error
    assert!(crate::twap::twap(&pool, 0).is_err());
    assert!(crate::twap::twap(&pool, 5).is_err());
    let other = AlkaneId { block: 9, tx: 9 };
    assert!(crate::twap::twap(&other, 1).is_err());

    crate::twap::unregister_pool();
    Ok(())
}

#[wasm_bindgen_test]
fn test_hook_records_via_index_block() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);
    crate::twap::seed_reserves(&pool, 1_000_000, 1_000_000); // price = 2^64

    let block = create_block_with_coinbase_tx(1);
    index_block(&block, 1)?;

    // The hook should have written cum[1] = spot_price_q64(1e6, 1e6) = 2^64,
    // and advanced last-height to 1.
    assert_eq!(crate::twap::cumulative_at(&pool, 1), 1u128 << 64);
    assert_eq!(crate::twap::last_height(&pool), 1);

    crate::twap::unregister_pool();
    Ok(())
}

// Seed a known accumulator directly (no hook), set tip, leave pool UNREGISTERED
// so the consumer's index_block does not perturb the series.
fn seed_series_unregistered(pool: &AlkaneId, r0: u128, r1s: &[u128]) -> [u128; 16] {
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(pool, &t0, &t1);
    let mut ref_cum = [0u128; 16];
    let mut acc = 0u128;
    for (i, &r1) in r1s.iter().enumerate() {
        let h = (i as u32) + 1;
        crate::twap::seed_reserves(pool, r0, r1);
        crate::twap::record_observation(h).unwrap();
        acc = acc.wrapping_add((r1 << 64) / r0);
        ref_cum[(i + 1)] = acc;
    }
    crate::twap::unregister_pool(); // hook is now inert during the consumer block
    ref_cum
}

#[wasm_bindgen_test]
fn test_get_twap_precompile_onchain() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let r0 = 1_000_000u128;
    let r1s = [1_000_000u128, 1_100_000, 1_200_000, 1_300_000, 1_400_000, 1_500_000];
    let ref_cum = seed_series_unregistered(&pool, r0, &r1s); // tip = 6, first = 1

    let window = 5u32;
    let expected = (ref_cum[6].wrapping_sub(ref_cum[1])) / (window as u128);

    // Consumer = alkanes-std-test; opcode 33 = test_static_call(target, inputs).
    // WIT dispatch: [opcode, target.block, target.tx, list_len, elem0, elem1, ...].
    // list<u64> decodes as: length-prefix then elements, all as u128.
    // Inner precompile cellpack inputs will be [pool.block, pool.tx, window].
    let cp = Cellpack {
        target: AlkaneId { block: 1, tx: 0 }, // deploy-and-call (like special_extcall)
        inputs: vec![33, 800000000, 4, 3, pool.block, pool.tx, window as u128],
    };
    let test_block = alkane_helpers::init_with_multiple_cellpacks_with_tx(
        [alkanes_std_test_build::get_bytes()].into(),
        [cp].into(),
    );
    index_block(&test_block, 0)?;

    let outpoint = OutPoint { txid: test_block.txdata[1].compute_txid(), vout: 3 };
    assert_return_context(&outpoint, |trace_response| {
        let got = u128::from_le_bytes(trace_response.inner.data[0..16].try_into()?);
        println!("on-chain TWAP = {}, expected = {}", got, expected);
        assert_eq!(got, expected);
        Ok(())
    })?;
    Ok(())
}

#[wasm_bindgen_test]
fn test_get_twap_precompile_reverts_on_insufficient_history() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let r0 = 1_000_000u128;
    let r1s = [1_000_000u128, 1_100_000]; // only 2 observations (tip = 2, first = 1)
    let _ = seed_series_unregistered(&pool, r0, &r1s);

    // window = 5 > available -> precompile returns Err -> extcall aborts -> revert.
    // WIT list<u64> encoding: length prefix then elements.
    let cp = Cellpack {
        target: AlkaneId { block: 1, tx: 0 },
        inputs: vec![33, 800000000, 4, 3, pool.block, pool.tx, 5u128],
    };
    let test_block = alkane_helpers::init_with_multiple_cellpacks_with_tx(
        [alkanes_std_test_build::get_bytes()].into(),
        [cp].into(),
    );
    index_block(&test_block, 0)?;

    let outpoint = OutPoint { txid: test_block.txdata[1].compute_txid(), vout: 3 };
    assert_revert_context(&outpoint, "insufficient history")?;
    Ok(())
}

// Reserves change each block (simulated swaps), r1 non-monotonic. The accumulator
// and twap must match a hand-computed reference over the changing spot prices.
#[wasm_bindgen_test]
fn test_record_tracks_changing_reserves() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);

    let r0 = 1_000_000u128;
    let r1s = [1_000_000u128, 3_000_000, 1_500_000, 2_000_000]; // up, down, up

    let mut ref_cum = [0u128; 5];
    let mut acc = 0u128;
    for (i, &r1) in r1s.iter().enumerate() {
        let h = (i as u32) + 1;
        crate::twap::seed_reserves(&pool, r0, r1); // re-seed = reserves moved via swaps
        crate::twap::record_observation(h)?;
        acc = acc.wrapping_add((r1 << 64) / r0);
        ref_cum[h as usize] = acc;
    }

    // tip = 4; window = 3 -> average over heights 2..=4 = (cum[4]-cum[1]) / 3
    let window = 3u32;
    let expected = (ref_cum[4].wrapping_sub(ref_cum[1])) / (window as u128);
    assert_eq!(crate::twap::twap(&pool, window)?, expected);

    crate::twap::unregister_pool();
    Ok(())
}

// A drained side (reserve == 0) must NOT be recorded: tip stays put, no cum written.
#[wasm_bindgen_test]
fn test_zero_reserve_skips_observation() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    let (t0, t1) = test_tokens();
    crate::twap::register_pool(&pool, &t0, &t1);

    // Block 1: healthy reserves -> recorded, tip advances to 1.
    crate::twap::seed_reserves(&pool, 1_000_000, 1_000_000);
    crate::twap::record_observation(1)?;
    assert_eq!(crate::twap::last_height(&pool), 1);
    let cum1 = crate::twap::cumulative_at(&pool, 1);

    // Block 2: one side drained (r1 == 0) -> must be skipped.
    crate::twap::seed_reserves(&pool, 1_000_000, 0);
    crate::twap::record_observation(2)?;
    assert_eq!(crate::twap::last_height(&pool), 1); // tip did NOT advance
    assert_eq!(crate::twap::cumulative_at(&pool, 2), 0); // nothing written at h=2
    assert_eq!(crate::twap::cumulative_at(&pool, 1), cum1); // h=1 untouched

    crate::twap::unregister_pool();
    Ok(())
}
