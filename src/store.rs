//! PR 跟踪表：记录每个 PR 的群卡片 message_id 与派出的 review 任务，
//! 供 `closed` 事件更新卡片、完成任务。

use crate::feishu::{bitable::Record, FeishuClient};
use serde_json::json;
use std::collections::BTreeMap;

// 「PR跟踪」表字段名。
const F_KEY: &str = "PR键"; // 复合唯一键 "org/repo#7"，用于精确检索
const F_REPO: &str = "仓库";
const F_NUMBER: &str = "PR号";
const F_TITLE: &str = "标题";
const F_MSG_ID: &str = "群message_id";
const F_TASKS: &str = "reviewer任务"; // JSON: {github_login: task_guid}
const F_STATUS: &str = "状态";

/// 复合键。
pub fn pr_key(repo_full_name: &str, number: u64) -> String {
    format!("{repo_full_name}#{number}")
}

/// PR 开启时登记：写入群卡片 message_id 与初始状态。
pub async fn record_opened(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    repo_full_name: &str,
    number: u64,
    title: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let key = pr_key(repo_full_name, number);
    let fields = json!({
        F_KEY: key,
        F_REPO: repo_full_name,
        F_NUMBER: number,
        F_TITLE: title,
        F_MSG_ID: message_id,
        F_TASKS: "{}",
        F_STATUS: "open",
    });
    // 幂等：已存在则更新，否则新建。
    match find(feishu, app_token, table_id, repo_full_name, number).await? {
        Some(rec) => {
            feishu
                .bitable_update(app_token, table_id, &rec.record_id, fields)
                .await?;
        }
        None => {
            feishu.bitable_create(app_token, table_id, fields).await?;
        }
    }
    Ok(())
}

/// 记录派给某 reviewer 的任务 guid（合并进 reviewer任务 JSON）。
pub async fn record_review_task(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    repo_full_name: &str,
    number: u64,
    reviewer_login: &str,
    task_guid: &str,
) -> anyhow::Result<()> {
    let rec = match find(feishu, app_token, table_id, repo_full_name, number).await? {
        Some(r) => r,
        None => return Ok(()), // 没有 open 记录（可能只请求了 review 未收到 opened），跳过
    };
    let mut tasks = read_tasks(&rec);
    tasks.insert(reviewer_login.to_string(), task_guid.to_string());
    let fields = json!({ F_TASKS: serde_json::to_string(&tasks)? });
    feishu
        .bitable_update(app_token, table_id, &rec.record_id, fields)
        .await?;
    Ok(())
}

/// PR 跟踪记录的读出结果。
pub struct PrRecord {
    pub record_id: String,
    pub task_guids: Vec<String>,
}

/// 读取某 PR 的跟踪记录（含所有派出的任务 guid）。
pub async fn get(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    repo_full_name: &str,
    number: u64,
) -> anyhow::Result<Option<PrRecord>> {
    let rec = match find(feishu, app_token, table_id, repo_full_name, number).await? {
        Some(r) => r,
        None => return Ok(None),
    };
    let task_guids = read_tasks(&rec).into_values().collect();
    Ok(Some(PrRecord {
        record_id: rec.record_id,
        task_guids,
    }))
}

/// 更新 PR 状态字段（merged / closed）。
pub async fn set_status(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    record_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    feishu
        .bitable_update(app_token, table_id, record_id, json!({ F_STATUS: status }))
        .await?;
    Ok(())
}

async fn find(
    feishu: &FeishuClient,
    app_token: &str,
    table_id: &str,
    repo_full_name: &str,
    number: u64,
) -> anyhow::Result<Option<Record>> {
    let key = pr_key(repo_full_name, number);
    let recs = feishu
        .bitable_search(app_token, table_id, F_KEY, &key)
        .await?;
    Ok(recs.into_iter().next())
}

fn read_tasks(rec: &Record) -> BTreeMap<String, String> {
    rec.fields
        .get(F_TASKS)
        .and_then(field_to_string)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

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
