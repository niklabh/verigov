#!/usr/bin/env node
// End-to-end VeriGov demo against a local `--dev` node. Reproduces the paper's
// `acme-utils v1.2.3` release (Section 5.6) and its failure case:
//
//   Scenario A  honest Build Service → proposal → role-weighted vote → enactment
//               → client verifies and installs.
//   Scenario B  compromised Build Service → proposal → independent rebuild
//               disagrees → counter-attestation → negligence slash, proposal rejected,
//               release log unchanged, client installs nothing.
//
//   node tools/demo/demo.js [--ws ws://127.0.0.1:9944]
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { cryptoWaitReady } from '@polkadot/util-crypto';
import {
  CAST,
  DEFAULT_WS,
  FIXTURES_DIR,
  INSTALL_DIR,
  VGOV,
  bytesArg,
  connect,
  findEvent,
  fmtVgov,
  keyring,
  perbill,
  sha256Hex,
  signAndSend,
  waitFor,
} from '../lib/common.js';
import { buildAndAttest, compile } from '../builder/build.js';
import { syncProject } from '../client/client.js';

const ws = process.argv.includes('--ws') ? process.argv[process.argv.indexOf('--ws') + 1] : DEFAULT_WS;

// Genesis parameters (paper: tau_CoreDev = 3/5, tau_SecAuditor = 1/2, 1,000 VGOV bond).
const STAKE = VGOV(10_000);
const BUILD_STAKE = VGOV(50_000);
const BOND = VGOV(1_000);
const FUND = VGOV(100_000);
const VOTING_PERIOD = 100; // blocks (paper: 7 days)
const ENACTMENT_DELAY = 2; // blocks (paper: 24 h)
const SIGMA_NEG = perbill(1, 2);
const SIGMA_MAL = perbill(1, 1);
const DELTA_MAX = 1_000;

const results = [];
const hr = (t) => console.log(`\n${'='.repeat(78)}\n${t}\n${'='.repeat(78)}`);
const step = (t) => console.log(`\n▶ ${t}`);
const info = (t) => console.log(`  ${t}`);
const check = (cond, msg) => {
  console.log(`  ${cond ? '✔' : '✘'} ${msg}`);
  if (!cond) throw new Error(`check failed: ${msg}`);
};

async function main() {
  await cryptoWaitReady();
  const api = await connect(ws);
  const kr = keyring();
  const who = Object.fromEntries(Object.entries(CAST).map(([k, uri]) => [k, kr.addFromUri(uri)]));
  const addr = (k) => who[k].address;
  const name = Object.fromEntries(Object.entries(who).map(([k, p]) => [p.address, k]));

  const chain = await api.rpc.system.chain();
  const version = api.runtimeVersion.specName.toString() + ' v' + api.runtimeVersion.specVersion.toString();
  hr(`VeriGov demo  —  ${chain} (${version}) at ${ws}`);

  try {
    // ------------------------------------------------------------------ setup
    step('Fund the cast (dev faucet: sudo → balances.forceSetBalance)');
    const nonce0 = (await api.rpc.system.accountNextIndex(addr('alice'))).toNumber();
    const cast = Object.keys(who);
    const topUps = [];
    for (const k of cast) {
      const free = (await api.query.system.account(addr(k))).data.free.toBigInt();
      if (free < FUND) topUps.push(k);
    }
    await Promise.all(
      topUps.map((k, i) =>
        signAndSend(api, api.tx.sudo.sudo(api.tx.balances.forceSetBalance(addr(k), FUND)), who.alice, { nonce: nonce0 + i }),
      ),
    );
    info(`${topUps.length} of ${cast.length} accounts topped up to ${fmtVgov(FUND)} free`);

    step('ProjectGenesis(acme-utils) via sudo: stakeholders, roles, quorums, slashing, initial stake');
    const stakeholders = [
      ...['alice', 'bob', 'carol', 'dan', 'eve'].map((k) => ({ account: addr(k), role: 'CoreDeveloper', stake: STAKE })),
      ...['fooAudit', 'fuzzIo'].map((k) => ({ account: addr(k), role: 'SecurityAuditor', stake: STAKE })),
      { account: addr('buildService'), role: 'BuildService', stake: BUILD_STAKE },
    ];
    const genesis = api.tx.verigov.projectGenesis(
      bytesArg('acme-utils'),
      stakeholders,
      {
        requiredRoles: [
          { role: 'CoreDeveloper', quorum: perbill(3, 5) },
          { role: 'SecurityAuditor', quorum: perbill(1, 2) },
        ],
        votingPeriod: VOTING_PERIOD,
        enactmentDelay: ENACTMENT_DELAY,
      },
      { sigmaNeg: SIGMA_NEG, sigmaMal: SIGMA_MAL, deltaMax: DELTA_MAX },
      BOND,
    );
    const g = await signAndSend(api, api.tx.sudo.sudo(genesis), who.alice);
    const projectId = findEvent(api, g.events, 'verigov', 'ProjectCreated').data.projectId.toNumber();
    info(`project #${projectId} registered; Core Devs: Alice Bob Carol Dan Eve (${fmtVgov(STAKE)} each)`);
    info(`Security Auditors: FooAudit fuzz.io (${fmtVgov(STAKE)} each); Build Service (${fmtVgov(BUILD_STAKE)})`);
    info(`policy: tau_CoreDev = 3/5, tau_SecAuditor = 1/2; bond ${fmtVgov(BOND)}; sigma_neg = 50%`);
    const bs = (await api.query.verigov.stakeholders(projectId, addr('buildService'))).unwrap();
    check(bs.role.isBuildService && bs.stake.toBigInt() === BUILD_STAKE, 'Build Service registered with escrowed stake');

    // ----------------------------------------------------------- scenario A
    hr('Scenario A — honest release of acme-utils v1.2.3 (paper, Section 5.6)');
    await scenarioA(api, who, name, projectId);

    // ----------------------------------------------------------- scenario B
    hr('Scenario B — compromised Build Service, counter-attestation, negligence slash');
    await scenarioB(api, who, name, projectId);
  } catch (e) {
    console.error(`\n  ✘ ${e.message}`);
    results.push({ name: 'demo aborted', pass: false });
  } finally {
    await api.disconnect();
  }

  hr('Summary');
  for (const r of results) console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name}`);
  const ok = results.length === 2 && results.every((r) => r.pass);
  console.log(ok ? '\nALL PASS' : '\nFAILED');
  process.exit(ok ? 0 : 1);
}

async function scenarioA(api, who, name, projectId) {
  const addr = (k) => who[k].address;
  try {
    step('Step 1 (off-chain): Build Service builds c0ff33d hermetically, signs A_build, pins to store');
    const build = await buildAndAttest({ version: '1.2.3', commit: 'c0ff33d' });
    info(`source_commit_hash   ${build.attestation.source_commit_hash}`);
    info(`build_environment    ${build.attestation.build_environment_hash}`);
    info(`output_binary_hash   ${build.attestation.output_binary_hash}`);
    info(`A_build CID          ${build.attestationCid}`);

    step('Step 2 (on-chain): Alice submits NewRelease(acme-utils, 1.2.3, c0ff33d, A_build)');
    const r = await signAndSend(
      api,
      api.tx.verigov.newRelease(projectId, bytesArg('1.2.3'), build.attestation, build.attestationCid, { Sr25519: build.signature }),
      who.alice,
    );
    const proposed = findEvent(api, r.events, 'verigov', 'ReleaseProposed').data;
    const proposalId = proposed.proposalId.toNumber();
    info(`proposal #${proposalId} open; voting ends at block ${proposed.votingEnds}`);
    const bondHeld = await heldBy(api, addr('alice'), 'ProposalBond');
    check(bondHeld === VGOV(1_000), `runtime auto-staked ${fmtVgov(bondHeld)} proposal bond from Alice`);

    step('Step 3 (off-chain): Bob, Carol, Foo Audit rebuild c0ff33d independently');
    const rebuilt = compile({ srcDir: path.join(FIXTURES_DIR, 'acme-utils'), version: '1.2.3', commit: 'c0ff33d' });
    check(sha256Hex(rebuilt) === build.attestation.output_binary_hash, 'independent rebuild hash matches A_build');

    step('Step 4 (on-chain): Bob, Carol, Dan vote yes; Eve abstains; Foo Audit and fuzz.io vote yes');
    const voters = ['bob', 'carol', 'dan', 'fooAudit', 'fuzzIo'];
    const votes = await Promise.all(voters.map((k) => signAndSend(api, api.tx.verigov.vote(proposalId, true), who[k])));
    for (const v of votes) {
      const ev = findEvent(api, v.events, 'verigov', 'Voted').data;
      info(`Voted(aye) by ${name[ev.voter.toString()]} as ${ev.role.type}, weight ${fmtVgov(ev.weight)}`);
    }
    const tallyDev = await api.query.verigov.tallies(proposalId, 'CoreDeveloper');
    const tallyAud = await api.query.verigov.tallies(proposalId, 'SecurityAuditor');
    info(`tally: CoreDev ayes ${fmtVgov(tallyDev.ayes)} / ${fmtVgov(5n * STAKE)}  (4/5 ≥ 3/5)`);
    info(`tally: SecAuditor ayes ${fmtVgov(tallyAud.ayes)} / ${fmtVgov(2n * STAKE)}  (2/2 ≥ 1/2)`);

    step('Step 5 (on-chain): runtime evaluates Eq. 1 and enacts after the delay');
    let prop = (await api.query.verigov.proposals(proposalId)).unwrap();
    check(prop.status.isApproved || prop.status.isEnacted, `proposal status ${prop.status.type}${prop.status.isApproved ? ` (enact at block ${prop.status.asApproved.enactAt})` : ''}`);
    await waitFor(async () => (await api.query.verigov.proposals(proposalId)).unwrap().status.isEnacted, {
      what: 'enactment',
      timeoutMs: 90_000,
    });
    const count = (await api.query.verigov.releaseCount(projectId)).toNumber();
    const entry = (await api.query.verigov.releaseLog(projectId, count - 1)).unwrap();
    check(entry.outputBinaryHash.toHex() === build.attestation.output_binary_hash, `Official Release Log[${count - 1}] = ${entry.outputBinaryHash.toHex().slice(0, 22)}…`);
    check((await heldBy(api, addr('alice'), 'ProposalBond')) === 0n, 'proposal bond released back to Alice');

    step('Step 6 (off-chain): VeriGov client polls the log, fetches from store, verifies, installs');
    fs.rmSync(path.join(INSTALL_DIR, 'acme-utils-1.2.3'), { force: true });
    const sync = await syncProject({ api, projectId, log: info });
    check(sync.installed.length === 1, 'client installed exactly one release');
    const installed = sync.installed[0];
    check(sha256Hex(fs.readFileSync(installed.path)) === entry.outputBinaryHash.toHex(), 'installed binary hash == blessed hash');
    const out = execFileSync('sh', [installed.path, 'sum', '2', '3']).toString().trim();
    check(out === 'acme-utils 1.2.3 (commit c0ff33d)\n5', `installed artifact runs: ${JSON.stringify(out)}`);

    results.push({ name: 'Scenario A: honest release accepted, enacted, verified and installed', pass: true });
  } catch (e) {
    console.error(`  ✘ ${e.message}`);
    results.push({ name: `Scenario A: ${e.message}`, pass: false });
  }
}

async function scenarioB(api, who, name, projectId) {
  const addr = (k) => who[k].address;
  try {
    const stakeBefore = (await api.query.verigov.stakeholders(projectId, addr('buildService'))).unwrap().stake.toBigInt();
    const holdBefore = await heldBy(api, addr('buildService'), 'Stake');
    const releasesBefore = (await api.query.verigov.releaseCount(projectId)).toNumber();
    const issuanceBefore = (await api.query.balances.totalIssuance()).toBigInt();

    step('Step 1 (off-chain): a COMPROMISED Build Service injects a payload into the 1.2.4 build and signs A_build anyway');
    const bad = await buildAndAttest({ version: '1.2.4', commit: 'd00d1e5', inject: true });
    info(`attested output_binary_hash ${bad.attestation.output_binary_hash}`);

    step('Step 2 (on-chain): Alice (unaware) submits NewRelease(acme-utils, 1.2.4, d00d1e5, A_build)');
    const r = await signAndSend(
      api,
      api.tx.verigov.newRelease(projectId, bytesArg('1.2.4'), bad.attestation, bad.attestationCid, { Sr25519: bad.signature }),
      who.alice,
    );
    const proposalId = findEvent(api, r.events, 'verigov', 'ReleaseProposed').data.proposalId.toNumber();
    info(`proposal #${proposalId} open`);
    await signAndSend(api, api.tx.verigov.vote(proposalId, true), who.carol);
    info('Carol votes yes without rebuilding (2/5 core devs so far)');

    step('Step 3 (off-chain): Bob rebuilds d00d1e5 in the attested environment');
    const rebuilt = compile({ srcDir: path.join(FIXTURES_DIR, 'acme-utils'), version: '1.2.4', commit: 'd00d1e5' });
    const rebuiltHash = sha256Hex(rebuilt);
    info(`Bob's H(B') = ${rebuiltHash}`);
    check(rebuiltHash !== bad.attestation.output_binary_hash, 'H(B\') ≠ attested output_binary_hash — discrepancy detected');

    step('Step 4 (on-chain): Bob submits a counter-attestation');
    const c = await signAndSend(api, api.tx.verigov.submitCounterAttestation(proposalId, rebuiltHash), who.bob);
    const accepted = findEvent(api, c.events, 'verigov', 'CounterAttestationAccepted');
    const slashed = findEvent(api, c.events, 'verigov', 'BuildServiceSlashed');
    const rejected = findEvent(api, c.events, 'verigov', 'ProposalRejected');
    check(!!accepted, `CounterAttestationAccepted by ${name[accepted?.data.challenger.toString()] ?? '?'}`);
    check(!!rejected, `ProposalRejected(#${proposalId})`);
    check(!!slashed, `BuildServiceSlashed: ${slashed ? fmtVgov(slashed.data.amount) : '-'} burned, ${slashed ? fmtVgov(slashed.data.remainingStake) : '-'} remaining`);

    step('Step 5: verify on-chain consequences');
    const stakeAfter = (await api.query.verigov.stakeholders(projectId, addr('buildService'))).unwrap().stake.toBigInt();
    check(stakeAfter === stakeBefore / 2n, `Build Service stake ${fmtVgov(stakeBefore)} → ${fmtVgov(stakeAfter)} (sigma_neg = 50%)`);
    const holdAfter = await heldBy(api, addr('buildService'), 'Stake');
    check(holdBefore - holdAfter === stakeBefore - stakeAfter, `VGOV held on Build Service reduced by ${fmtVgov(holdBefore - holdAfter)}`);
    const issuanceAfter = (await api.query.balances.totalIssuance()).toBigInt();
    // Fees are burned too on this chain, so issuance drops by at least the slash.
    const burned = issuanceBefore - issuanceAfter;
    check(burned >= stakeBefore - stakeAfter, `slashed VGOV burned from total issuance (Δ issuance = -${fmtVgov(burned)} incl. fees)`);
    const prop = (await api.query.verigov.proposals(proposalId)).unwrap();
    check(prop.status.isRejected, `proposal #${proposalId} status ${prop.status.type}`);
    check((await heldBy(api, addr('alice'), 'ProposalBond')) === 0n, 'good-faith proposer bond returned to Alice');
    const releasesAfter = (await api.query.verigov.releaseCount(projectId)).toNumber();
    check(releasesAfter === releasesBefore, `Official Release Log unchanged (${releasesAfter} entries)`);

    step('Step 6 (off-chain): client polls the log — nothing new to install');
    const sync = await syncProject({ api, projectId, log: info });
    check(sync.installed.length === 0, 'client installed nothing');
    check(!fs.existsSync(path.join(INSTALL_DIR, 'acme-utils-1.2.4')), 'tampered 1.2.4 never reached ./install');

    results.push({ name: 'Scenario B: tampered build caught, Build Service slashed, release blocked', pass: true });
  } catch (e) {
    console.error(`  ✘ ${e.message}`);
    results.push({ name: `Scenario B: ${e.message}`, pass: false });
  }
}

/// Amount of VGOV held on `account` under `verigov::HoldReason::<reason>`.
async function heldBy(api, account, reason) {
  const holds = await api.query.balances.holds(account);
  let total = 0n;
  for (const h of holds) {
    if (h.id.isVerigov && h.id.asVerigov.type === reason) total += h.amount.toBigInt();
  }
  return total;
}

await main();
