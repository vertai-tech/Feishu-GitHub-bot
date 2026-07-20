use super::{FeishuClient, API_BASE};
use serde::Deserialize;
use std::time::{Duration, Instant};

/// 内存中缓存的 token 及其过期时刻。
#[derive(Clone)]
pub struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// tenant_access_token 接口的响应（该接口不套 data 层）。
#[derive(Debug, Deserialize)]
struct TenantTokenResp {
    code: i64,
    msg: String,
    #[serde(default)]
    tenant_access_token: String,
    /// 有效期（秒）
    #[serde(default)]
    expire: u64,
}

impl FeishuClient {
    /// 返回可用的 tenant_access_token；缓存有效则直接复用，否则刷新。
    pub async fn tenant_token(&self) -> anyhow::Result<String> {
        // 快路径：读锁看缓存是否仍然新鲜（留 60s 余量）。
        {
            let guard = self.token_cache().read().await;
            if let Some(tok) = guard.as_ref() {
                if tok.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(tok.value.clone());
                }
            }
        }

        // 慢路径：取写锁刷新。二次检查避免并发重复刷新。
        let mut guard = self.token_cache().write().await;
        if let Some(tok) = guard.as_ref() {
            if tok.expires_at > Instant::now() + Duration::from_secs(60) {
                return Ok(tok.value.clone());
            }
        }

        let resp: TenantTokenResp = self
            .http
            .post(format!("{API_BASE}/auth/v3/tenant_access_token/internal"))
            .json(&serde_json::json!({
                "app_id": self.app_id(),
                "app_secret": self.app_secret(),
            }))
            .send()
            .await?
            .json()
            .await?;

        if resp.code != 0 {
            anyhow::bail!("获取 tenant_access_token 失败 code={} msg={}", resp.code, resp.msg);
        }

        let cached = CachedToken {
            value: resp.tenant_access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(resp.expire.max(60)),
        };
        *guard = Some(cached);
        Ok(resp.tenant_access_token)
    }
}
