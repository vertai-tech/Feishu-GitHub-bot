use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// 校验 GitHub webhook 的 `X-Hub-Signature-256` 头。
/// `header` 形如 "sha256=abcdef..."；返回是否匹配（常量时间比较）。
pub fn verify_signature(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let header = match header {
        Some(h) => h,
        None => return false,
    };
    let hex_sig = match header.strip_prefix("sha256=") {
        Some(s) => s,
        None => return false,
    };
    let expected = match hex::decode(hex_sig) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC 可接受任意长度 key");
    mac.update(body);
    let computed = mac.finalize().into_bytes();

    computed.ct_eq(expected.as_slice()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_signature() {
        // 用已知 secret/body 预先算出的签名做回归。
        let secret = "It's a Secret to Everybody";
        let body = b"Hello, World!";
        // echo -n "Hello, World!" | openssl dgst -sha256 -hmac "It's a Secret to Everybody"
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify_signature(secret, body, Some(sig)));
    }

    #[test]
    fn rejects_tampered_body() {
        let secret = "It's a Secret to Everybody";
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(!verify_signature(secret, b"tampered", Some(sig)));
    }

    #[test]
    fn rejects_missing_header() {
        assert!(!verify_signature("s", b"x", None));
    }
}
