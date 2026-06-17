# alkanes-twap-indexer — indexer-side TWAP for ALKANES (prototype fork)

> **This is a focused fork of [`kungfuflex/alkanes-rs`](https://github.com/kungfuflex/alkanes-rs)** (rev `888f4fe6`) that adds **one** thing: an **indexer-side TWAP** for an AMM pair, served on-chain by a native `get_twap(window)` precompile. The upstream alkanes-rs README is preserved below the divider.

**What it proves:** an AMM TWAP can be served **indexer-side** in alkanes-rs. The indexer computes a **fresh price accumulator every block**, and a native **`get_twap(window)` precompile** returns the time-weighted average — callable **on-chain** by any contract. This makes a price keeper, `poke` transactions, and on-chain ring buffers **redundant**, and makes oracle staleness **moot** (the value is fresh every block, trustless, and manipulation-resistant).

It is the indexer-side counterpart to a contract-side TWAP oracle (the usual `poke` + keeper + ring-buffer design); a lending protocol's `get_price` seam survives — it would simply read this precompile instead of computing from poked checkpoints. Proven by green tests (`cargo test`) + CI.

## How it works

Three native-Rust pieces in the `alkanes` crate (`crates/alkanes/src/twap.rs`, `crates/alkanes/src/indexer.rs`, `crates/alkanes/src/vm/host_functions.rs`):

```
[pool reserves change]
        │  (once per block)
        ▼
index_block(h)  ──►  Protorune::index_block  ──►  twap::record_observation(h)
                                                     reads live reserves from the VM balance-sheet
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

- **Accumulator (`twap::record_observation`)** runs once per block from `index_block`. It is a no-op unless a pool is registered, so existing indexing is unaffected. It computes a fresh accumulator even in blocks with no trades — that is the key property that removes the keeper and staleness.
- **Reserves are read live from the VM balance-sheet** (`/alkanes/<token>/balances/<pool>`) — a pool's reserve == the balance it holds in each token, the same key the VM's `balance_pointer` builds. So the accumulator weights the pool's *true end-of-block reserves*, not a seeded value. `register_pool(pool, token0, token1)` picks the pair and orientation (token0 = denominator, token1 = numerator → for DIESEL/frBTC, TWAP = frBTC per DIESEL). A block where either side is drained (reserve 0) is skipped, so a zero/∞ spot never poisons the average.
- **Precompile (`get_twap`, opcode `4` at the magic address `800000000`)** is a new arm in `_handle_special_extcall`, reachable on-chain via `staticcall` (the same mechanism as the existing block-header / miner-fee precompiles). It reverts on insufficient history.
- **Manipulation-resistant by construction:** `get_twap` reads `tip = last committed height`, so a trade in the *current* block cannot move the value a consumer reads in that block.

## Run it

```bash
# build needs the pinned rustc 1.86 (rust-toolchain.toml); scope to -p alkanes
cargo test -p alkanes --target wasm32-unknown-unknown --features test-utils twap_precompile
```

Nine `#[wasm_bindgen_test]` tests pass:

| Test | Proves |
|---|---|
| `test_spot_price_q64` | Q64.64 spot-price math |
| `test_record_and_twap_happy_path` | accumulator + TWAP equals an independent reference |
| `test_twap_insufficient_history_errors` | reverts on window=0 / too-large window / unknown pool |
| `test_hook_records_via_index_block` | the hook records an observation when a block is indexed |
| `test_get_twap_precompile_onchain` | a contract reads the TWAP **on-chain** via `staticcall` |
| `test_get_twap_precompile_reverts_on_insufficient_history` | the on-chain call reverts cleanly |
| `test_read_reserves_matches_real_balance_pointer_key` | the balance-sheet key matches the VM's real `balance_pointer` key (writes via the production path, reads via the hook) |
| `test_zero_reserve_skips_observation` | a drained side (reserve 0) is skipped: tip doesn't advance, no cumulative written |
| `test_record_tracks_changing_reserves` | accumulator tracks non-monotonic reserves (up/down/up) vs an independent reference |

The on-chain tests use the existing prebuilt `alkanes-std-test` contract's `test_static_call` (opcode 33) — **no contract rebuild** — exactly the `extcall → 8e8` path a consumer's `get_price` would use.

## Notable: a latent VM bug fixed along the way

The precompile error path (`extcall`, `crates/alkanes/src/vm/host_functions.rs`) returns **before** `atomic.checkpoint()`. The previous code routed a precompile error to `_handle_extcall_abort(…, true)`, which calls `rollback()` with no matching checkpoint → checkpoint-stack underflow → panic. This affects **all** precompiles (0–3 too); the insufficient-history error test is just the first to exercise a precompile error. Fixed by aborting the precompile branch with `rollback = false`. Successful precompile calls are unchanged (existing `special_extcall` tests stay green).

## Scope / non-goals (prototype)

- **Reserves are read live** from the VM balance-sheet (`/alkanes/<token>/balances/<pool>`) — a pool's reserve == the balance it holds in each token, the same key the VM's `balance_pointer` builds. `register_pool(pool, token0, token1)` records the pair; orientation token0=DIESEL (denominator), token1=frBTC (numerator) → TWAP = frBTC per DIESEL. Single tracked pool for simplicity.
- **Accumulator is `u128` Q64.64** (alkanes-rs has no u256 `ByteView`); reserves must stay < 2^64 (holds for BTC-scale sat reserves) to avoid overflow in the `r1 << 64` spot step. Production would widen to u256 with UniV2-style wrapping (already `wrapping_*` here).
- Time unit = 1 block. The accumulator is computed **here** from live reserves, so it does not depend on the pool's op98 cumulative — wiring into the live DIESEL/frBTC pool (`2:77087`) is a follow-up.
- Not wired into a consumer protocol yet (the `get_price` rewire is a follow-up), and the precompile isn't yet wired into the real indexer init (`register_pool`) — both deliberate follow-ups.

## Open question for the indexer team

The `index_block` hook is **consensus-critical** (it changes indexed state for everyone running the indexer — it is *not* a contract you deploy in a transaction). **Who builds & operates the precompile in production — us (fork → PR) or the indexer team?** This prototype exists to make that decision concrete.

---

<sub>↓ upstream `kungfuflex/alkanes-rs` README below ↓</sub>

# alkanes-rs

![Tests](https://img.shields.io/github/actions/workflow/status/AssemblyScript/assemblyscript/test.yml?branch=main&label=test&logo=github)
![Publish](https://img.shields.io/github/actions/workflow/status/AssemblyScript/assemblyscript/publish.yml?branch=main&label=publish&logo=github)

**The ALKANES specification is hosted at** 👉🏻👉🏼👉🏽👉🏾👉🏿 [https://github.com/kungfuflex/alkanes-rs/wiki](https://github.com/kungfuflex/alkanes-rs/wiki)

This repository hosts Rust sources for the ALKANES metaprotocol. The indexer for ALKANES can be built as the top level crate in the monorepo, with builds targeting wasm32-unknown-unknown, usable within the METASHREW indexer stack.

ALKANES is a metaprotocol designed to support an incarnation of DeFi as we have traditionally seen it, but designed specifically for the Bitcoin consensus model and supporting structures.
The ALKANES genesis block is 880000. Builders are encouraged to test on regtest using the docker-compose environment at [https://github.com/kungfuflex/alkanes](https://github.com/kungfuflex/alkanes)

A signet RPC will be available on https://signet.sandshrew.io

Join ALKANES / metashrew discussion on the SANDSHREW サンド Discord.

#### NOTE: ALKANES does not have a network token

Protocol fees are accepted in terms of Bitcoin and compute is metered with the wasmi fuel implementation, for protection against DoS.

## Software Topology

This repository is a pure Rust implementation, built entirely for a WASM target and even tested within the WASM test runner `wasm-bindgen-test-runner`.

The top level crate in the monorepo contains sources for the ALKANES indexer, built for the METASHREW environment.

ALKANES is designed and implemented as a subprotocol of runes, one which is protorunes compatible. In order to encapsulate the behavior of protorunes for a Rust build system, a Rust implementation of protorunes generics is contained in the monorepo in `crates/protorune`.

For information on protorunes, refer to the specification hosted at:

[https://github.com/kungfuflex/protorune/wiki](https://github.com/kungfuflex/protorune/wiki)

The indexer stack used to synchronize the state of the metaprotocol and offer an RPC to consume its data and features is METASHREW. METASHREW is started with a WASM binary of the indexer program, produced with a normal build of this repository as `alkanes.wasm`.

Bindings to the METASHREW environment are available in `crates/metashrew`.

Sources needed to build both metashrew and protorunes meant to be shared with builds of individual alkanes or the generic alkanes-runtime bindings are factored out into `crates/metashrew-support` and `crates/protorune-support` such that they can be imported into an alkane build without the metashrew import definitions leaking in and generating import statements for the METASHREW environment.

In this way, all crates with a `-support` suffix can be imported into any Rust project since they do not depend on a specific environment or `wasm-bindgen`.

This design is permissive enough for this monorepo to host `alkanes-runtime`, which is a complete set of bindings for building alkane smart contracts to a WASM format, suitable for deployment within the witness envelope of a Bitcoin transaction.

Boilerplate for various alkanes are included and prefixed with `alkanes-std-` and placed in the `alkanes/` directory. Pre-built WASM files for all alkanes are committed to the repository in `crates/alkanes/src/tests/std/wasm/` and are used by the test suite.

## Building

ALKANES indexer is built with the command:

```sh
cargo build --release
```

This will build the `alkanes.wasm` indexer binary at `target/wasm32-unknown-unknown/release/alkanes.wasm`.

### Building Standard Alkanes

The standard alkanes (prefixed with `alkanes-std-`) have pre-built WASM files committed to the repository. To rebuild them (only needed if you modify the alkane source code), run:

```sh
./scripts/build-std.sh
```

This script will:
- Build all alkanes in `alkanes/` to WASM
- Generate network-specific builds (bellscoin, luckycoin, mainnet, fractal, regtest, testnet) for alkanes that require them
- Place the WASM files in `crates/alkanes/src/tests/std/wasm/`
- Regenerate `crates/alkanes/src/tests/std/mod.rs` with the appropriate module declarations

The WASM files are platform-independent and are committed to the repository so developers don't need to rebuild them unless modifying the alkane source code

## Indexing

Refer to the METASHREW documentation for descriptions of the indexer stack used for ALKANES.

[https://github.com/sandshrewmetaprotocols/metashrew](https://github.com/sandshrewmetaprotocols/metashrew)

A sample command may look like:

```sh
~/metashrew/target/release/rockshrew-mono --daemon-rpc-url http://localhost:8332 --auth bitcoinrpc:bitcoinrpc --db-path ~/.metashrew --indexer ~/alkanes-rs/target/wasm32-unknown-unknown/release/alkanes.wasm --start-block 880000 --host 0.0.0.0 --port 8080 --cors '*'
```

### Testing

#### Prerequisites

If you encounter issues with `wasm-bindgen-test-runner`, install the correct version:

```sh
cargo install -f wasm-bindgen-cli --version 0.2.100
```

#### Running ALKANES Tests

The alkanes crate tests run in WebAssembly using `wasm-bindgen-test`. There are two ways to run them:

**Option 1: From the alkanes package directory (recommended)**

```sh
cd crates/alkanes
cargo test --lib
```

The package-level `.cargo/config.toml` automatically sets the target to `wasm32-unknown-unknown`.

**Option 2: From the workspace root with explicit target**

```sh
cargo test -p alkanes --target wasm32-unknown-unknown --lib
```

#### Running Other Tests

To test a specific crate (non-WASM tests):

```sh
cargo test -p [CRATE]
```

Example:

```sh
cargo test --features test-utils -p protorune
```

#### Unit Testing (Native Rust)

Some crates have unit tests that run on native Rust (not WASM). For these, you may need to specify your target architecture:

- Macbook Intel x86: `x86_64-apple-darwin`
- Macbook Apple Silicon: `aarch64-apple-darwin`
- Ubuntu 20.04 LTS: `x86_64-unknown-linux-gnu`

```sh
cargo test -p protorune --target TARGET
```

### Authors

- flex
- v16
- butenprks
- clothic
- m3

## Quick Usage Examples

### Wallet Operations

```bash
# Create a new wallet
alkanes wallet create

# Get addresses
alkanes wallet addresses

# Check balance
alkanes wallet balance

# Send Bitcoin
alkanes wallet send bc1p... 10000 --fee-rate 600 -y
```

### Alkanes Operations

```bash
# Wrap BTC to frBTC
alkanes alkanes wrap-btc 100000 --from "p2tr:0" --mine -y

# Execute an alkanes contract
alkanes alkanes execute \
  --inputs "B:10000" \
  --to "bc1p..." \
  --protostones "[32,0,77]" \
  -y

# Get balance
alkanes alkanes getbalance --address "bc1p..."

# Inspect a contract
alkanes alkanes inspect <outpoint> --disasm
```

### BRC20-Prog Operations

```bash
# Deploy a smart contract
alkanes brc20-prog deploy-contract ./out/MyContract.sol/MyContract.json \
  --from "p2tr:0" --mine -y

# Call a contract function
alkanes brc20-prog transact \
  --address 0x1234... \
  --signature "transfer(address,uint256)" \
  --calldata "0x5678...,1000" \
  --from "p2tr:0" -y

# Wrap BTC and execute
alkanes brc20-prog wrap-btc 100000 \
  --target 0xABCD... \
  --signature "deposit()" \
  --calldata "" \
  --from "p2tr:0" -y
```

## Documentation

Comprehensive documentation is available in the [`docs/`](./docs) directory:

### Core Documentation
- **[Documentation Index](./docs/README.md)** - Complete documentation structure
- **[Getting Started](./docs/quickstart.md)** - Quick start guide
- **[CLI Usage](./docs/cli-usage.md)** - Complete CLI reference

### Protocol Features
- **[BRC20-Prog Guide](./docs/cli/brc20-prog.md)** - BRC20 programmable contracts
- **[Wrap-BTC Feature](./docs/features/wrap-btc.md)** - Wrapping BTC to frBTC
- **[External Signing](./docs/features/external-signing.md)** - Address-only mode and external key signing
- **[Transaction Broadcasting](./docs/features/transaction-broadcasting.md)** - All broadcast options (Slipstream, Rebar, etc.)
- **[Rebar Shield](./docs/features/rebar-shield.md)** - Private relay with MEV protection

### Development
- **[Architecture](./docs/architecture/overview.md)** - System design and components
- **[Crates Reference](./docs/crates/)** - Detailed crate documentation
- **[Developer Guide](./docs/dev/building-alkanes.md)** - Building alkane contracts
- **[Examples](./docs/examples/)** - Usage examples and patterns
- **[Helper Scripts](./scripts/README.md)** - Transaction building and broadcasting scripts

For detailed API documentation and protocol specifications, see:
- [Alkanes Wiki](https://github.com/kungfuflex/alkanes-rs/wiki) - Protocol specification
- [Protorune Spec](https://github.com/kungfuflex/protorune/wiki) - Protorune protocol
- [Metashrew](https://github.com/sandshrewmetaprotocols/metashrew) - Indexer stack

### License

MIT
