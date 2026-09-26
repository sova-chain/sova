#!/usr/bin/env node
// Move the relayer's SOVA to another address (before a key rotation, or to
// retire it): everything but the transfer's own gas.
//   RELAYER_KEY=... SOVA_RPC_URL=... node src/sweep.mjs <0xTo> [--dry-run]
// On the host, as root (the key file is root 0600):
//   node --env-file=/etc/sova/checkout-relayer.env --env-file=/etc/sova/checkout-relayer.key.env \
//     /opt/sova-checkout-relayer/src/sweep.mjs 0x...
import { createPublicClient, createWalletClient, defineChain, formatEther, http } from 'viem';
import { privateKeyToAccount } from 'viem/accounts';

const [to, flag] = process.argv.slice(2);
if (!/^0x[0-9a-fA-F]{40}$/.test(to || '')) {
  console.error('usage: sweep.mjs <0xTo> [--dry-run]');
  process.exit(2);
}
const rpc = process.env.SOVA_RPC_URL || 'http://127.0.0.1:8545';
const account = privateKeyToAccount(process.env.RELAYER_KEY);
const transport = http(rpc);
const id = await createPublicClient({ transport }).getChainId();
const chain = defineChain({
  id, name: 'Sova', nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 }, rpcUrls: { default: { http: [rpc] } },
});
const pub = createPublicClient({ chain, transport });
const bal = await pub.getBalance({ address: account.address });
const gas = 21_000n;
// Legacy gas price with headroom, so the value + fee never exceeds the balance.
const price = ((await pub.getGasPrice()) * 3n) / 2n + 1n;
const value = bal - gas * price;
console.log(`relayer ${account.address}: ${formatEther(bal)} SOVA; sending ${value > 0n ? formatEther(value) : 0} to ${to}`);
if (value <= 0n) process.exit(0);
if (flag === '--dry-run') process.exit(0);
const hash = await createWalletClient({ chain, transport, account }).sendTransaction({ to, value, gas, gasPrice: price });
const r = await pub.waitForTransactionReceipt({ hash, timeout: 300_000 });
console.log(`${r.status} in ${hash}`);
