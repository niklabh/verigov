#!/usr/bin/env node
// VeriGov Build Service (toy). Performs a deterministic "hermetic build" of the
// acme-utils source tree, produces the build attestation A_build, signs it with
// the Build Service key, and pins both binary and attestation to the local
// content-addressed store (./data/store).
//
//   node tools/builder/build.js --version 1.2.3 --commit c0ff33d [--inject]
//
// `--inject` simulates a compromised Build Service that silently appends a
// malicious payload to the binary while still signing the attestation.
import fs from 'node:fs';
import path from 'node:path';
import { cryptoWaitReady } from '@polkadot/util-crypto';
import { u8aToHex } from '@polkadot/util';
import {
  CAST,
  FIXTURES_DIR,
  commitHash32,
  keyring,
  sha256Hex,
  signingPayload,
  storePath,
  storePut,
} from '../lib/common.js';

const PROJECT = 'acme-utils';

function listFiles(dir, base = dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFiles(full, base));
    else out.push(path.relative(base, full).split(path.sep).join('/'));
  }
  return out.sort();
}

/// `source_tree_hash`: hash of `<relpath>\0<sha256(content)>\n` for every file, sorted.
export function sourceTreeHash(srcDir) {
  const lines = listFiles(srcDir).map((rel) => `${rel}\0${sha256Hex(fs.readFileSync(path.join(srcDir, rel)))}\n`);
  return sha256Hex(Buffer.from(lines.join('')));
}

/// `build_environment_hash`: digest of the "container image", i.e. this builder's own code.
export function buildEnvironmentHash() {
  return sha256Hex(fs.readFileSync(import.meta.filename));
}

/// Deterministic compile: inline lib/*.sh, then main.sh with placeholders substituted.
export function compile({ srcDir, version, commit, inject = false }) {
  const files = listFiles(srcDir);
  let out = `#!/bin/sh\n# ${PROJECT} ${version}\n# source_commit=${commit}\n# built by the VeriGov hermetic toy builder\nset -e\n`;
  for (const rel of files) {
    const body = fs
      .readFileSync(path.join(srcDir, rel), 'utf8')
      .replace(/^#!.*\n/, '')
      .replaceAll('__VERSION__', version)
      .replaceAll('__COMMIT__', commit);
    out += `\n# --- ${rel} ---\n${body}`;
  }
  if (inject) {
    out += `\n# --- injected by compromised build service ---\necho "PWNED: exfiltrating ~/.ssh to attacker.example (simulated)" >&2\n`;
  }
  return Buffer.from(out);
}

/// Run the toy hermetic build and produce a signed A_build. Returns everything the
/// proposer and the client need.
export async function buildAndAttest({
  version,
  commit,
  inject = false,
  builderUri = CAST.buildService,
  srcDir = path.join(FIXTURES_DIR, PROJECT),
  pin = true,
} = {}) {
  await cryptoWaitReady();
  const builder = keyring().addFromUri(builderUri);

  const binary = compile({ srcDir, version, commit, inject });
  const attestation = {
    source_commit_hash: commitHash32(commit),
    source_tree_hash: sourceTreeHash(srcDir),
    build_environment_hash: buildEnvironmentHash(),
    output_binary_hash: sha256Hex(binary),
    builder: builder.address,
  };
  const payload = signingPayload(attestation);
  const signature = u8aToHex(builder.sign(payload));

  const attestationJson = Buffer.from(
    JSON.stringify(
      {
        schema: 'verigov/A_build/v1',
        project: PROJECT,
        version,
        ...attestation,
        builder_public_key: u8aToHex(builder.publicKey),
        builder_signature: signature,
        signature_scheme: 'sr25519',
        signed_payload: 'source_commit_hash || source_tree_hash || build_environment_hash || output_binary_hash',
      },
      null,
      2,
    ) + '\n',
  );

  let binaryCid = attestation.output_binary_hash;
  let attestationCid = sha256Hex(attestationJson);
  if (pin) {
    binaryCid = storePut(binary);
    attestationCid = storePut(attestationJson);
  }

  return { project: PROJECT, version, commit, inject, binary, attestation, signature, attestationJson, attestationCid, binaryCid };
}

// ------------------------------------------------------------------- CLI

function parseArgs(argv) {
  const args = { version: '1.2.3', commit: 'c0ff33d', inject: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--version') args.version = argv[++i];
    else if (a === '--commit') args.commit = argv[++i];
    else if (a === '--inject') args.inject = true;
    else if (a === '--builder') args.builderUri = argv[++i];
    else if (a === '--help' || a === '-h') {
      console.log('usage: build.js [--version V] [--commit C] [--builder //Uri] [--inject]');
      process.exit(0);
    } else throw new Error(`unknown argument ${a}`);
  }
  return args;
}

if (process.argv[1] && path.resolve(process.argv[1]) === import.meta.filename) {
  const args = parseArgs(process.argv.slice(2));
  const r = await buildAndAttest(args);
  console.log(`[builder] ${r.project} ${r.version} @ ${r.commit}${r.inject ? '  (COMPROMISED: payload injected)' : ''}`);
  console.log(`[builder] builder account      ${r.attestation.builder}`);
  console.log(`[builder] source_commit_hash   ${r.attestation.source_commit_hash}`);
  console.log(`[builder] source_tree_hash     ${r.attestation.source_tree_hash}`);
  console.log(`[builder] build_environment    ${r.attestation.build_environment_hash}`);
  console.log(`[builder] output_binary_hash   ${r.attestation.output_binary_hash}`);
  console.log(`[builder] builder_signature    ${r.signature}`);
  console.log(`[builder] pinned binary        ${storePath(r.binaryCid)}`);
  console.log(`[builder] pinned attestation   ${storePath(r.attestationCid)}  (CID ${r.attestationCid})`);
}
