#!/usr/bin/env node
// The relayer's hot key, made where it is used and never copied (like the
// faucet's): `keygen.mjs <file>` writes RELAYER_KEY=0x... to <file> (mode
// 0600, created exclusively) if it doesn't exist yet, then prints the
// relayer's address, the one thing to fund. It never prints the key.
//   node src/keygen.mjs /etc/sova/checkout-relayer.key.env
import { openSync, readFileSync, writeSync, closeSync } from 'node:fs';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';

const file = process.argv[2];
if (!file) {
  console.error('usage: keygen.mjs <key-env-file>');
  process.exit(2);
}

function read() {
  const m = readFileSync(file, 'utf8').match(/^RELAYER_KEY=(0x[0-9a-fA-F]{64})\s*$/m);
  if (!m) throw new Error(`${file} has no RELAYER_KEY=0x<64 hex> line`);
  return m[1];
}

let key;
try {
  key = read();
} catch (e) {
  if (e.code !== 'ENOENT') throw e;
  const fd = openSync(file, 'wx', 0o600);
  writeSync(fd, `# The checkout relayer's hot key (tools/checkout-relayer/src/keygen.mjs).\n# Never copy it; rotate = delete this file and re-run deploy.sh.\nRELAYER_KEY=${generatePrivateKey()}\n`);
  closeSync(fd);
  key = read();
  console.error(`keygen: new relayer key written to ${file} (0600)`);
}
console.log(privateKeyToAccount(key).address);
