//! The "welcome" protocol (t:"wl"): how a new arcade asks the mesh for test coins.
//! Records are Ed25519-signed by the arcade's node key `a` over canonical JSON of the
//! record without `sig` (welcome.py). Kinds: ask {addr, tag}, sent {ask, txid, tag},
//! closed {block, height, txids}.

use crate::canon::{sign_json, verify_json};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};

pub const AMOUNT_COINS: u64 = 1; // 1 test PEP per ask
pub const ASK_DAYS: u64 = 14;
pub const SKEW_SECS: u64 = 600;

/// Signed: the canonical record without its sig.
const STRIP: &[&str] = &["sig"];

pub fn sign(mut rec: Value, key: &SigningKey) -> Value {
    sign_json(key, &mut rec, STRIP, b"");
    rec
}

/// True if `sig` is the record author's (`a`) signature over the canonical record.
pub fn verify(rec: &Value) -> bool {
    verify_json(rec, STRIP, "a", b"")
}

/// Our ask: one per arcade, for `addr` (a Pepecoin testnet address), with the tag we want.
pub fn ask(key: &SigningKey, addr: &str, tag: &str, now: u64) -> Value {
    sign(
        json!({"a": hex::encode(key.verifying_key().to_bytes()), "addr": addr, "at": now, "k": "ask", "tag": tag}),
        key,
    )
}

pub fn frame(recs: Vec<Value>) -> crate::link::Frame {
    crate::link::Frame::new(json!({"t": "wl", "recs": recs}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vectors captured from the code before sign/verify moved to canon::sign_json:
    /// the signed bytes and signatures must stay byte-for-byte the same.
    #[test]
    fn golden_signatures() {
        let k = SigningKey::from_bytes(&[7u8; 32]);
        let rec = json!({"a": hex::encode(k.verifying_key().to_bytes()), "k": "sent", "ask": "ab", "txid": "cd", "tag": "é☃😀\u{7f}\n", "z": {"b": 1, "a": [1, 2.5, null, true]}});
        assert_eq!(hex::encode(crate::canon::signed_bytes(&rec, STRIP, b"")), "7b2261223a2265613461366336336532396335323061626566353530376231333265633566393935343737366165626562653762393234323165656136393134343664323263222c2261736b223a226162222c226b223a2273656e74222c22746167223a225c75303065395c75323630335c75643833645c75646530305c75303037665c6e222c2274786964223a226364222c227a223a7b2261223a5b312c322e352c6e756c6c2c747275655d2c2262223a317d7d");
        let s = sign(rec, &k);
        assert_eq!(s["sig"], "691497565707886e2d0ad03b4ed442a5ccfb5104a0c87f67fb30f22eadb853cc1bd844f4774664137d4a93e00f197e1da41eaeb92bbdec721a806e7ea8026408");
        assert!(verify(&s));
        assert_eq!(ask(&k, "nYW2BPLENpu2nGa7WCExvzxD3hQYueULFa", "bob", 1_793_281_039)["sig"], "10c7cc1ce1dca45645c9b393880e5e4d9be5226f17d5352f1a760832039381d4604fe2e2cb14f452f8e005429cf91f981638f0e24b2db96bdbf3c366023f3605");
    }

    /// A real ask seen on the live mesh, 2026-11-07: proves our canonical JSON matches.
    #[test]
    fn verifies_live_record() {
        let rec = json!({"a":"86e25ef9c6f1e5fc0d2aa53102828448bff0678de7c1b38e0d19a7e4dff7a9d5","addr":"nYW2BPLENpu2nGa7WCExvzxD3hQYueULFa","at":1793281039,"k":"ask","sig":"9f1ccf41ac9c772f586a7149abbc670d58ea5b7eedfcac9c433bc988d7a4d75ba58e3e47072e6581bf051053c84d7ba00059499522bf0f9c12360a19a38d3107","tag":"bigchiefenergy"});
        assert!(verify(&rec));
        let mut bad = rec.clone();
        bad["tag"] = json!("someoneelse");
        assert!(!verify(&bad));
    }

    #[test]
    fn our_ask_verifies() {
        let k = SigningKey::from_bytes(&[9; 32]);
        assert!(verify(&ask(&k, "nYW2BPLENpu2nGa7WCExvzxD3hQYueULFa", "martin", 1794083702)));
    }
}
