# Indexer-side TWAP precompile — working prototype

**What this proves:** an AMM TWAP can be served **indexer-side** in alkanes-rs. The indexer
computes a **fresh price accumulator every block**, and a native **`get_twap(window)` precompile**
returns the time-weighted average — callable **on-chain** by any contract. This makes a price
keeper, `poke` transactions, and on-chain ring buffers **redundant**, and makes oracle staleness
**moot** (the value is fresh every block, trustless, and manipulation-resistant).

This is a self-contained prototype in a fork of `kungfuflex/alkanes-rs`, proven by green tests
(`cargo test`) + CI. It is the indexer-side counterpart to the contract-side TWAP v1 (Ginko
`ginko-oracle-adapter`); the `get_price` seam in Ginko survives — it would simply read this
precompile instead of computing from poked checkpoints.

## How it works

Three native-Rust pieces in the `alkanes` crate (`crates/alkanes/src/twap.rs`,
`crates/alkanes/src/indexer.rs`, `crates/alkanes/src/vm/host_functions.rs`):

```
[pool reserves change]
        │  (once per block)
        ▼
index_block(h)  ──►  Protorune::index_block  ──►  twap::record_observation(h)
                                                     reads reserves (op97-style)
                                                     cum[h] = cum[h-1] + spot_price   (Q64.64, u128)
                                                     stores cum[h], last_height = h
        │  (later block)
        ▼
contract  ──staticcall──►  [800000000, 4, pool, window]   (the get_twap precompile, opcode 4 @ 8e8)
                                                     twap = (cum[tip] - cum[tip-window]) / window
                                                     tip = last committed height
        ▼
returndata  =  TWAP (Q64.64)
```

- **Accumulator (`twap::record_observation`)** runs once per block from `index_block`. It is a no-op
  unless a pool is registered, so existing indexing is unaffected. It computes a fresh accumulator
  even in blocks with no trades — that is the key property that removes the keeper and staleness.
- **Precompile (`get_twap`, opcode `4` at the magic address `800000000`)** is a new arm in
  `_handle_special_extcall`, reachable on-chain via `staticcall` (the same mechanism as the existing
  block-header / miner-fee precompiles). It reverts on insufficient history.
- **Manipulation-resistant by construction:** `get_twap` reads `tip = last committed height`, so a
  trade in the *current* block cannot move the value a consumer reads in that block.

## Run it

```bash
# build needs the pinned rustc 1.86 (rust-toolchain.toml); scope to -p alkanes
cargo test -p alkanes --target wasm32-unknown-unknown --features test-utils twap_precompile
```

Six `#[wasm_bindgen_test]` tests pass:

| Test | Proves |
|---|---|
| `test_spot_price_q64` | Q64.64 spot-price math |
| `test_record_and_twap_happy_path` | accumulator + TWAP equals an independent reference |
| `test_twap_insufficient_history_errors` | reverts on window=0 / too-large window / unknown pool |
| `test_hook_records_via_index_block` | the hook records an observation when a block is indexed |
| `test_get_twap_precompile_onchain` | a contract reads the TWAP **on-chain** via `staticcall` |
| `test_get_twap_precompile_reverts_on_insufficient_history` | the on-chain call reverts cleanly |

The on-chain tests use the existing prebuilt `alkanes-std-test` contract's `test_static_call`
(opcode 33) — **no contract rebuild** — exactly the `extcall → 8e8` path Ginko's `get_price` will use.

## Notable: a latent VM bug fixed along the way

The precompile error path (`extcall`, `crates/alkanes/src/vm/host_functions.rs`) returns **before**
`atomic.checkpoint()`. The previous code routed a precompile error to `_handle_extcall_abort(…, true)`,
which calls `rollback()` with no matching checkpoint → checkpoint-stack underflow → panic. This affects
**all** precompiles (0–3 too); the insufficient-history error test is just the first to exercise a
precompile error. Fixed by aborting the precompile branch with `rollback = false`. Successful
precompile calls are unchanged (existing `special_extcall` tests stay green).

## Scope / non-goals (prototype)

- **Reserves are seeded** into a mock pointer (`/twap/mock-reserves/<pool>`); `read_reserves` is the
  seam to swap for a real synth-pool read (op97 / pool storage). Single tracked pool for simplicity.
- **Accumulator is `u128` Q64.64** (alkanes-rs has no u256 `ByteView`); mock reserves are kept small
  to avoid overflow. Production would widen to u256 with UniV2-style wrapping (already `wrapping_*` here).
- Time unit = 1 block; reconciling the fixed-point scale / unit with the real DIESEL/frBTC pool
  (`2:77087`) op98 is a follow-up.
- Not wired into Ginko's `ginko-alkanes` yet (the `get_price` rewire is a follow-up), and not proposed
  upstream — this fork is the proof.

## Open question for the group

The `index_block` hook is **consensus-critical** (it changes indexed state for everyone running the
indexer). **Who builds/operates the precompile in production — us (fork → PR) or the indexer team?**
This prototype exists to make that decision concrete.
