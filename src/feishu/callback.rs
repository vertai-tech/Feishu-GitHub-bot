//! 解析飞书事件订阅 / 卡片回调请求体，支持可选 AES 加密。

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::Engine;
use sha2::{Digest, Sha256};

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// 解析后的回调类型。
#[derive(Debug)]
pub enum Callback {
    /// 事件订阅 URL 验证，需原样回 challenge。
    UrlVerification { challenge: String },
    /// 绑定卡片提交：某飞书用户填了 GitHub 用户名。
    BindGithub {
        open_id: String,
        github_username: String,
        /// 卡片令牌（c-…），用于对同一次点击的新旧两份回调去重
        dedup: String,
    },
    /// 用户私聊了机器人，回一张绑定卡片。
    SendBindingCard { open_id: String },
    /// 点了「添加到任务」按钮：给点击者建一条任务。
    AddToTask {
        open_id: String,
        kind: String,
        repo: String,
        title: String,
        url: String,
        dedup: String,
    },
    /// 已识别但本服务不处理的回调。
    Ignored,
}

/// 若配置了 encrypt_key，则请求体形如 {"encrypt":"..."}，需先解密。
/// 返回明文 JSON 文本。
pub fn decrypt_body(body: &[u8], encrypt_key: &str) -> anyhow::Result<String> {
    if encrypt_key.is_empty() {
        return Ok(String::from_utf8(body.to_vec())?);
    }
    let outer: serde_json::Value = serde_json::from_slice(body)?;
    let encrypt = match outer.get("encrypt").and_then(|v| v.as_str()) {
        Some(s) => s,
        // 未加密（如某些 challenge 直接明文），原样返回
        None => return Ok(String::from_utf8(body.to_vec())?),
    };

    let mut key = [0u8; 32];
    key.copy_from_slice(&Sha256::digest(encrypt_key.as_bytes()));

    let data = base64::engine::general_purpose::STANDARD.decode(encrypt)?;
    if data.len() < 16 {
        anyhow::bail!("飞书密文长度异常");
    }
    let (iv, ciphertext) = data.split_at(16);
    let plain = Aes256CbcDec::new(&key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|e| anyhow::anyhow!("AES 解密失败: {e}"))?;
    Ok(String::from_utf8(plain)?)
}

/// 解析明文 JSON，识别回调类型。
///
/// 飞书对同一次卡片点击会发**两份**回调到回调地址：
/// - 旧格式（无 header，顶层 open_id/action，token 为卡片令牌 c-…）
/// - 新格式 card.action.trigger（header.token 为验证令牌，event.token 为卡片令牌）
/// 两份共享同一卡片令牌，用它去重、动作只执行一次。
pub fn parse(plaintext: &str, verification_token: &str) -> anyhow::Result<Callback> {
    let v: serde_json::Value = serde_json::from_str(plaintext)?;

    // URL 验证：直接回 challenge（round-trip 本身即所有权证明，不强校验 token，
    // 以兼容事件订阅与卡片回调可能使用不同令牌的情况）。
    if v.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = v
            .get("challenge")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("url_verification 缺少 challenge"))?;
        return Ok(Callback::UrlVerification {
            challenge: challenge.to_string(),
        });
    }

    // 旧格式卡片回调：无 header，顶层直接是 open_id + action。
    // 其 token 是卡片令牌（c-…），无法与 verification_token 比对，故不校验，仅用于去重。
    if v.get("header").is_none() {
        if let Some(action) = v.get("action") {
            let open_id = v
                .get("open_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let dedup = v
                .get("token")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(card_action_callback(action, open_id, dedup));
        }
        return Ok(Callback::Ignored);
    }

    // 2.0 事件：header.token 必须匹配验证令牌。
    let header = v.get("header");
    let token = header.and_then(|h| h.get("token")).and_then(|t| t.as_str());
    check_token(token, verification_token)?;

    let event_type = header
        .and_then(|h| h.get("event_type"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    if event_type == "card.action.trigger" {
        let event = v.get("event").ok_or_else(|| anyhow::anyhow!("缺少 event"))?;
        let open_id = event
            .get("operator")
            .and_then(|o| o.get("open_id"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        // event.token 是卡片令牌（c-…），与旧格式同值，用于去重
        let dedup = event
            .get("token")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(action) = event.get("action") {
            return Ok(card_action_callback(action, open_id, dedup));
        }
    }

    // 用户私聊机器人 → 回绑定卡片（仅 p2p）。
    if event_type == "im.message.receive_v1" {
        let event = v.get("event");
        let chat_type = event
            .and_then(|e| e.get("message"))
            .and_then(|m| m.get("chat_type"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if chat_type == "p2p" {
            let open_id = event
                .and_then(|e| e.get("sender"))
                .and_then(|s| s.get("sender_id"))
                .and_then(|s| s.get("open_id"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if !open_id.is_empty() {
                return Ok(Callback::SendBindingCard { open_id });
            }
        }
    }

    Ok(Callback::Ignored)
}

/// 从 action 对象（含 value.action / form_value）构造对应回调。
fn card_action_callback(action: &serde_json::Value, open_id: String, dedup: String) -> Callback {
    let action_name = action
        .get("value")
        .and_then(|val| val.get("action"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    match action_name {
        "bind_github" => {
            let github_username = action
                .get("form_value")
                .and_then(|f| f.get("github_username"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            Callback::BindGithub {
                open_id,
                github_username,
                dedup,
            }
        }
        "add_to_task" => {
            let val = action.get("value");
            let get = |k: &str| {
                val.and_then(|v| v.get(k))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            Callback::AddToTask {
                open_id,
                kind: get("kind"),
                repo: get("repo"),
                title: get("title"),
                url: get("url"),
                dedup,
            }
        }
        _ => Callback::Ignored,
    }
}

fn check_token(got: Option<&str>, expected: &str) -> anyhow::Result<()> {
    match got {
        Some(t) if t == expected => Ok(()),
        _ => anyhow::bail!("飞书回调 verification_token 不匹配"),
    }
}
