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

## The paper

*VeriGov: A Crypto-Economic Protocol for Decentralized Software Supply Chain Security*
(Nikhil Ranjan; source in `paper/ms.tex`, PDF in `paper/ms.pdf`).

### Problem

Open-source software is critical infrastructure, yet the infrastructure that distributes it is
centralized: npm, PyPI, GitHub and a project's own build server are single points of trust.
The paper draws on a series of incidents to show that authenticating *who* published a package
is no longer a sufficient security model:

- **`rand-user-agent` (2025)** — a stolen npm automation token published malicious versions with
  no corresponding source commit. Registry checksums verified the malware perfectly.
- **`node-ipc` (2022)** — the legitimate, fully authenticated maintainer shipped destructive
  "protestware". Identity was correct; intent was not.
- **`xz-utils` (CVE-2024-3094)** — a multi-year social-engineering campaign earned a maintainer
  seat and smuggled an SSH backdoor inside binary test fixtures, invisible to source review.
- **SolarWinds (2020)** — a compromised build system injected a backdoor into signed, legitimate
  looking updates while the source repository stayed clean.

From these the paper distills two formal gaps:

- **Provenance Integrity (P_I)** — for binary *B* and source commit *c* there should exist a
  verifiable attestation `A_build` binding `H(S_c)` to `H(B)`. Checksums prove the bytes you got
  are the bytes uploaded; they say nothing about where the bytes came from.
- **Release Authorization Integrity (R_A)** — a release should be valid only with ≥ *m* valid
  signatures from *n* distinct authorized stakeholders. Today *m* = 1 almost everywhere, so one
  stolen credential, one compromised build box, or one malicious insider is enough.

### Why existing tools are not enough

The paper reviews the state of the art and finds each piece solves part of the problem:

| Framework | What it secures | What it leaves open |
|---|---|---|
| Checksums / GPG | transit integrity, single-signer identity | provenance, collective authorization |
| **TUF** | compromise-resilient *delivery* via Root/Targets/Snapshot/Timestamp roles | who controls Root and how release decisions are made is off-band; no economic accountability |
| **Sigstore** | keyless, transparent signer identity (Fulcio/Rekor/Cosign) | policy like "3 of 5 core devs" is social, not enforced; no penalty for a correctly identified malicious insider |
| **Reproducible / verified builds** | bit-for-bit source→binary link | *what* to build and release is a governance question outside their scope |
| **in-toto** | multi-step supply-chain layouts | static key set, off-chain policy, no stake |
| **CHAINIAC** | decentralized, collectively signed update transparency | fixed approver set, cryptographic incentives only, policy lives in the client not the ledger |

VeriGov positions itself as the integrating layer: authorization by on-chain collective
governance, provenance by build attestation, identity by public key, delivery by a verifying
client — plus an economic layer none of the above have.

### Architecture

Three components (Figure 1 of the paper):

1. **VeriGov Client** — a minimal binary on the end-user's machine; its first install is the
   only out-of-band trust step. It connects to the ledger, reads release metadata and governance
   outcomes, fetches binaries from the artifact store, verifies hash and provenance against
   on-chain data, and installs.
2. **VeriGov DAO & Ledger** — assumed to be a Substrate chain. Stores the *Stakeholder
   Registry* (role → public keys), *Release Proposals* (version, `source_commit_hash`, pointer to
   `A_build`), *Votes*, the append-only *Official Release Log* of blessed binary hashes, and the
   `VGOV` token logic. Large artifacts are never stored on-chain.
3. **Distributed Artifact Store** — content-addressed storage (IPFS) for binaries and
   attestations; the ledger holds only the hashes.

**The build attestation `A_build`** is produced by a hermetic (network-isolated, containerized)
reproducible build and signed by the Build Service. Its fields are `source_commit_hash`,
`source_tree_hash`, `build_environment_hash` (container image digest), `output_binary_hash`, and
`builder_signature`. Anyone can re-run the build in the named environment and check that their
`H(B')` equals the attested `output_binary_hash`.

**Bootstrapping.** A project joins with a `ProjectGenesis` transaction carrying a project id,
the initial stakeholders and roles, the role policy (quorums `τ_r` and required-role set
`R_req`), the slashing parameters `(σ_neg, σ_mal, Δ_max)`, and an initial stake escrow per
stakeholder. It plays the role of TUF's offline Root key but is recorded on-chain, so the
project's entire trust history is auditable. A single global `VGOV` token is shared by all
projects so that one market price secures every project and stake cannot be hidden across
projects.

### Governance

**Roles.** Core Developer (may propose releases), Security Auditor (human firms or automated
services whose approval may be mandatory), Community Trustee (user-elected oversight), and the
non-human Build Service (produces and signs `A_build`).

**Release lifecycle.** A Core Developer submits `NewRelease(project, version, source_commit_hash,
attestation CID)`; stakeholders independently review and rebuild during a voting period; they
sign `(proposal_id, output_binary_hash, yes/no)` votes; an on-chain tally enacts the release by
appending `output_binary_hash` to the Official Release Log after a delay.

**Acceptance rule (Eq. 1).** With `R_req` the required roles, `S_r` the stakeholders of role
`r`, `stake(i)` the `VGOV` staked by `i`, and `τ_r ∈ [0,1]` the role's quorum, a proposal is
accepted iff

```
for every r in R_req:   Σ_{i ∈ S_r, vote_i = yes} stake(i)  /  Σ_{i ∈ S_r} stake(i)   ≥   τ_r
```

The role partition gives *m*-of-*n* across roles; stake weighting within a role ties influence
to economic commitment. Constant stake recovers pure *m*-of-*n*; a single role recovers pure
stake-weighted voting.

**Proposal tracks** (Table 2): New Release (3 of 5 Core Dev **and** 1 of 2 Sec Auditor, 7-day
period, 24 h delay), Emergency Patch (2 of 5 Core Dev **or** 2 of 2 Sec Auditor, 24 h / 1 h),
Add Stakeholder (66 %, 14 d / 7 d), Remove Stakeholder (51 %, 7 d / 24 h), Update Params
(75 %, 30 d / 14 d). Adding voters is deliberately slow and hard; removing a compromised one is
fast — this on-chain **meta-governance** is what distinguishes VeriGov from TUF's manual root
rotation.

### Tokenomics and slashing

`VGOV` provides governance weight, a staking bond required to propose or vote, rewards for
correct participation, and a treasury for audits and bounties. Its teeth are two separately
adjudicated slashing regimes:

- **Negligence slashing** is *cryptographically self-evident* and adjudicated by the chain
  alone: a Build Service signs an `A_build` whose `output_binary_hash` disagrees with an
  independent reproducible rebuild submitted as a counter-attestation; also double-signing,
  voting after role revocation, and liveness faults. The offender's stake is slashed by
  `σ_neg` automatically.
- **Malice slashing** needs *off-chain evidence* ("binary *B* is malicious" is not a
  cryptographic relation), so it runs on a separate Dispute track: a bonded challenger, a
  high-quorum vote by stakeholders who did *not* vote on the original release, a statute of
  limitations `Δ_max`, slashing of the original yes-voters at `σ_mal` (reduced to `σ_neg` for
  voters who did rebuild and were fooled), and burning of frivolous challengers' bonds.

**Cost-of-attack bound (Eq. 2).** To push a malicious release an adversary must assemble, in
every required role, a cooperating set whose stake reaches `τ_r · Σ stake`, paying for each
member the lesser of a bribe and a key-compromise cost *plus* their expected slash
`σ_mal · stake(i) · p` (with `p` the token price). Security therefore scales with the *product*
of stake, slashing fraction and price; concentrated stake lowers the bribe budget; and a
short-`VGOV`-then-attack strategy motivates long unbonding delays.

### Worked example (Section 5.6) — what the demo reproduces

Project `acme-utils`: five Core Developers (Alice, Bob, Carol, Dan, Eve), two Security Auditors
(Foo Audit and the fuzzing service fuzz.io), one Build Service; `τ_CoreDev = 3/5`,
`τ_SecAuditor = 1/2`.

1. Alice tags commit `c0ff33d`; the Build Service builds it in container `deadbeef…`, gets
   `H(B) = a1b2…`, signs `A_build`, pins it to IPFS.
2. Alice submits `NewRelease(acme-utils, 1.2.3, c0ff33d, Qm…42)`; the runtime auto-stakes a
   1,000 `VGOV` bond.
3. Bob, Carol and Foo Audit rebuild independently and match `a1b2…`; fuzz.io finds no regressions.
4. Bob, Carol, Dan vote yes, Eve abstains, both auditors vote yes.
5. Core Dev approval 4/5 ≥ 3/5 and Sec Auditor 2/2 ≥ 1/2 → accepted; `a1b2…` is appended to
   the release log after the enactment delay.
6. A client polls the log, fetches *B*, verifies `H(B)`, installs.

**Failure case.** A compromised Build Service injects a payload at step 1. Bob's rebuild yields
`H(B') ≠ a1b2…`; he submits a counter-attestation; the negligence-slashing logic verifies the
discrepancy and slashes the Build Service without any further governance action.

### Security analysis, assumptions, limits

The paper argues VeriGov neutralizes a compromised maintainer account (one key can only
*propose*), protestware (collective staked review plus a Remove Stakeholder vote), build-server
compromise (independent rebuilds expose the discrepancy) and dependency confusion (the
environment hash pins the dependency set). It assumes secure hash and signature primitives, an
authentic initial client install, non-collusion of a quorum of economically rational
stakeholders, and stakeholder liveness. Open limitations include governance attacks on the
stakeholder set, release latency for continuous-deployment projects (mitigations: batched
releases, delegated pre-approval, bonded fast tracks), and the "Trusting Trust" compiler attack,
which reproducible builds alone cannot detect. Future work names a prototype (this repo),
reputation- or quadratic-weighted voting, formal verification of the protocol, and AI agents as
Security Auditor stakeholders.

### Paper → prototype map

| Paper concept | Where it lives here |
|---|---|
| `ProjectGenesis` (stakeholders, roles, `τ_r`, `R_req`, `σ_neg`/`σ_mal`/`Δ_max`, stake escrow) | `pallets/verigov` `project_genesis`; stake as a `VGOV` hold |
| Stakeholder Registry / Release Proposals / Votes / Official Release Log | `Stakeholders`, `Proposals`, `Votes` + `Tallies`, `ReleaseLog` storage |
| `A_build` and `builder_signature` | `BuildAttestation` + `MultiSignature`, verified in `new_release`; produced by `tools/builder` |
| Eq. 1 acceptance, enactment delay | `meets_quorums`, `PendingEnactments`, `on_initialize` |
| Negligence slashing via counter-attestation | `submit_counter_attestation` → `burn_held(σ_neg · stake)` |
| Distributed Artifact Store | `data/store/<sha256>` (local IPFS stand-in) |
| VeriGov Client | `tools/client` |
| Worked example + failure case | `tools/demo` Scenarios A and B |
| Malice/Dispute track, meta-governance, emergency track, rewards | not implemented (parameters are stored) |

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

CI (`.github/workflows/ci.yml`) builds the node in release mode, runs clippy, the pallet tests
and `cargo doc`, smoke-tests the builder offline, and then runs `scripts/run-demo.sh` against a
throwaway `--dev` node so both scenarios must `PASS` on every push and pull request.

## License

MIT — see `LICENSE`. The node/runtime scaffolding derives from Parity's
[polkadot-sdk-solochain-template](https://github.com/paritytech/polkadot-sdk-solochain-template)
(MIT-0). The paper in `paper/` is © its author.
