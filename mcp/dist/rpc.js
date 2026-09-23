// Minimal JSON-RPC client for a zebrad-compatible node, used only for the
// regtest-only `sova_fund_regtest` convenience tool (the miner CLI itself
// speaks RPC internally for everything else -- see crates/burn-wallet's
// RpcClient -- we don't reimplement that, we only need one call the CLI has
// no subcommand for: minting regtest coinbase to an arbitrary address).
export class RpcCallError extends Error {
    method;
    rpcError;
    constructor(method, rpcError) {
        super(`RPC ${method} failed: [${rpcError.code}] ${rpcError.message}`);
        this.method = method;
        this.rpcError = rpcError;
        this.name = "RpcCallError";
    }
}
/** POSTs a single JSON-RPC 1.0-style request, as zebrad expects. */
export async function callRpc(rpcUrl, method, params = [], timeoutMs = 15_000) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
        const res = await fetch(rpcUrl.endsWith("/") ? rpcUrl : `${rpcUrl}/`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
                jsonrpc: "2.0",
                id: `sova-mcp-${method}`,
                method,
                params,
            }),
            signal: controller.signal,
        });
        if (!res.ok) {
            throw new Error(`RPC ${method} HTTP ${res.status} ${res.statusText} from ${rpcUrl}`);
        }
        const body = (await res.json());
        if (body.error) {
            throw new RpcCallError(method, body.error);
        }
        return body.result;
    }
    finally {
        clearTimeout(timer);
    }
}
export async function getBlockCount(rpcUrl) {
    return callRpc(rpcUrl, "getblockcount");
}
/** Regtest-only: mints `count` blocks with coinbase paid to `address`. */
export async function generateToAddress(rpcUrl, count, address) {
    return callRpc(rpcUrl, "generatetoaddress", [count, address]);
}
//# sourceMappingURL=rpc.js.map