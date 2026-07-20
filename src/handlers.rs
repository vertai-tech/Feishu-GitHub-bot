//! HTTP 入口与事件编排：把 GitHub 事件翻译成飞书动作，处理飞书卡片回调。

use crate::cards::{binding_card, issue_card, issue_comment_card, pr_card, IssueCardStatus, PrCardStatus};
use crate::feishu::callback::{self, Callback};
use crate::github::events::{PrEvent, PrInfo, PullRequestPayload};
use crate::github::issues::{CommentInfo, IssueEvent, IssueInfo, IssuesPayload, IssueCommentPayload};
use crate::github::verify::verify_signature;
use crate::state::AppState;
use crate::{binding, store};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use tracing::{error, info, warn};

/// GET /health
pub async fn health() -> impl IntoResponse {
    "ok"
}

/// POST /webhook/github
pub async fn github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sig = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok());
    if !verify_signature(&state.cfg.github.webhook_secret, &body, sig) {
        warn!("GitHub webhook 签名校验失败");
        return StatusCode::UNAUTHORIZED;
    }

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if event == "ping" {
        return StatusCode::OK;
    }

    let delivery = headers
        .get("X-GitHub-Delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.mark_delivery(delivery) {
        info!("重复投递 {delivery}，跳过");
        return StatusCode::OK;
    }

    // 按事件类型分发。解析失败或非目标 org 都返回相应状态，实际动作异步执行。
    match event {
        "pull_request" => {
            let payload: PullRequestPayload = match serde_json::from_slice(&body) {
                Ok(p) => p,
                Err(e) => {
                    error!("解析 pull_request payload 失败: {e}");
                    return StatusCode::BAD_REQUEST;
                }
            };
            if !in_org(&state.cfg.github.org, &payload.repository.full_name) {
                return StatusCode::OK;
            }
            let ev = payload.classify();
            tokio::spawn(async move {
                if let Err(e) = handle_pr_event(&state, ev).await {
                    error!("处理 PR 事件失败: {e:#}");
                }
            });
        }
        "issues" => {
            let payload: IssuesPayload = match serde_json::from_slice(&body) {
                Ok(p) => p,
                Err(e) => {
                    error!("解析 issues payload 失败: {e}");
                    return StatusCode::BAD_REQUEST;
                }
            };
            if !in_org(&state.cfg.github.org, &payload.repository.full_name) {
                return StatusCode::OK;
            }
            let ev = payload.classify();
            tokio::spawn(async move {
                if let Err(e) = handle_issue_event(&state, ev).await {
                    error!("处理 issue 事件失败: {e:#}");
                }
            });
        }
        "issue_comment" => {
            let payload: IssueCommentPayload = match serde_json::from_slice(&body) {
                Ok(p) => p,
                Err(e) => {
                    error!("解析 issue_comment payload 失败: {e}");
                    return StatusCode::BAD_REQUEST;
                }
            };
            if !in_org(&state.cfg.github.org, &payload.repository.full_name) {
                return StatusCode::OK;
            }
            if let Some(c) = payload.as_created_issue_comment() {
                tokio::spawn(async move {
                    if let Err(e) = handle_issue_comment(&state, c).await {
                        error!("处理 issue 评论失败: {e:#}");
                    }
                });
            }
        }
        _ => {} // 其它事件忽略
    }
    StatusCode::OK
}

/// 防御：仅处理配置 org 下的仓库（org 为空则不限制）。
fn in_org(org: &str, full_name: &str) -> bool {
    if org.is_empty() {
        return true;
    }
    if full_name.starts_with(&format!("{org}/")) {
        true
    } else {
        warn!("忽略非 {org} 仓库的事件: {full_name}");
        false
    }
}

async fn handle_issue_event(state: &AppState, event: IssueEvent) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;

    // assigned 单独处理：只通知新受理人。
    if let IssueEvent::Assigned {
        issue,
        assignee_login,
    } = &event
    {
        notify_issue_assignee(state, issue, assignee_login).await;
        return Ok(());
    }

    let (issue, status) = match event {
        IssueEvent::Opened(i) => (i, IssueCardStatus::Opened),
        IssueEvent::Closed(i) => (i, IssueCardStatus::Closed),
        IssueEvent::Reopened(i) => (i, IssueCardStatus::Reopened),
        _ => return Ok(()),
    };
    let is_opened = matches!(status, IssueCardStatus::Opened);
    // 抄送管理员。
    let card = issue_card(&issue, status);
    state
        .feishu
        .send_card(&cfg.notify_id_type, &cfg.notify_id, &card)
        .await?;
    info!("Issue 通知已发：{}#{}", issue.repo_full_name, issue.number);
    // 开单时已带的受理人也通知（仅 Opened 建任务）。
    if is_opened {
        for login in &issue.assignees {
            notify_issue_assignee(state, &issue, login).await;
        }
    }
    Ok(())
}

/// 通知 Issue 受理人：绑定则私聊发卡片 + 建任务；未绑定则回退提示管理员。
/// 按 (Issue + 受理人) 去重。
async fn notify_issue_assignee(state: &AppState, issue: &IssueInfo, login: &str) {
    let cfg = &state.cfg.feishu;
    let dedup = format!(
        "assignee:{}#{}:{login}",
        issue.repo_full_name, issue.number
    );
    if !state.mark_delivery(&dedup) {
        return;
    }
    let open_id = match binding::lookup_open_id(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.binding_table_id,
        login,
    )
    .await
    {
        Ok(Some(oid)) => oid,
        Ok(None) => {
            let hint = format!(
                "⚠️ Issue #{num}（{repo}）的受理人 `{login}` 还没绑定飞书账号，无法直接通知，请其私聊我完成绑定。",
                num = issue.number,
                repo = issue.repo_full_name,
            );
            let _ = state
                .feishu
                .send_text(&cfg.notify_id_type, &cfg.notify_id, &hint)
                .await;
            warn!("Issue 受理人 {login} 未绑定，已回退通知管理员");
            return;
        }
        Err(e) => {
            error!("查询受理人 {login} 绑定失败: {e:#}");
            return;
        }
    };

    let card = issue_card(issue, IssueCardStatus::Opened);
    if let Err(e) = state.feishu.send_card("open_id", &open_id, &card).await {
        warn!("私聊 Issue 受理人 {login} 卡片失败: {e:#}");
    }
    let summary = format!("处理 Issue #{}: {}", issue.number, issue.title);
    let description = format!("{}\n{}", issue.repo_full_name, issue.url);
    match state
        .feishu
        .create_task(&summary, &description, &open_id)
        .await
    {
        Ok(guid) => info!("已通知 Issue 受理人 {login} 并建任务 {guid}"),
        Err(e) => warn!("给 Issue 受理人 {login} 建任务失败: {e:#}"),
    }
}

async fn handle_issue_comment(state: &AppState, c: CommentInfo) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;
    let card = issue_comment_card(&c);
    state
        .feishu
        .send_card(&cfg.notify_id_type, &cfg.notify_id, &card)
        .await?;
    info!("Issue 评论通知已发：{}#{}", c.repo_full_name, c.issue_number);
    Ok(())
}

async fn handle_pr_event(state: &AppState, event: PrEvent) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;
    match event {
        PrEvent::Opened(pr) => {
            // 抄送管理员一张广播卡片，并登记跟踪。
            let card = pr_card(&pr, PrCardStatus::Open, "有一条新 PR 待关注");
            let msg_id = state
                .feishu
                .send_card(&cfg.notify_id_type, &cfg.notify_id, &card)
                .await?;
            store::record_opened(
                &state.feishu,
                &cfg.base_app_token,
                &cfg.pr_table_id,
                &pr.repo_full_name,
                pr.number,
                &pr.title,
                &msg_id,
            )
            .await?;
            info!("PR opened 通知已发：{}", store::pr_key(&pr.repo_full_name, pr.number));
            // 开单时已带的受理人也通知。
            for login in &pr.assignees {
                notify_pr_assignee(state, &pr, login).await;
            }
        }
        PrEvent::Assigned { pr, assignee_login } => {
            notify_pr_assignee(state, &pr, &assignee_login).await;
        }
        PrEvent::ReviewRequested { pr, reviewer_login } => {
            handle_review_requested(state, &pr, &reviewer_login).await?;
        }
        PrEvent::Closed(pr) => {
            handle_closed(state, &pr).await?;
        }
        PrEvent::Ignored => {}
    }
    Ok(())
}

/// 通知 PR 受理人：绑定则私聊发卡片 + 建任务并登记；未绑定则回退提示管理员。
/// 按 (PR + 受理人) 去重，避免 opened 与 assigned 重复通知。
async fn notify_pr_assignee(state: &AppState, pr: &PrInfo, login: &str) {
    let cfg = &state.cfg.feishu;
    let dedup = format!("assignee:{}#{}:{login}", pr.repo_full_name, pr.number);
    if !state.mark_delivery(&dedup) {
        return;
    }
    let open_id = match binding::lookup_open_id(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.binding_table_id,
        login,
    )
    .await
    {
        Ok(Some(oid)) => oid,
        Ok(None) => {
            let hint = format!(
                "⚠️ PR #{num}（{repo}）的受理人 `{login}` 还没绑定飞书账号，无法直接通知，请其私聊我完成绑定。",
                num = pr.number,
                repo = pr.repo_full_name,
            );
            let _ = state
                .feishu
                .send_text(&cfg.notify_id_type, &cfg.notify_id, &hint)
                .await;
            warn!("PR 受理人 {login} 未绑定，已回退通知管理员");
            return;
        }
        Err(e) => {
            error!("查询受理人 {login} 绑定失败: {e:#}");
            return;
        }
    };

    let card = pr_card(pr, PrCardStatus::Open, "您有一条 PR 待处理");
    if let Err(e) = state.feishu.send_card("open_id", &open_id, &card).await {
        warn!("私聊受理人 {login} 卡片失败: {e:#}");
    }
    let summary = format!("处理 PR #{}: {}", pr.number, pr.title);
    let description = format!("{}\n{}", pr.repo_full_name, pr.url);
    match state
        .feishu
        .create_task(&summary, &description, &open_id)
        .await
    {
        Ok(guid) => {
            let _ = store::record_review_task(
                &state.feishu,
                &cfg.base_app_token,
                &cfg.pr_table_id,
                &pr.repo_full_name,
                pr.number,
                login,
                &guid,
            )
            .await;
            info!("已通知 PR 受理人 {login} 并建任务 {guid}");
        }
        Err(e) => warn!("给 PR 受理人 {login} 建任务失败: {e:#}"),
    }
}

async fn handle_review_requested(
    state: &AppState,
    pr: &PrInfo,
    reviewer_login: &str,
) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;
    let open_id = binding::lookup_open_id(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.binding_table_id,
        reviewer_login,
    )
    .await?;

    let Some(open_id) = open_id else {
        // 未绑定：群里提示，仍不报错。
        let hint = format!(
            "⚠️ GitHub 用户 `{reviewer_login}` 被请求 review PR #{num}（{repo}），但还没绑定飞书账号，无法派任务。请该同学私聊我完成绑定。",
            num = pr.number,
            repo = pr.repo_full_name,
        );
        state
            .feishu
            .send_text(&cfg.notify_id_type, &cfg.notify_id, &hint)
            .await?;
        warn!("reviewer {reviewer_login} 未绑定");
        return Ok(());
    };

    let summary = format!("Review PR #{}: {}", pr.number, pr.title);
    let description = format!("{}\n{}", pr.repo_full_name, pr.url);
    let task_guid = state
        .feishu
        .create_task(&summary, &description, &open_id)
        .await?;

    // 私聊推一张卡片。
    let card = pr_card(pr, PrCardStatus::Open, "您有一条 PR 待 Review");
    if let Err(e) = state.feishu.send_card("open_id", &open_id, &card).await {
        warn!("私聊 reviewer 卡片失败（任务已建）: {e:#}");
    }

    store::record_review_task(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.pr_table_id,
        &pr.repo_full_name,
        pr.number,
        reviewer_login,
        &task_guid,
    )
    .await?;
    info!("已给 {reviewer_login} 建 review 任务 {task_guid}");
    Ok(())
}

async fn handle_closed(state: &AppState, pr: &PrInfo) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;
    let (status, lead) = if pr.merged {
        (PrCardStatus::Merged, "您跟进的 PR 已合并")
    } else {
        (PrCardStatus::Closed, "您跟进的 PR 已关闭")
    };
    let status_str = if pr.merged { "merged" } else { "closed" };

    let record = store::get(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.pr_table_id,
        &pr.repo_full_name,
        pr.number,
    )
    .await?;

    let Some(record) = record else {
        info!("closed 事件无跟踪记录（可能 opened 时服务未上线），跳过收尾");
        return Ok(());
    };

    // 更新卡片状态。
    if let Some(msg_id) = &record.message_id {
        let card = pr_card(pr, status, lead);
        if let Err(e) = state.feishu.patch_card(msg_id, &card).await {
            warn!("更新群卡片失败: {e:#}");
        }
    }

    // 完成所有 review 任务。
    for guid in &record.task_guids {
        if let Err(e) = state.feishu.complete_task(guid).await {
            warn!("完成任务 {guid} 失败: {e:#}");
        }
    }

    store::set_status(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.pr_table_id,
        &record.record_id,
        status_str,
    )
    .await?;
    info!("PR {} 收尾完成（{status_str}）", store::pr_key(&pr.repo_full_name, pr.number));
    Ok(())
}

/// POST /webhook/feishu —— 事件订阅 / 卡片回调。
pub async fn feishu_webhook(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let cfg = &state.cfg.feishu;
    let plaintext = match callback::decrypt_body(&body, &cfg.encrypt_key) {
        Ok(t) => t,
        Err(e) => {
            error!("飞书回调解密失败: {e:#}");
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({}))).into_response();
        }
    };

    let parsed = match callback::parse(&plaintext, &cfg.verification_token) {
        Ok(p) => p,
        Err(e) => {
            warn!("飞书回调解析/校验失败: {e:#}");
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({}))).into_response();
        }
    };

    match parsed {
        Callback::UrlVerification { challenge } => {
            Json(serde_json::json!({ "challenge": challenge })).into_response()
        }
        Callback::BindGithub {
            open_id,
            github_username,
            dedup,
        } => {
            // 同一次点击的新旧两份回调按卡片令牌去重，仅第一份真正写库。
            if !state.mark_delivery(&dedup) {
                return Json(serde_json::json!({
                    "toast": { "type": "success", "content": "已绑定" }
                }))
                .into_response();
            }
            let msg = match binding::upsert_binding(
                &state.feishu,
                &cfg.base_app_token,
                &cfg.binding_table_id,
                &open_id,
                &github_username,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    error!("绑定写入失败: {e:#}");
                    "绑定失败，请稍后重试".to_string()
                }
            };
            // 卡片回调可返回 toast 提示。
            Json(serde_json::json!({
                "toast": { "type": "success", "content": msg }
            }))
            .into_response()
        }
        Callback::SendBindingCard { open_id } => {
            // 异步回卡片；即时返回 200 让飞书不重推。
            let feishu = state.feishu.clone();
            tokio::spawn(async move {
                let card = binding_card();
                if let Err(e) = feishu.send_card("open_id", &open_id, &card).await {
                    error!("发送绑定卡片失败: {e:#}");
                }
            });
            Json(serde_json::json!({})).into_response()
        }
        Callback::AddToTask {
            open_id,
            kind,
            repo,
            title,
            url,
            dedup,
        } => {
            // 去重：避免新旧两份回调建两条任务。
            if !state.mark_delivery(&dedup) {
                return Json(serde_json::json!({
                    "toast": { "type": "success", "content": "已添加到你的飞书任务" }
                }))
                .into_response();
            }
            let summary = format!("处理 {kind}: {title}");
            let description = format!("{repo}\n{url}");
            let msg = match state
                .feishu
                .create_task(&summary, &description, &open_id)
                .await
            {
                Ok(guid) => {
                    info!("已为 {open_id} 建任务 {guid}（{kind}）");
                    "已添加到你的飞书任务".to_string()
                }
                Err(e) => {
                    error!("添加到任务失败: {e:#}");
                    "添加失败，请稍后重试".to_string()
                }
            };
            Json(serde_json::json!({
                "toast": { "type": "success", "content": msg }
            }))
            .into_response()
        }
        Callback::Ignored => Json(serde_json::json!({})).into_response(),
    }
}
