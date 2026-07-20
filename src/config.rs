use serde::Deserialize;
use std::path::Path;

/// 顶层配置。从 TOML 文件加载，敏感字段可被同名环境变量覆盖。
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// 监听地址，如 "0.0.0.0:8080"
    #[serde(default = "default_listen")]
    pub listen_addr: String,
    pub feishu: FeishuConfig,
    pub github: GithubConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    /// 事件订阅校验 token（飞书开发者后台「事件订阅」页）
    pub verification_token: String,
    /// 事件订阅 AES 加密 key，未开启加密则留空
    #[serde(default)]
    pub encrypt_key: String,
    /// 广播型通知（PR/Issue 创建、Issue 评论、未绑定提示）的收件人类型：
    /// "union_id" / "open_id" / "chat_id" / "email"。默认 union_id（私聊）。
    #[serde(default = "default_receive_id_type")]
    pub notify_id_type: String,
    /// 广播型通知的收件人 id（配合 notify_id_type）。测试期填管理员本人。
    pub notify_id: String,
    /// 多维表格 app_token
    pub base_app_token: String,
    /// 绑定映射表 table_id
    pub binding_table_id: String,
    /// PR 跟踪表 table_id
    pub pr_table_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubConfig {
    /// webhook secret，用于校验 X-Hub-Signature-256
    pub webhook_secret: String,
    /// 接入的 organization（仅用于日志/校验，可选）
    #[serde(default)]
    pub org: String,
}

fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_receive_id_type() -> String {
    "union_id".to_string()
}

impl Config {
    /// 从 TOML 路径加载，随后用环境变量覆盖敏感字段（若已设置）。
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let mut cfg: Config = toml::from_str(&text)?;

        // 敏感字段允许用环境变量覆盖，方便用 op run / systemd 注入而不落盘。
        if let Ok(v) = std::env::var("FEISHU_APP_SECRET") {
            cfg.feishu.app_secret = v;
        }
        if let Ok(v) = std::env::var("FEISHU_ENCRYPT_KEY") {
            cfg.feishu.encrypt_key = v;
        }
        if let Ok(v) = std::env::var("FEISHU_VERIFICATION_TOKEN") {
            cfg.feishu.verification_token = v;
        }
        if let Ok(v) = std::env::var("GITHUB_WEBHOOK_SECRET") {
            cfg.github.webhook_secret = v;
        }
        Ok(cfg)
    }
}
