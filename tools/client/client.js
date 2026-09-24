#!/usr/bin/env node
// VeriGov Client (toy). Trusts only the on-chain Official Release Log. For every
// blessed entry it fetches the attestation and binary from the local artifact
// store, verifies content hashes, the attestation's binding to the on-chain
// entry, the Build Service signature, and the builder's on-chain role, and then
// installs the binary into ./install/.
//
//   node tools/client/client.js --project 0 [--watch] [--ws ws://127.0.0.1:9944]
import fs from 'node:fs';
import path from 'node:path';
import { hexToU8a } from '@polkadot/util';
import { cryptoWaitReady, decodeAddress, sr25519Verify } from '@polkadot/util-crypto';
import {
  DEFAULT_WS,
  INSTALL_DIR,
  bytesToString,
  connect,
  sha256Hex,
  signingPayload,
  sleep,
  storeGet,
} from '../lib/common.js';

const noop = () => {};

/// Verify and install one release-log entry. Throws on any verification failure.
export async function verifyAndInstall({ api, projectId, projectName, entry, installDir = INSTALL_DIR, log = noop }) {
  const version = bytesToString(entry.version);
  const onChainBinaryHash = entry.outputBinaryHash.toHex();
  const attestationCid = entry.attestationCid.toHex();
  const onChainCommit = entry.sourceCommitHash.toHex();
  log(`  release-log entry: ${projectName} ${version}  proposal #${entry.proposalId}  enacted at block ${entry.enactedAt}`);
  log(`    blessed output_binary_hash ${onChainBinaryHash}`);

  // 1. Fetch A_build by CID; the store verifies content == address.
  const attBytes = storeGet(attestationCid);
  const att = JSON.parse(attBytes.toString('utf8'));
  log(`    fetched A_build ${attestationCid} (${attBytes.length} bytes, content hash OK)`);

  // 2. A_build must be the one the vote was about.
  if (att.output_binary_hash?.toLowerCase() !== onChainBinaryHash) {
    throw new Error(`attestation output_binary_hash ${att.output_binary_hash} != on-chain ${onChainBinaryHash}`);
  }
  if (att.source_commit_hash?.toLowerCase() !== onChainCommit) {
    throw new Error(`attestation source_commit_hash ${att.source_commit_hash} != on-chain ${onChainCommit}`);
  }
  log(`    A_build binds source ${att.source_commit_hash.slice(0, 18)}… → binary ${onChainBinaryHash.slice(0, 18)}…  (matches log)`);

  // 3. The signer must be a registered Build Service of this project...
  const builderInfo = await api.query.verigov.stakeholders(projectId, att.builder);
  if (builderInfo.isNone || !builderInfo.unwrap().role.isBuildService) {
    throw new Error(`attestation builder ${att.builder} is not a registered Build Service`);
  }
  // ...and the signature must verify over the exact payload the chain checked.
  const sigOk = sr25519Verify(signingPayload(att), hexToU8a(att.builder_signature), decodeAddress(att.builder));
  if (!sigOk) throw new Error('builder_signature does not verify');
  log(`    builder ${att.builder} is an on-chain Build Service; sr25519 signature OK`);

  // 4. Fetch the binary by its hash and verify.
  const binary = storeGet(onChainBinaryHash);
  const actual = sha256Hex(binary);
  if (actual !== onChainBinaryHash) throw new Error(`binary hash ${actual} != ${onChainBinaryHash}`);
  log(`    fetched binary (${binary.length} bytes); sha256 matches the Official Release Log`);

  // 5. Install.
  fs.mkdirSync(installDir, { recursive: true });
  const target = path.join(installDir, `${projectName}-${version}`);
  fs.writeFileSync(target, binary, { mode: 0o755 });
  fs.writeFileSync(`${target}.attestation.json`, attBytes);
  log(`    installed → ${path.relative(process.cwd(), target)}`);
  return { version, path: target, hash: onChainBinaryHash, attestationCid };
}

/// Sync the local install dir with the project's Official Release Log.
export async function syncProject({ api, projectId, installDir = INSTALL_DIR, log = noop }) {
  const project = await api.query.verigov.projects(projectId);
  if (project.isNone) throw new Error(`project ${projectId} not found on chain`);
  const projectName = bytesToString(project.unwrap().name);
  const count = (await api.query.verigov.releaseCount(projectId)).toNumber();
  log(`[client] ${projectName} (project #${projectId}): ${count} entr${count === 1 ? 'y' : 'ies'} in the Official Release Log`);

  const installed = [];
  const skipped = [];
  for (let i = 0; i < count; i++) {
    const entry = (await api.query.verigov.releaseLog(projectId, i)).unwrap();
    const version = bytesToString(entry.version);
    const target = path.join(installDir, `${projectName}-${version}`);
    if (fs.existsSync(target) && sha256Hex(fs.readFileSync(target)) === entry.outputBinaryHash.toHex()) {
      skipped.push(version);
      continue;
    }
    installed.push(await verifyAndInstall({ api, projectId, projectName, entry, installDir, log }));
  }
  if (skipped.length) log(`  already installed and hash-verified: ${skipped.join(', ')}`);
  return { projectName, count, installed, skipped };
}

// ------------------------------------------------------------------- CLI

function parseArgs(argv) {
  const args = { projectId: 0, watch: false, ws: DEFAULT_WS, interval: 3000 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--project') args.projectId = Number(argv[++i]);
    else if (a === '--watch') args.watch = true;
    else if (a === '--ws') args.ws = argv[++i];
    else if (a === '--interval') args.interval = Number(argv[++i]);
    else if (a === '--help' || a === '-h') {
      console.log('usage: client.js [--project N] [--watch] [--ws URL] [--interval MS]');
      process.exit(0);
    } else throw new Error(`unknown argument ${a}`);
  }
  return args;
}

if (process.argv[1] && path.resolve(process.argv[1]) === import.meta.filename) {
  const args = parseArgs(process.argv.slice(2));
  await cryptoWaitReady();
  const api = await connect(args.ws);
  try {
    do {
      const r = await syncProject({ api, projectId: args.projectId, log: console.log });
      if (!r.installed.length && !args.watch) console.log('[client] nothing new to install');
      if (args.watch) await sleep(args.interval);
    } while (args.watch);
  } finally {
    await api.disconnect();
  }
}
