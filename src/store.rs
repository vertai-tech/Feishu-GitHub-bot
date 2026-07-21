//! SLA「待办跟踪」表读写：记录受理人/reviewer 对某 PR/Issue 的待处理状态，
//! 供后台调度器按工作时长提醒。每个 (item, 负责人) 一行，键为 `repo#number#login`。

use crate::state::AppState;
use serde_json::{json, Value};

const F_KEY: &str = "键";
const F_REPO: &str = "仓库";
const F_NUMBER: &str = "编号";
const F_TITLE: &str = "标题";
const F_URL: &str = "链接";
const F_TYPE: &str = "类型"; // pr / issue
const F_LOGIN: &str = "负责人";
const F_OPEN_ID: &str = "open_id";
const F_ROLE: &str = "角色"; // 受理人 / reviewer
const F_PENDING_SINCE: &str = "待处理起点";
const F_LAST_REMINDED: &str = "上次提醒";
const F_STATUS: &str = "状态"; // pending / done

fn row_key(repo: &str, number: u64, login: &str) -> String {
    format!("{repo}#{number}#{login}")
}

fn item_prefix(repo: &str, number: u64) -> String {
    format!("{repo}#{number}#")
}

/// 一条待办跟踪记录（调度器用）。
pub struct PendingRow {
    pub record_id: String,
    pub kind: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub open_id: String,
    pub role: String,
    pub pending_since_ms: i64,
    pub last_reminded_ms: Option<i64>,
}

/// 登记/刷新一个负责人对某项的待处理（指派/被请求 review 时调用）。已存在则重置为 pending+now。
pub async fn upsert_pending(
    state: &AppState,
    repo: &str,
    number: u64,
    title: &str,
    url: &str,
    is_pr: bool,
    login: &str,
    open_id: &str,
    role: &str,
) {
    let cfg = &state.cfg.feishu;
    let key = row_key(repo, number, login);
    let now = chrono::Utc::now().timestamp_millis();
    let fields = json!({
        F_KEY: key,
        F_REPO: repo,
        F_NUMBER: number,
        F_TITLE: title,
        F_URL: url,
        F_TYPE: if is_pr { "pr" } else { "issue" },
        F_LOGIN: login,
        F_OPEN_ID: open_id,
        F_ROLE: role,
        F_PENDING_SINCE: now,
        F_LAST_REMINDED: null,
        F_STATUS: "pending",
    });
    match state
        .feishu
        .bitable_search(&cfg.base_app_token, &cfg.sla_table_id, F_KEY, &key)
        .await
    {
        Ok(recs) => {
            let r = if let Some(rec) = recs.into_iter().next() {
                state
                    .feishu
                    .bitable_update(&cfg.base_app_token, &cfg.sla_table_id, &rec.record_id, fields)
                    .await
                    .map(|_| ())
            } else {
                state
                    .feishu
                    .bitable_create(&cfg.base_app_token, &cfg.sla_table_id, fields)
                    .await
                    .map(|_| ())
            };
            if let Err(e) = r {
                tracing::warn!("SLA upsert 写入失败 {key}: {e:#}");
            }
        }
        Err(e) => tracing::warn!("SLA upsert 查询失败: {e:#}"),
    }
}

/// 某负责人已响应 → 其行标记 done。
pub async fn mark_done(state: &AppState, repo: &str, number: u64, login: &str) {
    let cfg = &state.cfg.feishu;
    let key = row_key(repo, number, login);
    if let Ok(recs) = state
        .feishu
        .bitable_search(&cfg.base_app_token, &cfg.sla_table_id, F_KEY, &key)
        .await
    {
        for rec in recs {
            let _ = state
                .feishu
                .bitable_update(
                    &cfg.base_app_token,
                    &cfg.sla_table_id,
                    &rec.record_id,
                    json!({ F_STATUS: "done" }),
                )
                .await;
        }
    }
}

/// 该项所有行标记 done（PR 合并/关闭、Issue 关闭时调用）。
pub async fn mark_all_done(state: &AppState, repo: &str, number: u64) {
    let cfg = &state.cfg.feishu;
    if let Ok(recs) = state
        .feishu
        .bitable_search_op(
            &cfg.base_app_token,
            &cfg.sla_table_id,
            F_KEY,
            "contains",
            &item_prefix(repo, number),
        )
        .await
    {
        for rec in recs {
            let _ = state
                .feishu
                .bitable_update(
                    &cfg.base_app_token,
                    &cfg.sla_table_id,
                    &rec.record_id,
                    json!({ F_STATUS: "done" }),
                )
                .await;
        }
    }
}

/// 他人新评论 → 该项所有 pending 行重置计时（待处理起点=now，清空上次提醒）；
/// `responder` 若本身是负责人，其行改为 done（视为已响应）。
pub async fn reset_pending_on_activity(state: &AppState, repo: &str, number: u64, responder: &str) {
    let cfg = &state.cfg.feishu;
    let recs = match state
        .feishu
        .bitable_search_op(
            &cfg.base_app_token,
            &cfg.sla_table_id,
            F_KEY,
            "contains",
            &item_prefix(repo, number),
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    let now = chrono::Utc::now().timestamp_millis();
    for rec in recs {
        let login = rec.fields.get(F_LOGIN).and_then(field_str).unwrap_or_default();
        let status = rec.fields.get(F_STATUS).and_then(field_str).unwrap_or_default();
        let patch = if login == responder {
            // 负责人自己发言 = 已响应
            json!({ F_STATUS: "done" })
        } else if status == "pending" {
            // 其他人发言 = 该负责人需重新响应，重置计时
            json!({ F_PENDING_SINCE: now, F_LAST_REMINDED: null })
        } else {
            continue;
        };
        let _ = state
            .feishu
            .bitable_update(&cfg.base_app_token, &cfg.sla_table_id, &rec.record_id, patch)
            .await;
    }
}

/// 列出所有 pending 行（调度器用）。
pub async fn list_pending(state: &AppState) -> Vec<PendingRow> {
    let cfg = &state.cfg.feishu;
    let recs = match state
        .feishu
        .bitable_search(&cfg.base_app_token, &cfg.sla_table_id, F_STATUS, "pending")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("SLA list_pending 失败: {e:#}");
            return vec![];
        }
    };
    // 并发 upsert 竞态可能给同一 (item, 负责人) 留下多行，按复合键去重，只调度一行。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    recs.into_iter()
        .filter_map(|rec| {
            let open_id = rec.fields.get(F_OPEN_ID).and_then(field_str)?;
            if open_id.is_empty() {
                return None;
            }
            let key = rec.fields.get(F_KEY).and_then(field_str).unwrap_or_default();
            if !key.is_empty() && !seen.insert(key) {
                return None;
            }
            Some(PendingRow {
                record_id: rec.record_id.clone(),
                kind: rec.fields.get(F_TYPE).and_then(field_str).unwrap_or_default(),
                number: rec.fields.get(F_NUMBER).and_then(field_num).unwrap_or(0),
                title: rec.fields.get(F_TITLE).and_then(field_str).unwrap_or_default(),
                url: rec.fields.get(F_URL).and_then(field_str).unwrap_or_default(),
                open_id,
                role: rec.fields.get(F_ROLE).and_then(field_str).unwrap_or_default(),
                pending_since_ms: rec.fields.get(F_PENDING_SINCE).and_then(field_num_i64).unwrap_or(0),
                last_reminded_ms: rec.fields.get(F_LAST_REMINDED).and_then(field_num_i64),
            })
        })
        .collect()
}

/// 更新某行的上次提醒时间。
pub async fn set_reminded(state: &AppState, record_id: &str, now_ms: i64) {
    let cfg = &state.cfg.feishu;
    let _ = state
        .feishu
        .bitable_update(
            &cfg.base_app_token,
            &cfg.sla_table_id,
            record_id,
            json!({ F_LAST_REMINDED: now_ms }),
        )
        .await;
}

fn field_str(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|e| e.get("text").and_then(|t| t.as_str()))
            .collect();
        return Some(joined);
    }
    None
}

fn field_num(v: &Value) -> Option<u64> {
    v.as_f64().map(|f| f as u64)
}

fn field_num_i64(v: &Value) -> Option<i64> {
    // 日期/数字字段是数值；空字符串视为无
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    None
}

// 供日志用的复合键展示
pub fn pr_key(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}
