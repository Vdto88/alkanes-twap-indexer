#[allow(unused_imports)]
use crate::tests::helpers::clear;
#[allow(unused_imports)]
use alkanes_support::id::AlkaneId;
use anyhow::Result;
#[allow(unused_imports)]
use metashrew_core::{println, stdio::{stdout, Write}};
use wasm_bindgen_test::wasm_bindgen_test;

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
