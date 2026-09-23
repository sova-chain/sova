// Sova side: viem clients, the checkout ABI, and a serialized sender so
// concurrent requests never race on the relayer's nonce.
import {
  BaseError, ContractFunctionRevertedError, createPublicClient, createWalletClient, defineChain, http,
  parseAbi, parseEventLogs,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';

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

/** Custom error name of a revert, or null if it wasn't a contract revert. */
export function errorName(e) {
  if (!(e instanceof BaseError)) return null;
  const r = e.walk((x) => x instanceof ContractFunctionRevertedError);
  return r?.data?.errorName ?? (r ? 'reverted' : null);
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
  const transport = http(cfg.sovaRpc);
  const chainId = await createPublicClient({ transport }).getChainId();
  const chain = defineChain({
    id: chainId,
    name: 'Sova',
    nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 },
    rpcUrls: { default: { http: [cfg.sovaRpc] } },
  });
  const pub = createPublicClient({ chain, transport, pollingInterval: 500 });
  const account = privateKeyToAccount(cfg.relayerKey);
  const wallet = createWalletClient({ chain, transport, account });

  let tail = Promise.resolve();
  const locked = (fn) => {
    const run = tail.then(fn, fn);
    tail = run.catch(() => {});
    return run;
  };

  /** Simulate, then send (serialized), then wait for the receipt. */
  async function send(functionName, args) {
    const { request, result } = await pub.simulateContract({
      address: cfg.checkout, abi: CHECKOUT_ABI, functionName, args, account,
    });
    const hash = await locked(() => wallet.writeContract(request));
    const receipt = await pub.waitForTransactionReceipt({ hash, timeout: 180_000 });
    if (receipt.status !== 'success') throw new Error(`${functionName} reverted in ${hash}`);
    const logs = parseEventLogs({ abi: CHECKOUT_ABI, logs: receipt.logs });
    return { hash, result, logs };
  }

  return {
    chainId,
    address: account.address,
    pub,
    send,
    reservation: async (id) => {
      const [recipient, quoteZat, minConf, payeeP2sh, filled, payeeHash, reservedAt, window] =
        await pub.readContract({ address: cfg.checkout, abi: CHECKOUT_ABI, functionName: 'reservations', args: [id] });
      return { recipient, quoteZat, minConf, payeeP2sh, filled, payeeHash, reservedAt, window };
    },
    anchor: async () => (await pub.readContract({ address: ZCASH_PRECOMPILE, abi: ZCASH_ABI, functionName: 'anchor' }))[0],
    simulateClaim: (id, txid, vout) =>
      pub.simulateContract({
        address: cfg.checkout, abi: CHECKOUT_ABI, functionName: 'claim', args: [id, txid, vout], account,
      }),
  };
}
