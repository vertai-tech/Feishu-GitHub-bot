use crate::config::Config;
use crate::feishu::FeishuClient;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// 全局共享状态。
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub feishu: FeishuClient,
    /// 已处理的 X-GitHub-Delivery，用于去重（GitHub 会重投递）。
    seen: Arc<Mutex<HashSet<String>>>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let feishu = FeishuClient::new(cfg.feishu.app_id.clone(), cfg.feishu.app_secret.clone());
        Self {
            cfg: Arc::new(cfg),
            feishu,
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 首次见到该 delivery 返回 true；重复投递返回 false。
    pub fn mark_delivery(&self, delivery_id: &str) -> bool {
        if delivery_id.is_empty() {
            return true; // 无 delivery id 不做去重
        }
        let mut set = self.seen.lock().unwrap();
        // 简单封顶，避免无限增长。
        if set.len() > 10_000 {
            set.clear();
        }
        set.insert(delivery_id.to_string())
    }
}
