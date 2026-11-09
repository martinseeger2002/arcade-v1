//! Python-compatible canonical JSON, as the existing arcade signs it:
//! `json.dumps(v, separators=(",", ":"), sort_keys=True)` with ensure_ascii=True
//! (every non-ASCII character escaped as \uXXXX, astral ones as surrogate pairs).

use serde_json::Value;

pub fn canonical(v: &Value) -> String {
    let mut s = String::new();
    write(&mut s, v, true);
    s
}

/// Like Python's `json.dumps(v, separators=(",", ":"))`: compact, keys in insertion order.
pub fn compact(v: &Value) -> String {
    let mut s = String::new();
    write(&mut s, v, false);
    s
}

fn write(out: &mut String, v: &Value, sort: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_str(out, s),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(out, x, sort);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            let mut keys: Vec<&String> = m.keys().collect();
            if sort {
                // Python sorts str keys by code point; Rust's str ordering is by UTF-8 bytes,
                // which gives the same order for valid Unicode.
                keys.sort();
            }
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write(out, &m[*k], sort);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// What an Ed25519-signed JSON record signs: canonical JSON of `v` without the `strip` fields,
/// followed by `blob` (empty for records without one).
pub fn signed_bytes(v: &Value, strip: &[&str], blob: &[u8]) -> Vec<u8> {
    let mut m = v.as_object().cloned().unwrap_or_default();
    for k in strip {
        m.remove(*k);
    }
    let mut out = canonical(&Value::Object(m)).into_bytes();
    out.extend_from_slice(blob);
    out
}

/// Sign `v` (see signed_bytes) and set its "sig" to the hex signature.
pub fn sign_json(key: &ed25519_dalek::SigningKey, v: &mut Value, strip: &[&str], blob: &[u8]) {
    use ed25519_dalek::Signer;
    let sig = key.sign(&signed_bytes(v, strip, blob));
    v["sig"] = Value::String(hex::encode(sig.to_bytes()));
}

/// True if `v["sig"]` is the signature (see signed_bytes) by the hex Ed25519 key in `v[key_field]`.
pub fn verify_json(v: &Value, strip: &[&str], key_field: &str, blob: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let (Some(k), Some(sig)) = (v[key_field].as_str(), v["sig"].as_str()) else { return false };
    let (Ok(k), Ok(sig)) = (hex::decode(k), hex::decode(sig)) else { return false };
    let (Ok(k), Ok(sig)) = (<[u8; 32]>::try_from(k), <[u8; 64]>::try_from(sig)) else { return false };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&k) else { return false };
    vk.verify(&signed_bytes(v, strip, blob), &ed25519_dalek::Signature::from_bytes(&sig)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sign_and_verify_json() {
        let k = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut v = json!({"k": hex::encode(k.verifying_key().to_bytes()), "b": "é", "hops": 1});
        assert_eq!(signed_bytes(&v, &["sig", "hops"], b"!"), format!("{{\"b\":\"\\u00e9\",\"k\":\"{}\"}}!", v["k"].as_str().unwrap()).into_bytes());
        sign_json(&k, &mut v, &["sig", "hops"], b"!");
        assert!(verify_json(&v, &["sig", "hops"], "k", b"!"));
        assert!(!verify_json(&v, &["sig", "hops"], "k", b"?"));
        assert!(!verify_json(&v, &["sig"], "k", b"!"));
        assert!(!verify_json(&v, &["sig", "hops"], "nokey", b"!"));
        v["hops"] = json!(5);
        assert!(verify_json(&v, &["sig", "hops"], "k", b"!"));
        v["b"] = json!("e");
        assert!(!verify_json(&v, &["sig", "hops"], "k", b"!"));
    }

    #[test]
    fn matches_python_dumps() {
        // python3 -c 'import json;print(json.dumps({"b":1,"a":["x","é",None,True],"c":{"z":"😀","y":"\n"}},separators=(",",":"),sort_keys=True))'
        let v = json!({"b":1,"a":["x","é",null,true],"c":{"z":"😀","y":"\n"}});
        assert_eq!(
            canonical(&v),
            r#"{"a":["x","\u00e9",null,true],"b":1,"c":{"y":"\n","z":"\ud83d\ude00"}}"#
        );
    }

    #[test]
    fn del_is_escaped_like_python() {
        // python3 -c 'import json;print(json.dumps("\x7f"))'  ->  "\u007f"
        assert_eq!(canonical(&json!("\u{7f}")), "\"\\u007f\"");
    }
}
