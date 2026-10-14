//! Stable hashing: canonical JSON and SHA-256 hex digests.
//!
//! Used for history `afterHash`, segment cache keys and silence analysis
//! keys. Canonical JSON is compact with object keys sorted recursively, so
//! the hash never depends on field order or formatting.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Compact JSON with object keys sorted recursively.
pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(serde_json::to_string(key).expect("string serializes").as_bytes());
                out.push(b':');
                write_canonical(&map[key], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        scalar => out.extend_from_slice(serde_json::to_string(scalar).expect("scalar serializes").as_bytes()),
    }
}

/// SHA-256 of the canonical JSON of `value`.
pub fn json_hash(value: &Value) -> String {
    sha256_hex(&canonical_json(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_matter() {
        let a = json!({"b": 1, "a": [1, {"y": true, "x": null}]});
        let b = json!({"a": [1, {"x": null, "y": true}], "b": 1});
        assert_eq!(canonical_json(&a), br#"{"a":[1,{"x":null,"y":true}],"b":1}"#);
        assert_eq!(json_hash(&a), json_hash(&b));
    }

    #[test]
    fn known_digest() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
