//! 绑定映射：飞书 open_id ↔ GitHub 用户名，落在多维表格「绑定映射」表。

use crate::feishu::FeishuClient;
use serde_json::json;

// 表字段名，需与飞书多维表格里创建的列名一致。
const F_GITHUB: &str = "GitHub用户名";
const F_OPEN_ID: &str = "飞书open_id";
const F_BOUND_AT: &str = "绑定时间";

/// 写入/更新一条绑定：一个飞书用户对应一个 GitHub 用户名（按 open_id upsert）。
/// 返回给用户看的回执文案。
pub async fn upsert_binding(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    open_id: &str,
    github_username: &str,
) -> anyhow::Result<String> {
    if github_username.is_empty() {
        return Ok("GitHub 用户名不能为空".to_string());
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let fields = json!({
        F_GITHUB: github_username,
        F_OPEN_ID: open_id,
        F_BOUND_AT: now_ms,
    });

    // 已有该 open_id 的记录则更新，否则新建。
    let existing = feishu
        .bitable_search(app_token, table_id, F_OPEN_ID, open_id)
        .await?;
    if let Some(rec) = existing.into_iter().next() {
        feishu
            .bitable_update(app_token, table_id, &rec.record_id, fields)
            .await?;
    } else {
        feishu.bitable_create(app_token, table_id, fields).await?;
    }
    Ok(format!("已绑定 GitHub 账号：{github_username}"))
}

/// 按 GitHub 用户名反查飞书 open_id。
pub async fn lookup_open_id(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    github_login: &str,
) -> anyhow::Result<Option<String>> {
    let recs = feishu
        .bitable_search(app_token, table_id, F_GITHUB, github_login)
        .await?;
    let open_id = recs
        .into_iter()
        .find_map(|r| r.fields.get(F_OPEN_ID).and_then(field_to_string));
    Ok(open_id)
}

/// 多维表格文本字段可能是字符串，也可能是富文本数组 [{"text":...}]，统一取纯文本。
fn field_to_string(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|e| e.get("text").and_then(|t| t.as_str()))
            .collect();
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}
