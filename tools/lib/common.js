// Shared helpers for the VeriGov off-chain tools: paths, hashing, the local
// content-addressed artifact store, and thin polkadot-js wrappers.
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { ApiPromise, WsProvider } from '@polkadot/api';
import { Keyring } from '@polkadot/keyring';
import { u8aToHex, hexToU8a, stringToHex, u8aToString } from '@polkadot/util';

export const REPO_ROOT = path.resolve(import.meta.dirname, '..', '..');
export const STORE_DIR = path.join(REPO_ROOT, 'data', 'store');
export const INSTALL_DIR = path.join(REPO_ROOT, 'install');
export const FIXTURES_DIR = path.join(REPO_ROOT, 'fixtures');
export const DEFAULT_WS = process.env.VERIGOV_WS ?? 'ws://127.0.0.1:9944';

/// 1 VGOV = 10^12 base units (the runtime's `UNIT`).
export const UNIT = 10n ** 12n;
export const VGOV = (n) => BigInt(n) * UNIT;
export const fmtVgov = (v) => `${(BigInt(v.toString()) / UNIT).toLocaleString('en-US')} VGOV`;

/// Perbill helpers (parts per billion).
export const PERBILL = 1_000_000_000;
export const perbill = (num, den) => Math.floor((PERBILL * num) / den);

// ------------------------------------------------------------------ hashing

export const sha256 = (buf) => createHash('sha256').update(buf).digest();
export const sha256Hex = (buf) => u8aToHex(sha256(buf));

/// A git-style short hash (e.g. `c0ff33d`) left-aligned in a 32-byte word: `0xc0ff33d000…`.
export function commitHash32(shortHex) {
  const clean = shortHex.replace(/^0x/, '').toLowerCase();
  if (!/^[0-9a-f]+$/.test(clean) || clean.length > 64) throw new Error(`bad commit id ${shortHex}`);
  return '0x' + clean.padEnd(64, '0');
}

/// The exact 128 bytes a Build Service signs (mirrors `BuildAttestation::signing_payload`).
export function signingPayload(att) {
  const parts = [
    att.source_commit_hash,
    att.source_tree_hash,
    att.build_environment_hash,
    att.output_binary_hash,
  ].map((h) => {
    const u8 = hexToU8a(h);
    if (u8.length !== 32) throw new Error(`hash ${h} is not 32 bytes`);
    return u8;
  });
  const out = new Uint8Array(128);
  parts.forEach((p, i) => out.set(p, i * 32));
  return out;
}

// ------------------------------------------------------------- artifact store

/// Local content-addressed store mimicking IPFS: `data/store/<sha256-hex>`.
export function storePut(bytes) {
  fs.mkdirSync(STORE_DIR, { recursive: true });
  const cid = sha256Hex(bytes);
  const file = path.join(STORE_DIR, cid.slice(2));
  if (!fs.existsSync(file)) fs.writeFileSync(file, bytes);
  return cid;
}

export function storeGet(cid) {
  const file = path.join(STORE_DIR, cid.replace(/^0x/, ''));
  if (!fs.existsSync(file)) throw new Error(`artifact ${cid} not found in ${STORE_DIR}`);
  const bytes = fs.readFileSync(file);
  const actual = sha256Hex(bytes);
  if (actual !== cid.toLowerCase()) {
    throw new Error(`artifact store corruption: ${cid} has content hash ${actual}`);
  }
  return bytes;
}

export const storePath = (cid) => path.join(STORE_DIR, cid.replace(/^0x/, ''));

// ---------------------------------------------------------------- chain glue

export async function connect(ws = DEFAULT_WS) {
  const provider = new WsProvider(ws);
  const api = await ApiPromise.create({ provider, noInitWarn: true });
  return api;
}

export function keyring() {
  return new Keyring({ type: 'sr25519', ss58Format: 42 });
}

/// The acme-utils cast, as dev-derivation URIs. `//Alice` is also the chain's sudo key.
export const CAST = {
  alice: '//Alice',
  bob: '//Bob',
  carol: '//Carol',
  dan: '//Dan',
  eve: '//Eve',
  fooAudit: '//FooAudit',
  fuzzIo: '//FuzzIo',
  buildService: '//BuildService',
};

/// Encode a UTF-8 string for a `BoundedVec<u8, _>` argument.
export const bytesArg = (s) => stringToHex(s);
export const bytesToString = (b) => u8aToString(b.toU8a ? b.toU8a(true) : b);

export function decodeDispatchError(api, dispatchError) {
  if (dispatchError.isModule) {
    const { section, name, docs } = api.registry.findMetaError(dispatchError.asModule);
    return `${section}.${name}: ${docs.join(' ')}`;
  }
  return dispatchError.toString();
}

/// Sign, submit, and resolve once the extrinsic is in a block (rejecting on dispatch error).
export function signAndSend(api, tx, signer, opts = {}) {
  return new Promise((resolve, reject) => {
    let unsub = () => {};
    tx.signAndSend(signer, opts, ({ status, events, dispatchError }) => {
      if (dispatchError) {
        unsub();
        reject(new Error(decodeDispatchError(api, dispatchError)));
        return;
      }
      if (status.isInBlock || status.isFinalized) {
        unsub();
        // A sudo call can fail inside the wrapper; surface that too.
        for (const { event } of events) {
          if (api.events.sudo?.Sudid?.is(event) && event.data.sudoResult.isErr) {
            reject(new Error(`sudo: ${decodeDispatchError(api, event.data.sudoResult.asErr)}`));
            return;
          }
        }
        resolve({ events, blockHash: status.isInBlock ? status.asInBlock : status.asFinalized });
      }
    })
      .then((u) => { unsub = u; })
      .catch(reject);
  });
}

export function findEvent(api, events, section, method) {
  const hit = events.find(({ event }) => api.events[section]?.[method]?.is(event));
  return hit ? hit.event : undefined;
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// Poll `fn` until it returns a truthy value or `timeoutMs` elapses.
export async function waitFor(fn, { timeoutMs = 120_000, intervalMs = 1_000, what = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(intervalMs);
  }
}
