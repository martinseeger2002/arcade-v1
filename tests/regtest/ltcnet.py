#!/usr/bin/env python3
"""Consensus harness, SegWit era: Arcade V1.0 against a real Litecoin Core, block for block.

The companion of regnet.py (Pepecoin) for the rules Bitcoin Core 0.13+ chains add: SegWit (witness
scripts, the witness commitment, block weight, witness signature-operation cost), NULLDUMMY,
Taproot (BIP340/341 key-path spends), CSV (BIP68 sequence locks, BIP112, BIP113 median-time lock
times), the full-version floor, and Litecoin's MWEB as V1 sees it (peg-ins, MWEB transfers and
peg-outs mined by Core; fees leaving through the HogEx).

Two runs of Litecoin Core in -regtest:
  A. MWEB held off (-vbparams): hand-built good and bad blocks, Core's verdict against V1's.
  B. MWEB as regtest has it: Core's wallet pegs in, moves coins inside MWEB and pegs out; V1 must
     follow every block. (Once MWEB is on every block needs a real extension block, so bad
     blocks are built in run A only.)
Every block goes to Core (submitblock) and to V1 (examples/regtest_follow ... ltcregtest). After
each scenario Core's best block and V1's tip must be the same block.

    cargo build --release -p arcade-chaindb --example regtest_follow
    unshare -Urn sh -c 'ip link set lo up; python3 tests/regtest/ltcnet.py'
Options: --litecoind PATH, --follow PATH, --keep.
"""
import argparse, hashlib, json, os, shutil, struct, subprocess, sys, tempfile, time, urllib.request

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from regnet import sha256d, scrypt, varint, push, script_num, tx, merkle, target, hexid, bhash, p2pkh_address, COIN, OP_TRUE  # noqa: E402

BITS = 0x207FFFFF
VERSION = 0x20000000
SUBSIDY_INTERVAL = 150

def subsidy(h):
    return (50 * COIN) >> (h // SUBSIDY_INTERVAL) if h // SUBSIDY_INTERVAL < 64 else 0

def sha256(b):
    return hashlib.sha256(b).digest()

# ---------------------------------------------------------------- secp256k1 (pure Python, for BIP340)

P = 2**256 - 2**32 - 977
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
     0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8)

def padd(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % P == 0:
        return None
    if a == b:
        lam = 3 * a[0] * a[0] * pow(2 * a[1], P - 2, P) % P
    else:
        lam = (b[1] - a[1]) * pow(b[0] - a[0], P - 2, P) % P
    x = (lam * lam - a[0] - b[0]) % P
    return (x, (lam * (a[0] - x) - a[1]) % P)

def pmul(k, pt=G):
    r = None
    while k:
        if k & 1:
            r = padd(r, pt)
        pt = padd(pt, pt)
        k >>= 1
    return r

def xonly(pt):
    return pt[0].to_bytes(32, "big")

def tagged(tag, msg):
    t = sha256(tag.encode())
    return sha256(t + t + msg)

def schnorr_sign(seckey, msg):
    """BIP340 signing (aux randomness zero)."""
    pt = pmul(seckey)
    d = seckey if pt[1] % 2 == 0 else N - seckey
    t = (d ^ int.from_bytes(tagged("BIP0340/aux", b"\x00" * 32), "big")).to_bytes(32, "big")
    k0 = int.from_bytes(tagged("BIP0340/nonce", t + xonly(pt) + msg), "big") % N
    r = pmul(k0)
    k = k0 if r[1] % 2 == 0 else N - k0
    e = int.from_bytes(tagged("BIP0340/challenge", xonly(r) + xonly(pt) + msg), "big") % N
    return xonly(r) + ((k + e * d) % N).to_bytes(32, "big")

class TapKey:
    """A key-path-only Taproot output (BIP341, no script tree)."""
    def __init__(self, seed):
        self.sk = int.from_bytes(sha256(seed), "big") % N
        p = pmul(self.sk)
        d = self.sk if p[1] % 2 == 0 else N - self.sk
        tweak = int.from_bytes(tagged("TapTweak", xonly(p)), "big")
        self.tweaked = (d + tweak) % N
        self.script = b"\x51\x20" + xonly(pmul(self.tweaked))

def taproot_sighash(version, inputs, outputs, lock_time, spent, i):
    """BIP341 SIGHASH_DEFAULT, key path, no annex. inputs [(txid, vout, sequence)], spent [(value, script)]."""
    sp = sha256(b"".join(t + struct.pack("<I", v) for t, v, _ in inputs))
    sa = sha256(b"".join(struct.pack("<q", v) for v, _ in spent))
    ss = sha256(b"".join(varint(len(s)) + s for _, s in spent))
    sq = sha256(b"".join(struct.pack("<I", q) for _, _, q in inputs))
    so = sha256(b"".join(struct.pack("<q", v) + varint(len(s)) + s for v, s in outputs))
    msg = b"\x00" + b"\x00" + struct.pack("<iI", version, lock_time) + sp + sa + ss + sq + so + b"\x00" + struct.pack("<I", i)
    return tagged("TapSighash", msg)

def bip143_sighash(version, inputs, outputs, lock_time, i, script_code, amount):
    """SegWit v0 SIGHASH_ALL. inputs [(txid, vout, sequence)]."""
    hp = sha256d(b"".join(t + struct.pack("<I", v) for t, v, _ in inputs))
    hs = sha256d(b"".join(struct.pack("<I", q) for _, _, q in inputs))
    ho = sha256d(b"".join(struct.pack("<q", v) + varint(len(s)) + s for v, s in outputs))
    t, v, q = inputs[i]
    pre = (struct.pack("<i", version) + hp + hs + t + struct.pack("<I", v) + varint(len(script_code)) + script_code
           + struct.pack("<q", amount) + struct.pack("<I", q) + ho + struct.pack("<I", lock_time) + struct.pack("<I", 1))
    return sha256d(pre)

class EcKey:
    """An ECDSA key (Python's cryptography package) for P2WSH <pubkey> OP_CHECKSIG coins."""
    def __init__(self):
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.hazmat.primitives import serialization
        self.k = ec.generate_private_key(ec.SECP256K1())
        self.pub = self.k.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.CompressedPoint)
        self.ws = push(self.pub) + b"\xac"
        self.script = p2wsh(self.ws)

    def sign(self, digest):
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec, utils
        der = self.k.sign(digest, ec.ECDSA(utils.Prehashed(hashes.SHA256())))
        # Low S (Core's policy only, but keep signatures canonical).
        from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature, encode_dss_signature
        r, s = decode_dss_signature(der)
        if s > N // 2:
            s = N - s
        return encode_dss_signature(r, s)

def p2wsh(ws):
    return b"\x00\x20" + sha256(ws)

# ---------------------------------------------------------------- transactions and blocks

def wtx(inputs, outputs, witness=None, version=1, lock_time=0):
    """inputs [(txid, vout, script_sig, sequence)]; returns (raw, txid, wtxid)."""
    body_in = varint(len(inputs)) + b"".join(t + struct.pack("<I", v) + varint(len(s)) + s + struct.pack("<I", q) for t, v, s, q in inputs)
    body_out = varint(len(outputs)) + b"".join(struct.pack("<q", v) + varint(len(s)) + s for v, s in outputs)
    lt = struct.pack("<I", lock_time)
    stripped = struct.pack("<i", version) + body_in + body_out + lt
    txid = sha256d(stripped)
    if witness is None or all(not w for w in witness):
        return stripped, txid, txid
    wit = b"".join(varint(len(st)) + b"".join(varint(len(x)) + x for x in st) for st in witness)
    full = struct.pack("<i", version) + b"\x00\x01" + body_in + body_out + wit + lt
    return full, txid, sha256d(full)

COMMIT_HEAD = b"\x6a\x24\xaa\x21\xa9\xed"

def ltc_block(prev, height, t, txs, fees=0, commitment="auto", version=VERSION, cb_value=None, cb_tag=b""):
    """txs [(raw, txid, wtxid)]. The coinbase pays the subsidy plus `fees` (or cb_value) to OP_TRUE.
    commitment: "auto" (when a tx has a witness), "always", "bad", or None."""
    has_wit = any(x[1] != x[2] for x in txs)
    want = commitment == "always" or commitment == "bad" or (commitment == "auto" and has_wit)
    value = cb_value if cb_value is not None else subsidy(height) + fees
    cb_script = script_num(height) + push(b"ltcnet" + cb_tag)
    outs = [(value, OP_TRUE)]
    nonce = b"\x00" * 32
    if want:
        root = merkle([b"\x00" * 32] + [x[2] for x in txs])
        c = sha256d(root + nonce)
        if commitment == "bad":
            c = bytes([c[0] ^ 1]) + c[1:]
        outs.append((0, COMMIT_HEAD + c))
    cb = wtx([(b"\x00" * 32, 0xFFFFFFFF, cb_script, 0xFFFFFFFF)], outs, witness=[[nonce]] if want else None)
    all_txs = [cb] + list(txs)
    mroot = merkle([x[1] for x in all_txs])
    h = struct.pack("<i", version) + prev + mroot + struct.pack("<III", t, BITS, 0)
    for n in range(1 << 20):
        hh = h[:76] + struct.pack("<I", n)
        if int.from_bytes(scrypt(hh), "little") <= target(BITS):
            break
    return hh + varint(len(all_txs)) + b"".join(x[0] for x in all_txs), cb

# ---------------------------------------------------------------- Core and the run

class Core:
    def __init__(self, exe, work, name, extra):
        self.dir = os.path.join(work, name)
        os.makedirs(self.dir)
        self.port = 19443
        self.proc = subprocess.Popen([exe, "-regtest", f"-datadir={self.dir}", "-server", "-listen=0",
                                      "-rpcuser=regnet", "-rpcpassword=regnet", f"-rpcport={self.port}", "-rpcbind=127.0.0.1",
                                      "-rpcallowip=127.0.0.1", "-printtoconsole=0", "-fallbackfee=0.0002"] + extra,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(120):
            try:
                self.call("getblockcount")
                return
            except Exception:
                time.sleep(0.5)
        raise RuntimeError("litecoind did not start")

    def call(self, method, *params, wallet=None):
        path = f"/wallet/{wallet}" if wallet else "/"
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", json.dumps({"id": 1, "method": method, "params": list(params)}).encode(),
                                     {"Authorization": "Basic cmVnbmV0OnJlZ25ldA==", "Content-Type": "application/json"})
        try:
            r = json.load(urllib.request.urlopen(req, timeout=300))
        except urllib.error.HTTPError as e:
            r = json.load(e)
        if r.get("error"):
            raise RuntimeError(f"{method}: {r['error']}")
        return r["result"]

    def stop(self):
        try:
            self.call("stop")
            self.proc.wait(60)
        except Exception:
            self.proc.kill()

class Net:
    def __init__(self, core, follow, work, name):
        self.core, self.follow = core, follow
        self.blocks = os.path.join(work, f"{name}-blocks.hex")
        self.db = os.path.join(work, f"{name}-v1.mdbx")
        self.addr = p2pkh_address(b"\x11" * 20)
        self.results = []
        self.seen_errors = set()
        self.t = int(time.time())
        self.known = set()
        self.coins = []  # our OP_TRUE coinbase coins: (txid, vout, value, height)
        open(self.blocks, "w").close()

    def now(self, step=1):
        self.t += step
        return self.t

    def give_v1(self, raw):
        with open(self.blocks, "a") as f:
            f.write(raw.hex() + "\n")

    def from_core(self):
        n = self.core.call("getblockcount")
        for h in range(n + 1):
            bh = self.core.call("getblockhash", h)
            if bh not in self.known:
                raw = bytes.fromhex(self.core.call("getblock", bh, 0))
                self.give_v1(raw)
                self.known.add(bh)
                self.t = max(self.t, struct.unpack("<I", raw[68:72])[0])

    def mine(self, n, addr=None):
        for _ in range(0, n, 100):
            self.core.call("generatetoaddress", min(100, n), addr or self.addr)
            n -= 100
            if n <= 0:
                break
        self.from_core()

    def tip(self):
        return bytes.fromhex(self.core.call("getbestblockhash"))[::-1], self.core.call("getblockcount")

    def mtp(self):
        return self.core.call("getblockheader", self.core.call("getbestblockhash"))["mediantime"]

    def submit(self, raw):
        res = self.core.call("submitblock", raw.hex())
        self.give_v1(raw)
        self.known.add(hexid(bhash(raw)))
        return res

    def build(self, txs, **kw):
        """A block on Core's tip; submitted to both. Returns Core's answer and the coinbase."""
        tip, h = self.tip()
        t = kw.pop("t", None) or self.now()
        raw, cb = ltc_block(tip, h + 1, t, txs, **kw)
        r = self.submit(raw)
        return r, cb, h + 1

    def v1(self):
        out = subprocess.run([self.follow, self.db, self.blocks, "ltcregtest"], capture_output=True, text=True, timeout=900)
        line = [l for l in out.stdout.splitlines() if l.startswith("{")]
        if out.returncode != 0 or not line:
            raise RuntimeError(f"regtest_follow failed: {out.stderr[-2000:]}")
        return json.loads(line[-1])

    def check(self, name, what, notes=""):
        """`what` says what Core must do with the scenario's block: "both take it" (accepted) or
        "Core: <reason>" (refused). A Core verdict other than that is a broken scenario (BUILD):
        it proves nothing about V1, so it fails the run too."""
        core_best = self.core.call("getbestblockhash")
        core_h = self.core.call("getblockcount")
        v = self.v1()
        same = v["tip"] == core_best
        status = "MATCH" if same else "FAIL"
        if what.startswith("both take") and notes not in ("", None):
            status = "BUILD"
        if what.startswith("Core:") and notes in ("", None):
            status = "BUILD"
        self.results.append((status, name))
        print(f"\n[{status}] {name}\n    {what}")
        if notes:
            print(f"    Core: {notes}")
        if status == "BUILD":
            print("    the block isn't what the scenario meant: Core's verdict differs")
        print(f"    Core best {core_best[:16]}… at {core_h}; V1 tip {v['tip'][:16]}… at {v['height']}")
        for e in v["errors"]:
            if e not in self.seen_errors:
                print(f"    V1: {e[:240]}")
        self.seen_errors.update(v["errors"])
        return same

    def heal(self):
        """After a refused block both must keep following Core."""
        self.mine(1)

    def coin(self):
        """One of our mature OP_TRUE coins."""
        _, h = self.tip()
        for i, c in enumerate(self.coins):
            if h + 1 - c[3] >= 100:
                return self.coins.pop(i)
        raise RuntimeError("no mature coin")

# ---------------------------------------------------------------- run A: SegWit, Taproot, CSV

def run_a(net):
    net.from_core()
    # Our own coins: 40 blocks paying OP_TRUE, then Core mines them mature.
    for _ in range(40):
        r, cb, h = net.build([])
        net.coins.append((cb[1], 0, subsidy(h), h))
    net.mine(110)
    net.check("Hand-built blocks, then Core mines 110", "both follow (SegWit on from genesis on regtest)")

    # Version floor: the whole nVersion, 0x20000000 from SegWit on (not the AuxPoW base version).
    r, _, _ = net.build([], version=4)
    net.check("A version-4 block once SegWit is on", "Core: bad-version (whole nVersion below 0x20000000)", notes=r)
    net.heal()

    # P2WSH coins: OP_TRUE, a key, NULLDUMMY multisig, CSV, and many CHECKSIGs.
    key = EcKey()
    tap = TapKey(b"ltcnet taproot")
    ms = b"\x51" + push(key.pub) + b"\x51\xae"                      # 1 <pub> 1 CHECKMULTISIG
    csv = script_num(10) + b"\xb2\x75\x51"                           # <10> CSV DROP TRUE
    big = b"\xac" * 10_000                                           # 10,000 CHECKSIGs (never run)
    c = net.coin()
    v = c[2] // 25
    outs = [(v, p2wsh(OP_TRUE))] * 4 + [(v, key.script)] * 3 + [(v, p2wsh(ms))] * 2 + [(v, p2wsh(csv))] * 2 + [(v, p2wsh(big))] * 9 + [(v - 50_000, tap.script)]
    fund = wtx([(c[0], c[1], b"", 0xFFFFFFFF)], outs)
    fee = c[2] - sum(o[0] for o in outs)
    r, _, fh = net.build([fund], fees=fee)
    net.check("Funding P2WSH and Taproot coins", "both take it", notes=r)
    f = fund[1]
    idx = {"true": [0, 1, 2, 3], "key": [4, 5, 6], "ms": [7, 8], "csv": [9, 10], "big": list(range(11, 20)), "tap": [20]}
    amt = {i: o[0] for i, o in enumerate(outs)}
    net.mine(2)

    # Witness spends.
    sp = wtx([(f, idx["true"][0], b"", 0xFFFFFFFF)], [(amt[0] - 1000, OP_TRUE)], witness=[[OP_TRUE]])
    r, _, _ = net.build([sp], fees=1000)
    net.check("A P2WSH OP_TRUE spend", "both take it", notes=r)

    sp = wtx([(f, idx["true"][1], b"", 0xFFFFFFFF)], [(amt[1] - 1000, OP_TRUE)], witness=[[OP_TRUE]])
    r, _, _ = net.build([sp], fees=1000, commitment=None)
    net.check("A witness spend in a block with no witness commitment", "Core: unexpected-witness", notes=r)
    net.heal()

    r, _, _ = net.build([], commitment="bad")
    net.check("A wrong witness commitment in a block with no witness data", "Core: bad-witness-merkle-match", notes=r)
    net.heal()

    sp = wtx([(f, idx["true"][1], b"", 0xFFFFFFFF)], [(amt[1] - 1000, OP_TRUE)], witness=[[OP_TRUE]])
    r, _, _ = net.build([sp], fees=1000, commitment="bad")
    net.check("A witness spend under a wrong commitment", "Core: bad-witness-merkle-match", notes=r)
    net.heal()

    # A legacy OP_TRUE coin spent with witness data: the script check refuses it.
    c2 = net.coin()
    sp = wtx([(c2[0], c2[1], b"", 0xFFFFFFFF)], [(c2[2] - 1000, OP_TRUE)], witness=[[b"\x01"]])
    r, _, _ = net.build([sp], fees=1000)
    net.check("Witness data on a non-witness spend", "Core: witness unexpected (script)", notes=r)
    net.heal()
    net.coins.append(c2)

    # Signed P2WSH spends (BIP143).
    def key_spend(i, wrong=False):
        ins = [(f, i, 0xFFFFFFFF)]
        o = [(amt[i] - 1000, OP_TRUE)]
        d = bip143_sighash(1, ins, o, 0, 0, key.ws, amt[i] + (1 if wrong else 0))
        return wtx([(f, i, b"", 0xFFFFFFFF)], o, witness=[[key.sign(d) + b"\x01", key.ws]])
    r, _, _ = net.build([key_spend(idx["key"][0], wrong=True)], fees=1000)
    net.check("A P2WSH spend signed over the wrong amount (BIP143)", "Core: script check fails", notes=r)
    net.heal()
    r, _, _ = net.build([key_spend(idx["key"][0])], fees=1000)
    net.check("A correctly signed P2WSH spend", "both take it", notes=r)

    # NULLDUMMY in a witness multisig.
    def ms_spend(i, dummy):
        ins = [(f, i, 0xFFFFFFFF)]
        o = [(amt[i] - 1000, OP_TRUE)]
        d = bip143_sighash(1, ins, o, 0, 0, ms, amt[i])
        return wtx([(f, i, b"", 0xFFFFFFFF)], o, witness=[[dummy, key.sign(d) + b"\x01", ms]])
    r, _, _ = net.build([ms_spend(idx["ms"][0], b"\x01")], fees=1000)
    net.check("Witness CHECKMULTISIG with a non-empty dummy", "Core: NULLDUMMY fails", notes=r)
    net.heal()
    r, _, _ = net.build([ms_spend(idx["ms"][0], b"")], fees=1000)
    net.check("Witness CHECKMULTISIG with an empty dummy", "both take it", notes=r)

    # Taproot key path (BIP340/341).
    ti = idx["tap"][0]
    def tap_spend(flip):
        ins = [(f, ti, 0xFFFFFFFF)]
        o = [(amt[ti] - 1000, OP_TRUE)]
        sig = schnorr_sign(tap.tweaked, taproot_sighash(2, ins, o, 0, [(amt[ti], tap.script)], 0))
        if flip:
            sig = sig[:10] + bytes([sig[10] ^ 1]) + sig[11:]
        return wtx([(f, ti, b"", 0xFFFFFFFF)], o, witness=[[sig]], version=2)
    r, _, _ = net.build([tap_spend(True)], fees=1000)
    net.check("A Taproot key-path spend with a broken signature", "Core: script check fails (Schnorr)", notes=r)
    net.heal()
    r, _, _ = net.build([tap_spend(False)], fees=1000)
    net.check("A Taproot key-path spend", "both take it", notes=r)

    # Witness signature-operation cost: 9 inputs of 10,000 CHECKSIGs each = 90,000 > 80,000.
    ins = [(f, i, b"", 0xFFFFFFFF) for i in idx["big"]]
    sp = wtx(ins, [(sum(amt[i] for i in idx["big"]) - 1000, OP_TRUE)], witness=[[big]] * 9)
    r, _, _ = net.build([sp], fees=1000)
    net.check("Witness scripts with 90,000 signature operations", "Core: bad-blk-sigops (cost over 80,000)", notes=r)
    net.heal()

    # Block weight: 4.1 MB of witness data (under a correct commitment).
    junk = [b"\x00" * 500] * 8200
    sp = wtx([(f, idx["true"][2], b"", 0xFFFFFFFF)], [(amt[2] - 1000, OP_TRUE)], witness=[junk + [OP_TRUE]])
    r, _, _ = net.build([sp], fees=1000)
    net.check("A block of weight over 4,000,000", "Core: bad-blk-weight", notes=r)
    net.heal()

    # CSV: on from 432.
    _, h = net.tip()
    if h < 440:
        net.mine(440 - h)
    # BIP68, height lock 10: a coin confirmed now, spent 5 then 10 blocks later.
    c3 = net.coin()
    mk = wtx([(c3[0], c3[1], b"", 0xFFFFFFFF)], [(c3[2] - 1000, OP_TRUE)])
    r, _, ch = net.build([mk], fees=1000)
    net.mine(4)
    seq_spend = wtx([(mk[1], 0, b"", 10)], [(c3[2] - 2000, OP_TRUE)], version=2)
    r, _, _ = net.build([seq_spend], fees=1000)
    net.check("A version-2 spend with a 10-block relative lock, 5 blocks in", "Core: bad-txns-nonfinal (BIP68)", notes=r)
    net.heal()
    net.mine(4)
    r, _, _ = net.build([seq_spend], fees=1000)
    net.check("The same spend 10 blocks in", "both take it", notes=r)

    # The same lock on a version-1 transaction means nothing (BIP68 is version 2 and up).
    c4 = net.coin()
    mk4 = wtx([(c4[0], c4[1], b"", 0xFFFFFFFF)], [(c4[2] - 1000, OP_TRUE)])
    net.build([mk4], fees=1000)
    v1_spend = wtx([(mk4[1], 0, b"", 10)], [(c4[2] - 2000, OP_TRUE)], version=1)
    r, _, _ = net.build([v1_spend], fees=1000)
    net.check("A version-1 spend with the same sequence, 1 block in", "both take it", notes=r)

    # BIP68 by time: 512 seconds, the coin's median time past against the block's.
    c5 = net.coin()
    mk5 = wtx([(c5[0], c5[1], b"", 0xFFFFFFFF)], [(c5[2] - 1000, OP_TRUE)])
    net.build([mk5], fees=1000)
    tspend = wtx([(mk5[1], 0, b"", (1 << 22) | 1)], [(c5[2] - 2000, OP_TRUE)], version=2)
    r, _, _ = net.build([tspend], fees=1000)
    net.check("A version-2 spend locked 512 seconds, right away", "Core: bad-txns-nonfinal (BIP68 time)", notes=r)
    net.heal()
    for _ in range(12):
        net.build([], t=net.now(120))
    r, _, _ = net.build([tspend], fees=1000, t=net.now(120))
    net.check("The same spend once the median time past has moved 24 minutes", "both take it", notes=r)

    # BIP113: a lock time between the median time past and the block's own time.
    c6 = net.coin()
    mtp = net.mtp()
    lt = mtp + 5
    lspend = wtx([(c6[0], c6[1], b"", 0xFFFFFFFE)], [(c6[2] - 1000, OP_TRUE)], lock_time=lt)
    r, _, _ = net.build([lspend], fees=1000, t=max(net.now(), lt + 600))
    net.check("Lock time after the median time past, before the block's time", "Core: bad-txns-nonfinal (BIP113)", notes=r)
    net.heal()

    # OP_CHECKSEQUENCEVERIFY (BIP112) in a witness script: <10> CSV, sequence 5 then 10.
    ci = idx["csv"][0]
    def csv_spend(seq):
        return wtx([(f, ci, b"", seq)], [(amt[ci] - 1000, OP_TRUE)], witness=[[csv]], version=2)
    r, _, _ = net.build([csv_spend(5)], fees=1000)
    net.check("A <10> CHECKSEQUENCEVERIFY coin spent with sequence 5", "Core: script check fails (CSV)", notes=r)
    net.heal()
    r, _, _ = net.build([csv_spend(10)], fees=1000)
    net.check("The same coin with sequence 10", "both take it", notes=r)

    # Coin rules at a halved subsidy (regtest halves every 150 blocks).
    _, h = net.tip()
    r, _, _ = net.build([], cb_value=subsidy(h + 1) + 1)
    net.check(f"A coinbase one unit over the subsidy at height {h + 1}", "Core: bad-cb-amount", notes=r)
    net.heal()
    r, _, _ = net.build([], cb_value=subsidy(h + 2))
    net.check("A coinbase paying exactly the subsidy", "both take it", notes=r)

# ---------------------------------------------------------------- run B: MWEB

def run_b(net):
    core = net.core
    core.call("createwallet", "w")
    addr = core.call("getnewaddress", "", "bech32", wallet="w")
    net.mine(150, addr)
    # "active" is for the next block: the first HogEx needs a peg-in to spend, so one waits
    # in the mempool when it comes (as Litecoin's own tests do).
    for _ in range(2000):
        st = core.call("getblockchaininfo")["softforks"].get("mweb", {})
        if st.get("active"):
            break
        core.call("generatetoaddress", 1, addr)
    net.from_core()
    print(f"\nMWEB: {json.dumps(st)[:200]} at {core.call('getblockcount')}")
    m1 = core.call("getnewaddress", "", "mweb", wallet="w")
    m2 = core.call("getnewaddress", "", "mweb", wallet="w")
    core.call("sendtoaddress", m1, 10, wallet="w")
    net.mine(1, addr)
    net.check("Core mines until MWEB is on; the first MWEB block, with a peg-in", "both follow (HogEx and extension blocks from here)")
    core.call("sendtoaddress", m2, 3, wallet="w")
    net.mine(1, addr)
    net.check("A transfer inside MWEB (its fee leaves through the HogEx)", "both follow")
    out = core.call("getnewaddress", "", "bech32", wallet="w")
    core.call("sendtoaddress", out, 2, wallet="w")
    net.mine(1, addr)
    net.check("A peg-out from MWEB", "both follow")
    net.mine(6, addr)
    utx = [u for u in core.call("listunspent", 1, 9999999, [out], wallet="w")]
    if utx:
        back = core.call("getnewaddress", "", "bech32", wallet="w")
        raw = core.call("createrawtransaction", [{"txid": utx[0]["txid"], "vout": utx[0]["vout"]}], {back: round(utx[0]["amount"] - 0.001, 8)}, wallet="w")
        sraw = core.call("signrawtransactionwithwallet", raw, wallet="w")["hex"]
        core.call("sendrawtransaction", sraw)
        net.mine(1, addr)
        net.check("Spending the peg-out after 6 blocks", "both follow")
    else:
        print("    (no peg-out coin found to spend)")
    for i in range(5):
        core.call("sendtoaddress", m1 if i % 2 else core.call("getnewaddress", "", "bech32", wallet="w"), 0.5, wallet="w")
        core.call("sendtoaddress", core.call("getnewaddress", "", "mweb", wallet="w"), 0.25, wallet="w")
        net.mine(1, addr)
    net.check("Five more blocks of peg-ins, transfers and peg-outs", "both follow")

def main():
    ap = argparse.ArgumentParser()
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--litecoind", default=shutil.which("litecoind") or os.path.expanduser("~/.local/bin/litecoind"))
    ap.add_argument("--follow", default=os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(root, "target")), "release/examples/regtest_follow"))
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", choices=["a", "b"])
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="ltcnet-")
    results = []
    try:
        for name, fn, extra in [("a", run_a, ["-disablewallet", "-vbparams=mweb:4102444800:4102444801"]), ("b", run_b, [])]:
            if a.only and a.only != name:
                continue
            print(f"\n======== run {name.upper()}")
            core = Core(a.litecoind, work, f"core-{name}", extra)
            net = Net(core, a.follow, work, name)
            try:
                fn(net)
            finally:
                core.stop()
                results += net.results
    finally:
        if not a.keep:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print(f"\nwork dir kept: {work}")
    fails = [n for s, n in results if s in ("FAIL", "BUILD")]
    print(f"\n{len(results)} scenarios: {len(results) - len(fails)} match, {len(fails)} FAIL")
    for n in fails:
        print(f"  FAIL: {n}")
    sys.exit(1 if fails else 0)

if __name__ == "__main__":
    main()
