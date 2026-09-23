// Just enough of zebrad's JSON-RPC for the watcher: getblockcount,
// getaddresstxids, getrawtransaction (verbose). Shapes follow zebrad 2.x.
import http from 'node:http';

export function fakeZebrad() {
  const state = { tip: 0, txs: new Map() };
  const methods = {
    getblockcount: () => state.tip,
    getaddresstxids: ([{ addresses, start = 0, end = state.tip }]) =>
      [...state.txs.values()]
        .filter((t) => t.height >= start && t.height <= end && t.height <= state.tip)
        .filter((t) => t.vout.some((o) => addresses.some((a) => o.scriptPubKey.addresses.includes(a))))
        .map((t) => t.txid),
    getrawtransaction: ([txid]) => {
      const t = state.txs.get(txid);
      if (!t || t.height > state.tip) throw new Error('No such mempool or main chain transaction');
      return { ...t, confirmations: state.tip - t.height + 1 };
    },
  };
  const server = http.createServer(async (req, res) => {
    let raw = '';
    for await (const c of req) raw += c;
    const { id, method, params } = JSON.parse(raw);
    let body;
    try {
      if (!methods[method]) throw new Error(`Method not found: ${method}`);
      body = { jsonrpc: '2.0', id, result: methods[method](params || []) };
    } catch (e) {
      body = { jsonrpc: '2.0', id, error: { code: -5, message: e.message } };
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  });
  return {
    state,
    server,
    /** outputs: [{ value (zat bigint), script (hex), addr }] */
    addTx(txid, height, outputs) {
      state.txs.set(txid, {
        txid, height, version: 5,
        vout: outputs.map((o, n) => ({
          n, value: Number(o.value) / 1e8, valueZat: Number(o.value),
          scriptPubKey: { hex: o.script, addresses: [o.addr], type: 'pubkeyhash' },
        })),
      });
    },
  };
}
