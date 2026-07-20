pub mod bitable;
pub mod callback;
pub mod im;
pub mod task;
pub mod token;

use std::sync::Arc;
use tokio::sync::RwLock;

/// 飞书开放平台 API 根地址。
pub const API_BASE: &str = "https://open.feishu.cn/open-apis";

use token::CachedToken;

/// 飞书开放平台客户端：持有凭证、复用 HTTP 连接、缓存 tenant_access_token。
#[derive(Clone)]
pub struct FeishuClient {
    pub(crate) http: reqwest::Client,
    app_id: String,
    app_secret: String,
    token: Arc<RwLock<Option<CachedToken>>>,
}

impl FeishuClient {
    pub fn new(app_id: String, app_secret: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("构建 reqwest 客户端失败");
        Self {
            http,
            app_id,
            app_secret,
            token: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn app_id(&self) -> &str {
        &self.app_id
    }

    pub(crate) fn app_secret(&self) -> &str {
        &self.app_secret
    }

    pub(crate) fn token_cache(&self) -> &Arc<RwLock<Option<CachedToken>>> {
        &self.token
    }
}

/// 飞书 API 统一响应外壳；`code == 0` 表示成功。
#[derive(Debug, serde::Deserialize)]
pub struct ApiEnvelope<T> {
    pub code: i64,
    #[serde(default)]
    pub msg: String,
    pub data: Option<T>,
}

impl<T> ApiEnvelope<T> {
    /// 校验 code==0，取出 data；否则返回带 code/msg 的错误。
    pub fn into_result(self) -> anyhow::Result<T> {
        if self.code != 0 {
            anyhow::bail!("飞书 API 错误 code={} msg={}", self.code, self.msg);
        }
        self.data
            .ok_or_else(|| anyhow::anyhow!("飞书 API 返回 code=0 但 data 为空"))
    }
}
