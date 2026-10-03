#!/usr/bin/env python3
"""A zebrad JSON-RPC proxy that can freeze one node's view of Zcash.

Used by box/sim/seed-stall-scenario.sh to give two Sova nodes that share one
regtest zebrad different views of the Zcash chain at chosen moments, the way
two real nodes with their own zebrads see a reorg at different times.

  live    every request is forwarded to the upstream zebrad and every
          successful answer is cached by (method, params).
  frozen  `getblockcount` answers the tip recorded when the freeze began;
          everything else is answered from the cache, and a request the
          cache can't answer gets a JSON-RPC error (the node's zebrad client
          reads that as "no such block"). So the node keeps seeing the chain
          exactly as it was at the freeze, whatever happens upstream.

Freezing first walks the upstream chain 0..tip through the cache
(`getblockhash [h]`, `getblock [hash, 1]`, `getrawtransaction [txid, 1]`:
the only calls `crates/consensus/src/zebrad.rs` makes), so a frozen node can
re-read any height it could see when it was frozen.

Control (POST, no body): /__freeze -> {"tip": N}, /__thaw -> {"ok": true}.

Usage: zebrad-freeze-proxy.py <listen_port> <upstream_url>
"""

import json
import sys
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LISTEN_PORT = int(sys.argv[1])
UPSTREAM = sys.argv[2].rstrip("/") + "/"

LOCK = threading.Lock()
CACHE = {}
STATE = {"frozen": False, "tip": None}


def key(method, params):
    return method + " " + json.dumps(params, sort_keys=True, separators=(",", ":"))


def upstream(method, params, req_id="proxy"):
    body = json.dumps({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}).encode()
    req = urllib.request.Request(UPSTREAM, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def cached_call(method, params):
    """Live call through the cache (used to pre-warm it at freeze time)."""
    reply = upstream(method, params)
    if reply.get("error") is None and "result" in reply:
        with LOCK:
            CACHE[key(method, params)] = reply["result"]
    return reply.get("result")


def freeze():
    tip = cached_call("getblockcount", [])
    for height in range(0, tip + 1):
        block_hash = cached_call("getblockhash", [height])
        if not block_hash:
            continue
        block = cached_call("getblock", [block_hash, 1])
        for txid in (block or {}).get("tx", []):
            cached_call("getrawtransaction", [txid, 1])
    with LOCK:
        STATE["frozen"] = True
        STATE["tip"] = tip
    return tip


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # quiet
        pass

    def reply(self, obj):
        data = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        if self.path == "/__freeze":
            self.reply({"tip": freeze()})
            return
        if self.path == "/__thaw":
            with LOCK:
                STATE["frozen"] = False
            self.reply({"ok": True})
            return
        try:
            req = json.loads(raw or b"{}")
        except ValueError:
            self.reply({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}})
            return
        method, params, req_id = req.get("method"), req.get("params", []), req.get("id")
        with LOCK:
            frozen, tip = STATE["frozen"], STATE["tip"]
            hit = CACHE.get(key(method, params), None) if frozen else None
            has = frozen and key(method, params) in CACHE
        if frozen:
            if method == "getblockcount":
                self.reply({"jsonrpc": "2.0", "id": req_id, "result": tip})
            elif has:
                self.reply({"jsonrpc": "2.0", "id": req_id, "result": hit})
            else:
                self.reply({"jsonrpc": "2.0", "id": req_id,
                            "error": {"code": -8, "message": "frozen proxy: not in the frozen view"}})
            return
        try:
            reply = upstream(method, params, req_id)
        except Exception as err:  # upstream down: look like a transport error
            self.send_response(502)
            self.end_headers()
            self.wfile.write(str(err).encode())
            return
        if reply.get("error") is None and "result" in reply:
            with LOCK:
                CACHE[key(method, params)] = reply["result"]
        self.reply(reply)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", LISTEN_PORT), Handler)
    server.daemon_threads = True
    server.serve_forever()
