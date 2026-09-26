// Sova side: viem clients, the checkout ABI, and a serialized sender so
// concurrent requests never race on the relayer's nonce. Every RPC request
// goes through one throttle (limits.mjs), so a busy relayer queues instead
// of tripping the public RPC's per-IP limit.
import {
  BaseError, ContractFunctionRevertedError, InsufficientFundsError, createPublicClient, createWalletClient,
  defineChain, http, parseAbi, parseEventLogs,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { throttledFetch } from './limits.mjs';

export const ZCASH_PRECOMPILE = '0x0000000000000000000000000000000000005a00';

export const CHECKOUT_ABI = parseAbi([
  'function reserve(uint256 listingId, address recipient) returns (uint256)',
  'function claim(uint256 id, bytes32 txid, uint32 vout) returns (uint256)',
  'function reservations(uint256) view returns (address recipient, uint64 quoteZat, uint16 minConf, bool payeeP2sh, bool filled, bytes20 payeeHash, uint64 reservedAt, uint32 window)',
  'event Reserved(uint256 indexed reservationId, uint256 indexed listingId, address indexed recipient, uint64 quoteZat, bytes20 payeeHash, bool payeeP2sh, uint64 reservedAt, uint64 deadline)',
  'event Claimed(uint256 indexed reservationId, address indexed recipient, bytes32 txid, uint32 vout, uint64 height, uint256 itemId)',
  'error NoSuchReservation()',
  'error AlreadyFilled()',
  'error ListingInactive()',
  'error ZeroRecipient()',
  'error TagsExhausted()',
  'error Immutable()',
  'error PaymentAlreadyUsed(uint256 filledReservation)',
  'error WrongAmount(uint64 valueZat, uint64 quoteZat)',
  'error PaidBeforeReservation(uint64 paymentHeight, uint64 reservedAt)',
  'error PaidAfterDeadline(uint64 paymentHeight, uint64 deadline)',
  'error ZcashTxNotFound(bytes32 txid)',
  'error ZcashNotYet(bytes32 txid)',
  'error ZcashOutOfRange(bytes32 txid)',
  'error ZcashNoSuchOutput(bytes32 txid, uint32 vout)',
  'error ZcashInsufficientConfirmations(bytes32 txid, uint64 confirmations, uint64 minConf)',
  'error ZcashWrongScript(bytes32 txid, uint32 vout)',
  'error ZcashUnderpaid(bytes32 txid, uint32 vout, uint64 valueZat, uint64 minZat)',
  'error ZcashBadStatus(uint8 status)',
]);

const ZCASH_ABI = parseAbi(['function anchor() view returns (uint64 height, bytes32 hash)']);
const ASHWINGS_ABI = parseAbi(['function zecCheckout() view returns (address)']);

/** Custom error name of a revert, or null if it wasn't a contract revert. */
export function errorName(e) {
  if (!(e instanceof BaseError)) return null;
  const r = e.walk((x) => x instanceof ContractFunctionRevertedError);
  return r?.data?.errorName ?? (r ? 'reverted' : null);
}

/** True if the relayer's own balance can't pay for the transaction. */
export function outOfFunds(e) {
  if (e instanceof BaseError && e.walk((x) => x instanceof InsufficientFundsError)) return true;
  return /insufficient funds|gas required exceeds allowance/i.test(e?.message || '');
}

export function describe(e) {
  if (e instanceof BaseError) {
    const r = e.walk((x) => x instanceof ContractFunctionRevertedError);
    if (r?.data) return `${r.data.errorName}(${(r.data.args || []).map(String).join(', ')})`;
    return e.shortMessage;
  }
  return e instanceof Error ? e.message : String(e);
}

export async function connectSova(cfg) {
  const transport = http(cfg.sovaRpc, {
    fetchFn: throttledFetch(cfg.rpcPer10s ?? 30),
    retryCount: 4,
    retryDelay: 1000,
    timeout: 20_000,
  });
  const chainId = await createPublicClient({ transport }).getChainId();
  const chain = defineChain({
    id: chainId,
    name: 'Sova',
    nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 },
    rpcUrls: { default: { http: [cfg.sovaRpc] } },
  });
  const pub = createPublicClient({ chain, transport, pollingInterval: cfg.rpcPollMs ?? 4000 });

  // The checkout: CHECKOUT, or Ashwings.zecCheckout() of ASHWINGS; with both
  // set they must agree (a stale CHECKOUT after a redeploy fails loudly).
  let checkout = cfg.checkout || null;
  if (cfg.ashwings) {
    const fromChain = await pub.readContract({ address: cfg.ashwings, abi: ASHWINGS_ABI, functionName: 'zecCheckout' });
    if (checkout && checkout.toLowerCase() !== fromChain.toLowerCase()) {
      throw new Error(`CHECKOUT ${checkout} is not Ashwings(${cfg.ashwings}).zecCheckout() = ${fromChain}`);
    }
    checkout = fromChain;
  }
  if (!checkout) throw new Error('set CHECKOUT or ASHWINGS');
  const code = await pub.getCode({ address: checkout });
  if (!code || code === '0x') throw new Error(`no contract at CHECKOUT ${checkout}`);

  const account = privateKeyToAccount(cfg.relayerKey);
  const wallet = createWalletClient({ chain, transport, account });

  let tail = Promise.resolve();
  const locked = (fn) => {
    const run = tail.then(fn, fn);
    tail = run.catch(() => {});
    return run;
  };

  /**
   * Simulate (a call that would revert costs no gas), then send
   * (serialized). Returns once the transaction is broadcast; `mined`
   * resolves with its decoded logs.
   */
  async function broadcast(functionName, args) {
    const { request, result } = await pub.simulateContract({
      address: checkout, abi: CHECKOUT_ABI, functionName, args, account,
    });
    const hash = await locked(() => wallet.writeContract(request));
    const mined = pub.waitForTransactionReceipt({ hash, timeout: cfg.receiptTimeoutMs ?? 300_000 }).then((receipt) => {
      if (receipt.status !== 'success') throw new Error(`${functionName} reverted in ${hash}`);
      return { receipt, logs: parseEventLogs({ abi: CHECKOUT_ABI, logs: receipt.logs }) };
    });
    mined.catch(() => {}); // callers that don't wait must not crash the process
    return { hash, result, mined };
  }

  /** broadcast() and wait for the receipt. */
  async function send(functionName, args) {
    const { hash, result, mined } = await broadcast(functionName, args);
    const { logs } = await mined;
    return { hash, result, logs };
  }

  return {
    chainId,
    checkout,
    address: account.address,
    pub,
    broadcast,
    send,
    balance: () => pub.getBalance({ address: account.address }),
    receipt: async (hash) => {
      const receipt = await pub.getTransactionReceipt({ hash }).catch(() => null);
      if (!receipt) return null;
      return { receipt, logs: parseEventLogs({ abi: CHECKOUT_ABI, logs: receipt.logs }) };
    },
    reservation: async (id) => {
      const [recipient, quoteZat, minConf, payeeP2sh, filled, payeeHash, reservedAt, window] =
        await pub.readContract({ address: checkout, abi: CHECKOUT_ABI, functionName: 'reservations', args: [id] });
      return { recipient, quoteZat, minConf, payeeP2sh, filled, payeeHash, reservedAt, window };
    },
    anchor: async () => (await pub.readContract({ address: ZCASH_PRECOMPILE, abi: ZCASH_ABI, functionName: 'anchor' }))[0],
    simulateClaim: (id, txid, vout) =>
      pub.simulateContract({
        address: checkout, abi: CHECKOUT_ABI, functionName: 'claim', args: [id, txid, vout], account,
      }),
  };
}
