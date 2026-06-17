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
    crate::twap::register_pool(&pool);

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

#[wasm_bindgen_test]
fn test_twap_insufficient_history_errors() -> Result<()> {
    clear();
    let pool = AlkaneId { block: 2, tx: 77087 };
    crate::twap::register_pool(&pool);
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
    crate::twap::register_pool(&pool);
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
