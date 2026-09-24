# VeriGov prototype

A runnable local prototype of **VeriGov** (see `paper/ms.tex`): a DAO-governed software
release log with reproducible-build attestations and crypto-economic slashing,
implemented as a **Polkadot SDK solochain** plus off-chain tooling.

It reproduces the paper's worked example — the `acme-utils v1.2.3` release
(Section 5.6) — and its failure case (compromised Build Service → counter-attestation
→ negligence slash), and prints `PASS`/`FAIL` for both from one script.

```
$ ./scripts/run-demo.sh
...
  [PASS] Scenario A: honest release accepted, enacted, verified and installed
  [PASS] Scenario B: tampered build caught, Build Service slashed, release blocked

ALL PASS
```

## Layout

```
Cargo.toml                   Polkadot SDK solochain template workspace (stable2503) + pallet-verigov
pallets/verigov/             the VeriGov FRAME pallet (lib.rs, mock.rs, tests.rs)
runtime/                     solochain runtime with Verigov at pallet index 7; VGOV = pallet-balances
node/                        unchanged template node (Aura + GRANDPA, --dev mode)
.cargo/config.toml           wasm link flag for Rust >= 1.98 (see below)
tools/                       off-chain tooling (Node, @polkadot/api)
  builder/build.js           Build Service: hermetic toy build → signed A_build → artifact store
  client/client.js           VeriGov Client: poll Release Log → fetch → verify → install
  demo/demo.js               drives Scenario A and Scenario B, prints PASS/FAIL
  lib/common.js              hashing, artifact store, chain helpers
fixtures/acme-utils/         toy source tree that gets "compiled"
scripts/run-demo.sh          build if needed, start a --dev node, run the demo, stop the node
paper/                       the VeriGov paper (ms.tex, references.bib, PRIMEarxiv.sty, build products)
docs/                        the solochain template's original README and Rust setup notes
data/store/<sha256-hex>      local content-addressed artifact store (mimics IPFS; created at runtime)
install/                     where the client installs verified releases (created at runtime)
```

## Prerequisites

- Rust stable with the wasm target: `rustup target add wasm32v1-none` (the template also
  accepts `wasm32-unknown-unknown`). Tested with Rust 1.98 on macOS/arm64.
- `protoc`, `clang` (standard Substrate build deps).
- Node.js ≥ 20 and npm.
- No IPFS daemon, relay chain, or parachain is needed. This is a solo chain in `--dev` mode.

> Rust ≥ 1.98 no longer turns undefined `extern "C"` symbols into wasm imports implicitly,
> which breaks linking of every Substrate runtime's host functions. `.cargo/config.toml`
> sets `WASM_BUILD_RUSTFLAGS="-C link-arg=--import-undefined"` so `substrate-wasm-builder` links
> correctly; nothing to do manually.

## Exact commands

```bash
# 1. Build the node + runtime (10–20 min first time; ~1 min incremental)
cargo build --release

# 2. Pallet unit tests (20 tests: genesis, proposals, Eq. 1 quorums, enactment, slashing)
cargo test -p pallet-verigov

# 3. Off-chain tool dependencies
(cd tools && npm install)

# 4. Everything at once: start a throwaway dev node, run both scenarios, stop the node
./scripts/run-demo.sh
```

Or run the pieces by hand against your own node:

```bash
# terminal 1: a fresh dev chain (Alice is sudo)
target/release/solochain-template-node --dev --tmp

# terminal 2
node tools/demo/demo.js                                   # both scenarios, PASS/FAIL, exit code 0/1
node tools/builder/build.js --version 1.2.3 --commit c0ff33d          # honest build → data/store
node tools/builder/build.js --version 1.2.4 --commit d00d1e5 --inject # compromised build
node tools/client/client.js --project 0 [--watch]        # verify + install blessed releases
```

Environment: `VERIGOV_WS` (default `ws://127.0.0.1:9944`), `VERIGOV_RPC_PORT` for the script.
The demo is re-runnable against the same node; each run registers a new project id.

## What the demo does

Cast (all sr25519 dev-derived accounts): Core Developers **Alice, Bob, Carol, Dan, Eve**
(`//Alice` … `//Eve`, 10,000 VGOV stake each), Security Auditors **Foo Audit** and
**fuzz.io** (`//FooAudit`, `//FuzzIo`, 10,000 each), one **Build Service** (`//BuildService`,
50,000). Policy: τ_CoreDev = 3/5, τ_SecAuditor = 1/2 (Eq. 1), 1,000 VGOV proposal bond,
σ_neg = 50 %, σ_mal = 100 %, Δ_max = 1000 blocks, voting period 100 blocks, enactment delay
2 blocks (the paper's 7 days / 24 h shrunk for a 6 s block time).

1. **ProjectGenesis** (`sudo(verigov.projectGenesis)`) registers stakeholders, roles, quorums,
   slashing parameters and escrows every stakeholder's initial stake as a `VGOV` hold.
2. **Build (off-chain)**: `tools/builder` deterministically "compiles" `fixtures/acme-utils`
   into a self-contained shell script, computes `source_commit_hash` (`c0ff33d` padded),
   `source_tree_hash`, `build_environment_hash` (digest of the builder itself),
   `output_binary_hash` (sha256), signs the 128-byte concatenation with the Build Service's
   sr25519 key, and pins binary + `A_build` JSON to `data/store/<sha256>`.
3. **NewRelease**: Alice submits `verigov.newRelease(project, "1.2.3", A_build, cid, sig)`.
   The runtime checks she is a Core Developer, that `A_build.builder` is a registered Build
   Service, verifies the signature over the attestation, holds the 1,000 VGOV bond and records
   her implicit aye.
4. **Vote**: Bob, Carol, Dan, Foo Audit, fuzz.io vote aye (Eve abstains). Weight = stake.
5. **Tally**: after Foo Audit's vote every required role meets its quorum (4/5 ≥ 3/5,
   2/2 ≥ 1/2) → `ProposalApproved`; two blocks later `on_initialize` appends the blessed
   `output_binary_hash` to the **Official Release Log** and releases the bond.
6. **Client**: `tools/client` reads `ReleaseLog`, fetches `A_build` by CID (content-hash
   checked), checks it binds the same commit and binary hash as the log entry, checks the
   builder is an on-chain Build Service and its sr25519 signature verifies, fetches the binary
   by hash, verifies sha256, installs to `install/acme-utils-1.2.3`, and runs it.
7. **Failure path**: the builder is run with `--inject` for `1.2.4`; the tampered hash is
   attested and proposed. Bob rebuilds honestly, gets a different hash, and calls
   `verigov.submitCounterAttestation(proposal, rebuilt_hash)`. The chain verifies the
   discrepancy against the attested hash, burns σ_neg = 50 % of the Build Service's held stake
   (`BuildServiceSlashed`), rejects the proposal, returns Alice's bond, and leaves the release
   log untouched, so the client installs nothing.

## The pallet (`pallets/verigov`)

Calls: `project_genesis` (root), `new_release`, `vote`, `submit_counter_attestation`,
`close_expired`. Storage: `Projects`, `Stakeholders` (the Stakeholder Registry), `RoleStake`,
`Proposals`, `Votes`, `Tallies`, `ReleaseLog`/`ReleaseCount` (append-only Official Release Log),
`CounterAttestations`, `PendingEnactments`. Hold reasons: `Stake`, `ProposalBond`.

Acceptance is exactly Eq. 1 of the paper: for every required role
`Σ stake(aye voters in r) / Σ stake(r) ≥ τ_r`, evaluated in integer arithmetic. Votes may be
changed until enactment; the tally standing at the enactment block is what gets enacted, and a
vote change that breaks a quorum un-schedules enactment.

Design choices worth knowing:

- `VGOV` is `pallet-balances` via the `fungible` traits; stake and bonds are holds, slashing is
  `burn_held`.
- `A_build` is signed with the chain's `MultiSignature`, so the Build Service is an ordinary
  account and the same signature is verified on-chain (`new_release`) and by the client.
- Root (sudo) stands in for the "founding maintainer set" signature on `ProjectGenesis`.
- A counter-attestation is accepted from any staked stakeholder of the project other than the
  builder; the chain checks only that the reported hash differs from the attested one (the
  paper's "self-evident" negligence condition). A challenger bond is a natural next step.

Not implemented (out of scope for the demo): meta-governance (add/remove stakeholder,
parameter updates), the malice/dispute track (σ_mal and Δ_max are stored but unused),
emergency-patch track, rewards, benchmarked weights (calls use fixed weights).

## Tests

```bash
cargo test -p pallet-verigov
```

20 unit tests with a mock runtime (`mock.rs`, `u64` accounts, `pallet-balances`), covering
genesis validation and escrow, proposer/builder/signature checks, the paper's worked example,
per-role quorums and stake weighting, re-voting and approval revocation, expiry, and the
counter-attestation/slash path (including cancelling a scheduled enactment and repeated slashes).
`tools/demo/demo.js` is the integration test against a live node.
