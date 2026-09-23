#!/usr/bin/env python3
"""Print the enode URL of a reth node key, without starting the node.

Usage: enode.py <discovery-secret file> <ip> <port>

The file is what reth reads from `<datadir>/discovery-secret` (64 hex
chars, optionally 0x-prefixed). The enode id is the uncompressed secp256k1
public key without its 0x04 prefix. Only the enode (public) is printed;
the key never leaves this process. Pure Python, no dependencies.
"""

import ipaddress
import sys

P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % P == 0:
        return None
    if a == b:
        lam = 3 * a[0] * a[0] * pow(2 * a[1], -1, P) % P
    else:
        lam = (b[1] - a[1]) * pow(b[0] - a[0], -1, P) % P
    x = (lam * lam - a[0] - b[0]) % P
    return (x, (lam * (a[0] - x) - a[1]) % P)


def mul(k, point):
    acc = None
    while k:
        if k & 1:
            acc = add(acc, point)
        point = add(point, point)
        k >>= 1
    return acc


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    raw = open(sys.argv[1], encoding="ascii").read().strip()
    raw = raw[2:] if raw.startswith("0x") else raw
    if len(raw) != 64:
        sys.exit("key file must hold 64 hex chars")
    k = int(raw, 16)
    if not 0 < k < N:
        sys.exit("key out of range")
    ip = ipaddress.ip_address(sys.argv[2])
    port = int(sys.argv[3])
    x, y = mul(k, G)
    host = f"[{ip}]" if ip.version == 6 else str(ip)
    print(f"enode://{x:064x}{y:064x}@{host}:{port}")


if __name__ == "__main__":
    main()
