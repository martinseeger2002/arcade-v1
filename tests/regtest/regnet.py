#!/usr/bin/env python3
"""Consensus harness: Arcade V1.0 against a real Pepecoin Core, block for block.

A Pepecoin Core runs in -regtest. Each scenario makes blocks: Core mines good ones, this script
builds the rest by hand (bad blocks and competing branches, with real scrypt proof of work and
real AuxPoW). Every block goes to Core (submitblock) and to V1 (examples/regtest_follow, the real
Syncer reading the same blocks from a file). After each scenario Core's best block and V1's tip
must be the same block.

Run it off the network (nothing here needs the Internet; Core listens on localhost only):
    unshare -Urn sh -c 'ip link set lo up; python3 tests/regtest/regnet.py'
Options: --pepecoind PATH (default: pepecoind on PATH, then ~/.local/bin/pepecoind),
         --follow PATH (default: target/release/examples/regtest_follow), --keep (keep the work dir).

Scenarios marked "gap" are rules V1 does not check yet: they are reported, and don't fail
the run. Since step 3 there are none for Pepecoin. Every other scenario must match, or the run fails.
"""
import argparse, hashlib, json, os, shutil, struct, subprocess, sys, tempfile, time, urllib.request

COIN = 100_000_000
BITS = 0x207FFFFF
CHAIN_ID = 0x3F
AUXPOW_START = 20
VERSION_AUXPOW = 0x100
OP_TRUE = b"\x51"

# ---------------------------------------------------------------- encoding

def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()

def scrypt(b):
    return hashlib.scrypt(b, salt=b, n=1024, r=1, p=1, dklen=32)

def varint(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)

def push(b):
    if len(b) < 0x4C:
        return bytes([len(b)]) + b
    assert len(b) < 0x100
    return b"\x4c" + bytes([len(b)]) + b

def script_num(n):
    out = b""
    while n:
        out += bytes([n & 0xFF])
        n >>= 8
    if out and out[-1] & 0x80:
        out += b"\x00"
    return push(out) if out else b"\x00"

def tx(inputs, outputs, version=1, witness=None, lock_time=0, sequence=0xFFFFFFFF):
    """inputs: [(txid_internal, vout, script_sig)], outputs: [(value, script)].
    With `witness` (a list of stacks, one per input) the tx is witness-serialized."""
    body_in = varint(len(inputs)) + b"".join(t + struct.pack("<I", v) + varint(len(s)) + s + struct.pack("<I", sequence) for t, v, s in inputs)
    body_out = varint(len(outputs)) + b"".join(struct.pack("<q", v) + varint(len(s)) + s for v, s in outputs)
    lt = struct.pack("<I", lock_time)
    stripped = struct.pack("<i", version) + body_in + body_out + lt
    if witness is None:
        return stripped, sha256d(stripped)
    wit = b"".join(varint(len(st)) + b"".join(varint(len(x)) + x for x in st) for st in witness)
    full = struct.pack("<i", version) + b"\x00\x01" + body_in + body_out + wit + lt
    return full, sha256d(stripped)

class Key:
    """A secp256k1 key (Python's cryptography package): P2PK scripts and legacy SIGHASH_ALL signatures."""
    def __init__(self):
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.hazmat.primitives import serialization
        self.k = ec.generate_private_key(ec.SECP256K1())
        self.pub = self.k.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.CompressedPoint)
        self.script = push(self.pub) + b"\xac"  # <pubkey> OP_CHECKSIG

    def sign(self, digest):
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec, utils
        return self.k.sign(digest, ec.ECDSA(utils.Prehashed(hashes.SHA256())))

def sighash(inputs, outputs, i, script_code, lock_time=0, sequence=0xFFFFFFFF):
    """Legacy SIGHASH_ALL: input i's script is the coin's script, the others empty."""
    ins = [(t, v, script_code if j == i else b"") for j, (t, v, _) in enumerate(inputs)]
    raw, _ = tx(ins, outputs, lock_time=lock_time, sequence=sequence)
    return sha256d(raw + struct.pack("<I", 1))

def signed(key, inputs, outputs, lock_time=0, sequence=0xFFFFFFFF, mangle=None):
    """A tx whose inputs (each a coin paying key.script) are signed by `key` (P2PK)."""
    sigs = []
    for i in range(len(inputs)):
        der = key.sign(sighash(inputs, outputs, i, key.script, lock_time, sequence))
        if mangle:
            der = mangle(der)
        sigs.append(push(der + b"\x01"))
    return tx([(t, v, sigs[i]) for i, (t, v, _) in enumerate(inputs)], outputs, lock_time=lock_time, sequence=sequence)

def loose_der(der):
    """The same signature with a needless leading zero in R: valid BER, not strict DER (BIP66)."""
    rlen = der[3]
    r = der[4:4 + rlen]
    rest = der[4 + rlen:]
    r2 = b"\x00" + r
    body = b"\x02" + bytes([len(r2)]) + r2 + rest
    return b"\x30" + bytes([len(body)]) + body

def coinbase(height, value, extra=b"", outputs=None):
    script = script_num(height) + push(b"regnet" + extra)
    return tx([(b"\x00" * 32, 0xFFFFFFFF, script)], outputs or [(value, OP_TRUE)])

def merkle(ids):
    ids = list(ids)
    while len(ids) > 1:
        if len(ids) % 2:
            ids.append(ids[-1])
        ids = [sha256d(ids[i] + ids[i + 1]) for i in range(0, len(ids), 2)]
    return ids[0]

def target(bits):
    return (bits & 0xFFFFFF) << (8 * ((bits >> 24) - 3))

def header(version, prev, mroot, t, nonce=0):
    return struct.pack("<i", version) + prev + mroot + struct.pack("<III", t, BITS, nonce)

def grind(h80):
    """Find a nonce whose scrypt hash meets BITS (half of all hashes do)."""
    for nonce in range(1 << 20):
        h = h80[:76] + struct.pack("<I", nonce)
        if int.from_bytes(scrypt(h), "little") <= target(BITS):
            return h
    raise RuntimeError("no nonce")

def block(prev_hash, height, t, txs, version=None, auxpow=None, chain_id=CHAIN_ID):
    """txs: [(raw, txid)]. auxpow None = by height (merge-mined from AUXPOW_START)."""
    if auxpow is None:
        auxpow = height >= AUXPOW_START
    if version is None:
        version = (chain_id << 16) | 4 | (VERSION_AUXPOW if auxpow else 0)
    mroot = merkle([i for _, i in txs])
    body = varint(len(txs)) + b"".join(r for r, _ in txs)
    if not auxpow:
        h = grind(header(version, prev_hash, mroot, t))
        return h + body
    h = header(version, prev_hash, mroot, t)
    bhash = sha256d(h)
    # Parent coinbase: the merged-mining marker, our block hash (big-endian), tree size 1, nonce 0.
    pscript = b"\xfa\xbemm" + bhash[::-1] + struct.pack("<II", 1, 0)
    praw, pid = tx([(b"\x00" * 32, 0xFFFFFFFF, push(b"parent") + pscript)], [(0, OP_TRUE)])
    parent = grind(header(1, b"\x00" * 32, pid, t))
    aux = praw + sha256d(parent) + varint(0) + struct.pack("<i", 0) + varint(0) + struct.pack("<i", 0) + parent
    return h + aux + body

def bhash(raw):
    return sha256d(raw[:80])

def p2pkh_address(h160, version=111):
    """Base58Check P2PKH address (regtest version 111)."""
    b = bytes([version]) + h160
    b += sha256d(b)[:4]
    n = int.from_bytes(b, "big")
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = alphabet[r] + out
    return "1" * (len(b) - len(b.lstrip(b"\x00"))) + out

def hexid(h):
    return h[::-1].hex()

# ---------------------------------------------------------------- Core

class Core:
    def __init__(self, exe, work):
        self.dir = os.path.join(work, "core")
        os.makedirs(self.dir)
        self.port = 18443
        self.proc = subprocess.Popen([exe, "-regtest", f"-datadir={self.dir}", "-server", "-listen=0", "-disablewallet",
                                      "-rpcuser=regnet", "-rpcpassword=regnet", f"-rpcport={self.port}", "-rpcbind=127.0.0.1",
                                      "-rpcallowip=127.0.0.1", "-printtoconsole=0"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(120):
            try:
                self.call("getblockcount")
                return
            except Exception:
                time.sleep(0.5)
        raise RuntimeError("pepecoind did not start")

    def call(self, method, *params):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}/", json.dumps({"id": 1, "method": method, "params": list(params)}).encode(),
                                     {"Authorization": "Basic cmVnbmV0OnJlZ25ldA==", "Content-Type": "application/json"})
        try:
            r = json.load(urllib.request.urlopen(req, timeout=120))
        except urllib.error.HTTPError as e:
            r = json.load(e)
        if r.get("error"):
            raise RuntimeError(f"{method}: {r['error']}")
        return r["result"]

    def stop(self):
        try:
            self.call("stop")
            self.proc.wait(30)
        except Exception:
            self.proc.kill()

# ---------------------------------------------------------------- the run

class Net:
    def __init__(self, core, follow, work):
        self.core, self.follow, self.work = core, follow, work
        self.blocks = os.path.join(work, "blocks.hex")
        self.db = os.path.join(work, "v1-chain.mdbx")
        self.addr = p2pkh_address(b"\x11" * 20)  # nobody's key: these coins are never spent
        self.results = []
        self.seen_errors = set()
        self.t = 1_700_000_000
        self.height = {}  # block hash -> height, for blocks we know
        open(self.blocks, "w").close()

    def now(self):
        self.t += 1
        return self.t

    def give_v1(self, raw):
        with open(self.blocks, "a") as f:
            f.write(raw.hex() + "\n")

    def from_core(self):
        """Hand V1 every block Core has that V1 hasn't been given (Core's own chain)."""
        n = self.core.call("getblockcount")
        for h in range(n + 1):
            bh = self.core.call("getblockhash", h)
            if bh not in self.height:
                raw = bytes.fromhex(self.core.call("getblock", bh, False))
                self.give_v1(raw)
                self.height[bh] = h
                self.t = max(self.t, struct.unpack("<I", raw[68:72])[0])

    def mine(self, n, auxpow):
        self.core.call("generatetoaddress", n, self.addr, 1_000_000, 1 if auxpow else 0)
        self.from_core()

    def submit(self, raw, height):
        """Give a hand-built block to both; Core's answer (None = accepted)."""
        res = self.core.call("submitblock", raw.hex())
        self.give_v1(raw)
        self.height[hexid(bhash(raw))] = height
        return res

    def tip(self):
        h = self.core.call("getbestblockhash")
        return bytes.fromhex(h)[::-1], self.core.call("getblockcount")

    def v1(self):
        out = subprocess.run([self.follow, self.db, self.blocks], capture_output=True, text=True, timeout=600)
        line = [l for l in out.stdout.splitlines() if l.startswith("{")]
        if out.returncode != 0 or not line:
            raise RuntimeError(f"regtest_follow failed: {out.stderr[-2000:]}")
        return json.loads(line[-1])

    def check(self, name, what, gap=False, notes=""):
        core_best = self.core.call("getbestblockhash")
        core_h = self.core.call("getblockcount")
        v = self.v1()
        same = v["tip"] == core_best
        status = "MATCH" if same else ("GAP" if gap else "FAIL")
        self.results.append((status, name))
        print(f"\n[{status}] {name}\n    {what}")
        if notes:
            print(f"    Core: {notes}")
        print(f"    Core best {core_best[:16]}… at {core_h}; V1 tip {v['tip'][:16]}… at {v['height']} (best header at {v['best_height']})")
        for e in v["errors"]:
            if e not in self.seen_errors:
                print(f"    V1: {e[:220]}")
        self.seen_errors.update(v["errors"])
        return same

def run(a):
    work = tempfile.mkdtemp(prefix="regnet-")
    core = Core(a.pepecoind, work)
    net = Net(core, a.follow, work)
    try:
        scenarios(net)
    finally:
        core.stop()
        if not a.keep:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print(f"\nwork dir kept: {work}")
    fails = [n for s, n in net.results if s == "FAIL"]
    gaps = [n for s, n in net.results if s == "GAP"]
    print(f"\n{len(net.results)} scenarios: {len(net.results) - len(fails) - len(gaps)} match, {len(gaps)} known gaps, {len(fails)} FAIL")
    return 1 if fails else 0

def on_tip(net, txs_fn, **kw):
    """Build a block on Core's tip; txs_fn(height) -> [(raw, txid)]."""
    tip, h = net.tip()
    return block(tip, h + 1, net.now(), txs_fn(h + 1), **kw), h + 1

def scenarios(net):
    good = lambda h, x=b"": [coinbase(h, 1 * COIN, x)]

    # 1. Plain chain before merge mining.
    net.mine(10, auxpow=False)
    net.check("Core mines 10 blocks", "both follow the same chain")

    # 2. A merge-mined block before merge mining starts.
    raw, h = on_tip(net, good, auxpow=True)
    r = net.submit(raw, h)
    net.check("Merge-mined block at height 11 (merge mining starts at 20)", "Core: early-auxpow-block", notes=r)

    # 3. Across the merge-mining start, merge-mined and not.
    # (Core's own miner can't make merge-mined blocks before 20 either: up to 19 without.)
    net.mine(AUXPOW_START - 1 - 10, auxpow=False)
    net.mine(4, auxpow=True)
    net.mine(3, auxpow=False)
    net.check("Core mines past height 20, with and without AuxPoW", "both follow")

    # 4. A legacy (version 1) block after the start.
    raw, h = on_tip(net, good, version=1, auxpow=False)
    r = net.submit(raw, h)
    net.check("Version-1 block after merge mining started", "Core: late-legacy-block", notes=r)

    # 5. Merge-mined, with another chain's id in the version.
    raw, h = on_tip(net, good, chain_id=0x40)
    r = net.submit(raw, h)
    net.check("Merge-mined block carrying chain id 64, not 63", "Core: refuses it (its log: block does not have our chain ID; reject code high-hash)", notes=r)

    # 6. A good hand-built merge-mined block (the builder makes what Core takes).
    raw, h = on_tip(net, lambda h: good(h, b"ok"))
    r = net.submit(raw, h)
    net.check("Hand-built merge-mined block", "Core takes it; so must V1", notes=r)

    # 7. Witness data on a chain without SegWit.
    def witness_txs(h):
        cb = coinbase(h, 1 * COIN, b"w")
        spend = tx([(cb[1], 0, b"")], [(1, OP_TRUE)], witness=[[b"\x01" * 33]])
        return [cb, spend]
    raw, h = on_tip(net, witness_txs)
    r = net.submit(raw, h)
    net.check("Block with a witness-serialized transaction", "Core: unexpected-witness (Pepecoin has no SegWit)", notes=r)

    # 8. The reorg trap: a heavier branch with a block that can't be connected in it.
    tip, th = net.tip()
    fork = bytes.fromhex(net.core.call("getblockhash", th - 1))[::-1]
    b1 = block(fork, th, net.now(), good(th, b"b1"))
    ghost = tx([(b"\x07" * 32, 0, b"")], [(1, OP_TRUE)])
    b2 = block(bhash(b1), th + 1, net.now(), good(th + 1, b"b2") + [ghost])
    b3 = block(bhash(b2), th + 2, net.now(), good(th + 2, b"b3"))
    b4 = block(bhash(b3), th + 3, net.now(), good(th + 3, b"b4"))
    rs = [net.submit(x, hh) for x, hh in ((b1, th), (b2, th + 1), (b3, th + 2), (b4, th + 3))]
    net.check("Heavier branch whose 2nd block spends a coin that doesn't exist",
              "Core marks it invalid and stays; V1 must not get stuck on the branch", notes=str(rs))

    # 9. A valid heavier branch: both move to it.
    tip, th = net.tip()
    fork = bytes.fromhex(net.core.call("getblockhash", th - 1))[::-1]
    prev, rs = fork, []
    for i in range(3):
        x = block(prev, th + i, net.now(), good(th + i, b"c%d" % i))
        rs.append(net.submit(x, th + i))
        prev = bhash(x)
    net.check("Valid branch, 1 block deeper than the tip, 2 longer", "both reorg to it", notes=str(rs))

    # 10. More blocks from Core on top: both follow.
    net.mine(5, auxpow=True)
    net.check("Core mines 5 more", "both follow")

    # ---- coin rules (step 2) ----
    def heal(name):
        net.mine(2, auxpow=True)
        net.check(f"{name}: Core mines 2 more", "V1 must come back to Core's chain")

    raw, h = on_tip(net, lambda h: [coinbase(h, 10_000_000 * COIN, b"rich")])
    r = net.submit(raw, h)
    net.check("Coinbase paying 10,000,000 PEP", "Core: bad-cb-amount", notes=r)
    heal("After the overpaid coinbase")

    raw, h = on_tip(net, lambda h: good(h, b"one") + [coinbase(h, 1, b"two")])
    r = net.submit(raw, h)
    net.check("Two coinbases in one block", "Core: bad-cb-multiple", notes=r)
    heal("After two coinbases")

    tip, th = net.tip()
    young = bytes.fromhex(net.core.call("getblock", net.core.call("getblockhash", th), True)["tx"][0])[::-1]
    raw, h = on_tip(net, lambda h: good(h, b"sp") + [tx([(young, 0, b"")], [(50_000_000 * COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("Unsigned spend of a 1-block-old coinbase, paying out more than it holds", "Core: refuses it (maturity, value, script)", notes=r)
    heal("After the bad spend")

    raw, h = on_tip(net, lambda h: [tx([(b"\x00" * 32, 0xFFFFFFFF, b"\x51")], [(1 * COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("Coinbase script of 1 byte", "Core: bad-cb-length", notes=r)

    # A coin of our own that anyone can spend (OP_TRUE): spends of it need no signature, so
    # Core takes them, and fees, maturity and amounts can be tried exactly.
    mine = coinbase(net.tip()[1] + 1, 1000 * COIN, b"mine")
    raw, h0 = on_tip(net, lambda h: [mine])
    r = net.submit(raw, h0)
    net.check("A coinbase of our own: 1,000 PEP anyone can spend", "both take it", notes=r)
    net.mine(h0 + 58 - net.tip()[1], auxpow=True)
    spend = tx([(mine[1], 0, b"")], [(999 * COIN, OP_TRUE)])  # 1 PEP fee
    raw, h = on_tip(net, lambda h: good(h, b"p59") + [spend])
    r = net.submit(raw, h)
    net.check(f"Spending it {h - h0} blocks later (maturity 60)", "Core: bad-txns-premature-spend-of-coinbase", notes=r)
    net.mine(1, auxpow=True)

    over = tx([(mine[1], 0, b"")], [(1001 * COIN, OP_TRUE)])
    raw, h = on_tip(net, lambda h: good(h, b"over") + [over])
    r = net.submit(raw, h)
    net.check(f"Spending it {h - h0} blocks later, paying out 1,001 of its 1,000 PEP", "Core: bad-txns-in-belowout", notes=r)

    sub = lambda h: (500_000 * COIN) >> (h // 150) if h < 900 else 10_000 * COIN
    raw, h = on_tip(net, lambda h: [coinbase(h, sub(h) + 1 * COIN + 1, b"fee+1"), spend])
    r = net.submit(raw, h)
    net.check("Coinbase claiming the subsidy plus the 1 PEP fee, plus 1 unit", "Core: bad-cb-amount", notes=r)

    raw, h = on_tip(net, lambda h: [coinbase(h, sub(h) + 1 * COIN, b"fee"), spend])
    r = net.submit(raw, h)
    net.check(f"Spending it {h - h0} blocks later; coinbase claims the subsidy plus exactly the fee", "both take it", notes=r)

    # A mutated copy (CVE-2012-2459): the last transaction repeated, same merkle root. It must
    # be refused as a damaged copy, not taken, and not mark the real block invalid.
    a = tx([(spend[1], 0, b"")], [(998 * COIN, OP_TRUE)])
    b = tx([(a[1], 0, b"")], [(997 * COIN, OP_TRUE)])
    real, h = on_tip(net, lambda h: [coinbase(h, sub(h) + 2 * COIN, b"mut"), a, b])
    txs_len = len(varint(3)) + sum(len(x[0]) for x in ([coinbase(h, sub(h) + 2 * COIN, b"mut"), a, b]))
    mutated = real[:len(real) - txs_len] + varint(4) + real[len(real) - txs_len + 1:] + b[0]
    r = net.submit(mutated, h)
    net.check("A mutated copy of a block (its last transaction twice, same merkle root)", "Core: refuses the copy (bad-txns-duplicate)", notes=r)
    r = net.submit(real, h)
    net.check("Then the real block", "both take it: the damaged copy didn't mark it", notes=r)

    # ---- scripts, signatures and the rest of Core's block rules (step 3) ----
    tip, th = net.tip()
    old = net.core.call("getblockhash", th - 70)
    cbid = net.core.call("getblock", old, True)["tx"][0]
    value = round(net.core.call("gettxout", cbid, 0)["value"] * COIN)
    raw, h = on_tip(net, lambda h: good(h, b"nosig") + [tx([(bytes.fromhex(cbid)[::-1], 0, b"")], [(value - COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("Unsigned spend of a matured coinbase that needs a signature (amounts all right)", "Core: script check fails", notes=r)
    heal("After the unsigned spend")

    # Coins of our own behind real keys: 4 x 1,000 PEP to a key (P2PK), and one locked by
    # CHECKLOCKTIMEVERIFY until height 1,000 (a NOP before BIP65, enforced from 1,351 here).
    key, other = Key(), Key()
    cltv = b"\x02\xe8\x03" + b"\xb1\x75" + OP_TRUE  # <1000> OP_CHECKLOCKTIMEVERIFY OP_DROP OP_TRUE
    kc = coinbase(net.tip()[1] + 1, 0, b"keys", outputs=[(1000 * COIN, key.script)] * 4 + [(1000 * COIN, cltv)])
    raw, hk = on_tip(net, lambda h: [kc])
    r = net.submit(raw, hk)
    net.check("A coinbase of our own paying 4 coins to a key and 1 locked by CHECKLOCKTIMEVERIFY", "both take it", notes=r)
    net.mine(hk + 60 - net.tip()[1], auxpow=True)
    coin = lambda v: (kc[1], v, b"")

    ok = signed(key, [coin(0)], [(999 * COIN, OP_TRUE)])
    raw, h = on_tip(net, lambda h: good(h, b"sig") + [ok])
    r = net.submit(raw, h)
    net.check("A correctly signed spend", "both take it", notes=r)

    raw, h = on_tip(net, lambda h: good(h, b"wrong") + [signed(other, [coin(1)], [(999 * COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("A spend signed by the wrong key", "Core: script check fails", notes=r)

    late = signed(key, [coin(1)], [(999 * COIN, OP_TRUE)], lock_time=net.tip()[1] + 500, sequence=0)
    raw, h = on_tip(net, lambda h: good(h, b"late") + [late])
    r = net.submit(raw, h)
    net.check("A transaction locked until 500 blocks from now", "Core: bad-txns-nonfinal", notes=r)

    raw, h = on_tip(net, lambda h: [coinbase(h, 1 * COIN, b"ops", outputs=[(1 * COIN, b"\xac" * 20_001)])])
    r = net.submit(raw, h)
    net.check("A block with 20,001 signature operations", "Core: bad-blk-sigops", notes=r)

    raw, h = on_tip(net, lambda h: [coinbase(h, 1 * COIN, b"big", outputs=[(0, b"\x6a" + b"\x00" * 1_000_000)])])
    r = net.submit(raw, h)
    net.check("A block over 1,000,000 bytes", "Core: bad-blk-length", notes=r)

    # Past DERSIG (1,251) and CHECKLOCKTIMEVERIFY and version 4 (1,351).
    net.mine(1360 - net.tip()[1], auxpow=True)
    net.check("Core mines to height 1,360", "both follow")

    raw, h = on_tip(net, lambda h: good(h, b"v3"), version=(CHAIN_ID << 16) | VERSION_AUXPOW | 3)
    r = net.submit(raw, h)
    net.check("A version-3 block after 1,351", "Core: bad-version", notes=r)

    raw, h = on_tip(net, lambda h: good(h, b"ber") + [signed(key, [coin(1)], [(999 * COIN, OP_TRUE)], mangle=loose_der)])
    r = net.submit(raw, h)
    net.check("A signature that isn't strict DER, after 1,251", "Core: script check fails (DERSIG)", notes=r)

    raw, h = on_tip(net, lambda h: good(h, b"cltv0") + [tx([coin(4)], [(999 * COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("Spending the CHECKLOCKTIMEVERIFY coin with no lock time, after 1,351", "Core: script check fails (CLTV)", notes=r)

    raw, h = on_tip(net, lambda h: good(h, b"cltv1") + [tx([coin(4)], [(999 * COIN, OP_TRUE)], lock_time=1000, sequence=0xFFFFFFFE)])
    r = net.submit(raw, h)
    net.check("Spending it with lock time 1,000", "both take it", notes=r)

    raw, h = on_tip(net, lambda h: good(h, b"sig2") + [signed(key, [coin(2), coin(3)], [(1999 * COIN, OP_TRUE)])])
    r = net.submit(raw, h)
    net.check("A correctly signed spend of two coins, after 1,351", "both take it", notes=r)

def main():
    ap = argparse.ArgumentParser()
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--pepecoind", default=shutil.which("pepecoind") or os.path.expanduser("~/.local/bin/pepecoind"))
    ap.add_argument("--follow", default=os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(root, "target")), "release/examples/regtest_follow"))
    ap.add_argument("--keep", action="store_true")
    sys.exit(run(ap.parse_args()))

if __name__ == "__main__":
    main()
