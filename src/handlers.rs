//! HTTP 入口与事件编排：把 GitHub 事件翻译成飞书动作，处理飞书卡片回调。

use crate::cards::{binding_card, comment_card, issue_card, pr_card, pr_review_card, IssueCardStatus, PrCardStatus};
use crate::feishu::callback::{self, Callback};
use crate::github::events::{PrEvent, PrInfo, PullRequestPayload, ReviewInfo, ReviewPayload};
use crate::github::issues::{CommentInfo, IssueEvent, IssuesPayload, IssueCommentPayload};
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
            if let Some(c) = payload.as_created_comment() {
                tokio::spawn(async move {
                    if let Err(e) = handle_issue_comment(&state, c).await {
                        error!("处理评论失败: {e:#}");
                    }
                });
            }
        }
        "pull_request_review" => {
            let payload: ReviewPayload = match serde_json::from_slice(&body) {
                Ok(p) => p,
                Err(e) => {
                    error!("解析 pull_request_review payload 失败: {e}");
                    return StatusCode::BAD_REQUEST;
                }
            };
            if !in_org(&state.cfg.github.org, &payload.repository.full_name) {
                return StatusCode::OK;
            }
            if let Some(review) = payload.as_submitted() {
                tokio::spawn(async move {
                    if let Err(e) = handle_review_submitted(&state, review).await {
                        error!("处理 PR review 失败: {e:#}");
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
    match event {
        IssueEvent::Opened(issue) => {
            let card = issue_card(&issue, IssueCardStatus::Opened);
            let task = Some((
                format!("处理 Issue #{}: {}", issue.number, issue.title),
                format!("{}\n{}", issue.repo_full_name, issue.url),
            ));
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &issue.assignees, &card, task, Some("assign"), true,
            )
            .await;
        }
        IssueEvent::Assigned { issue, assignee_login } => {
            let card = issue_card(&issue, IssueCardStatus::Opened);
            let task = Some((
                format!("处理 Issue #{}: {}", issue.number, issue.title),
                format!("{}\n{}", issue.repo_full_name, issue.url),
            ));
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                std::slice::from_ref(&assignee_login), &card, task, Some("assign"), true,
            )
            .await;
        }
        IssueEvent::Closed(issue) => {
            let card = issue_card(&issue, IssueCardStatus::Closed);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &issue.assignees, &card, None, None, false,
            )
            .await;
        }
        IssueEvent::Reopened(issue) => {
            let card = issue_card(&issue, IssueCardStatus::Reopened);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &issue.assignees, &card, None, None, false,
            )
            .await;
        }
        IssueEvent::Ignored => {}
    }
    Ok(())
}

async fn handle_issue_comment(state: &AppState, c: CommentInfo) -> anyhow::Result<()> {
    if c.is_pr {
        // PR 评论 → 通知 PR 作者本人；作者评论自己的 PR 则跳过。
        if c.commenter == c.author {
            info!("PR 作者评论自己的 PR，跳过：{}#{}", c.repo_full_name, c.issue_number);
            return Ok(());
        }
        let card = comment_card(&c, "Pull Request 新评论", "您的 Pull Request 有新评论");
        deliver_to_assignees(
            state, &c.repo_full_name, c.issue_number, true, "创建者",
            std::slice::from_ref(&c.author), &card, None, None, false,
        )
        .await;
    } else {
        // Issue 评论 → 通知受理人（排除评论人本人）。
        let recipients: Vec<String> = c
            .assignees
            .iter()
            .filter(|a| **a != c.commenter)
            .cloned()
            .collect();
        // 仅在本就无受理人时才回退管理员；受理人恰好是评论人则不回退（有人在跟进）。
        let admin_fb = c.assignees.is_empty();
        let card = comment_card(&c, "Issue 新评论", "您受理的 Issue 有一条新评论待处理");
        deliver_to_assignees(
            state, &c.repo_full_name, c.issue_number, false, "受理人",
            &recipients, &card, None, None, admin_fb,
        )
        .await;
    }
    info!("评论已处理：{}#{} (is_pr={})", c.repo_full_name, c.issue_number, c.is_pr);
    Ok(())
}

async fn handle_pr_event(state: &AppState, event: PrEvent) -> anyhow::Result<()> {
    let cfg = &state.cfg.feishu;
    match event {
        PrEvent::Opened(pr) => {
            // 登记跟踪（供 closed 完成任务）。message_id 留空：不再固定抄送管理员。
            store::record_opened(
                &state.feishu, &cfg.base_app_token, &cfg.pr_table_id,
                &pr.repo_full_name, pr.number, &pr.title, "",
            )
            .await?;
            let card = pr_card(&pr, PrCardStatus::Open, "您有一条 Pull Request 待处理");
            let task = Some((
                format!("处理 Pull Request #{}: {}", pr.number, pr.title),
                format!("{}\n{}", pr.repo_full_name, pr.url),
            ));
            deliver_to_assignees(
                state, &pr.repo_full_name, pr.number, true, "受理人",
                &pr.assignees, &card, task, Some("assign"), true,
            )
            .await;
            info!("PR opened 处理完成：{}", store::pr_key(&pr.repo_full_name, pr.number));
        }
        PrEvent::Assigned { pr, assignee_login } => {
            let card = pr_card(&pr, PrCardStatus::Open, "您有一条 Pull Request 待处理");
            let task = Some((
                format!("处理 Pull Request #{}: {}", pr.number, pr.title),
                format!("{}\n{}", pr.repo_full_name, pr.url),
            ));
            deliver_to_assignees(
                state, &pr.repo_full_name, pr.number, true, "受理人",
                std::slice::from_ref(&assignee_login), &card, task, Some("assign"), true,
            )
            .await;
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

/// 把卡片投递给受理人：
/// - 无受理人 → 回退发给管理员；
/// - 受理人未绑定 / 发送失败 → 回退提示管理员；
/// - task=Some 时给绑定的受理人建任务（is_pr 时把 guid 记入 PR 跟踪表）；
/// - dedup_kind=Some 时按 (item+受理人+kind) 去重（opened 与 assigned 不重复）。
#[allow(clippy::too_many_arguments)]
async fn deliver_to_assignees(
    state: &AppState,
    repo: &str,
    number: u64,
    is_pr: bool,
    role: &str,
    assignees: &[String],
    card: &serde_json::Value,
    task: Option<(String, String)>,
    dedup_kind: Option<&str>,
    admin_fallback: bool,
) {
    let cfg = &state.cfg.feishu;
    let label = if is_pr { "Pull Request" } else { "Issue" };

    // 无收件人 → 视需要回退给管理员（带显著"管理员通知"标识）。
    if assignees.is_empty() {
        if admin_fallback {
            let notice = crate::cards::to_admin_notice(card);
            if let Err(e) = state
                .feishu
                .send_card(&cfg.notify_id_type, &cfg.notify_id, &notice)
                .await
            {
                error!("回退通知管理员失败: {e:#}");
            }
        }
        return;
    }

    for login in assignees {
        if let Some(kind) = dedup_kind {
            let key = format!("assignee:{repo}#{number}:{login}:{kind}");
            if !state.mark_delivery(&key) {
                continue;
            }
        }
        match binding::lookup_open_id(&state.feishu, &cfg.base_app_token, &cfg.binding_table_id, login)
            .await
        {
            Ok(Some(open_id)) => {
                if let Err(e) = state.feishu.send_card("open_id", &open_id, card).await {
                    let hint = format!("{label} #{number}（{repo}）通知{role} `{login}` 失败：{e}");
                    let _ = state
                        .feishu
                        .send_text(&cfg.notify_id_type, &cfg.notify_id, &hint)
                        .await;
                    warn!("通知{label}{role} {login} 失败，已回退管理员: {e:#}");
                    continue;
                }
                if let Some((summary, description)) = &task {
                    match state.feishu.create_task(summary, description, &open_id).await {
                        Ok(guid) => {
                            if is_pr {
                                let _ = store::record_review_task(
                                    &state.feishu, &cfg.base_app_token, &cfg.pr_table_id,
                                    repo, number, login, &guid,
                                )
                                .await;
                            }
                            info!("已通知{label}{role} {login} 并建任务 {guid}");
                        }
                        Err(e) => warn!("给{label}{role} {login} 建任务失败: {e:#}"),
                    }
                } else {
                    info!("已通知{label}{role} {login}");
                }
            }
            Ok(None) => {
                let hint = format!(
                    "{label} #{number}（{repo}）的{role} `{login}` 还没绑定飞书账号，无法通知，请其私聊我完成绑定。"
                );
                let _ = state
                    .feishu
                    .send_text(&cfg.notify_id_type, &cfg.notify_id, &hint)
                    .await;
                warn!("{label}{role} {login} 未绑定，已回退管理员");
            }
            Err(e) => error!("查询受理人 {login} 绑定失败: {e:#}"),
        }
    }
}

/// 审查者提交 review → 通知 PR 作者「审查已完成」。
async fn handle_review_submitted(state: &AppState, review: ReviewInfo) -> anyhow::Result<()> {
    let card = pr_review_card(&review.pr, &review.reviewer, &review.state);
    deliver_to_assignees(
        state,
        &review.pr.repo_full_name,
        review.pr.number,
        true,
        "创建者",
        std::slice::from_ref(&review.pr.author),
        &card,
        None,
        None,
        false,
    )
    .await;
    info!(
        "PR review 已通知作者：{}#{} ({})",
        review.pr.repo_full_name, review.pr.number, review.state
    );
    Ok(())
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
            "GitHub 用户 `{reviewer_login}` 被请求 review Pull Request #{num}（{repo}），但还没绑定飞书账号，无法派任务。请该同学私聊我完成绑定。",
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

    let summary = format!("Review Pull Request #{}: {}", pr.number, pr.title);
    let description = format!("{}\n{}", pr.repo_full_name, pr.url);
    let task_guid = state
        .feishu
        .create_task(&summary, &description, &open_id)
        .await?;

    // 私聊推一张卡片。
    let card = pr_card(pr, PrCardStatus::Open, "您有一条 Pull Request 待 Review");
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
        (PrCardStatus::Merged, "您跟进的 Pull Request 已合并")
    } else {
        (PrCardStatus::Closed, "您跟进的 Pull Request 已关闭")
    };
    let status_str = if pr.merged { "merged" } else { "closed" };

    // 完成相关任务并更新跟踪状态。
    if let Some(record) = store::get(
        &state.feishu,
        &cfg.base_app_token,
        &cfg.pr_table_id,
        &pr.repo_full_name,
        pr.number,
    )
    .await?
    {
        for guid in &record.task_guids {
            if let Err(e) = state.feishu.complete_task(guid).await {
                warn!("完成任务 {guid} 失败: {e:#}");
            }
        }
        let _ = store::set_status(
            &state.feishu,
            &cfg.base_app_token,
            &cfg.pr_table_id,
            &record.record_id,
            status_str,
        )
        .await;
    }

    // 通知 PR 作者：已合并/关闭。
    let author_lead = if pr.merged { "您的 Pull Request 已合并" } else { "您的 Pull Request 已关闭" };
    let author_card = pr_card(pr, status, author_lead);
    deliver_to_assignees(
        state, &pr.repo_full_name, pr.number, true, "创建者",
        std::slice::from_ref(&pr.author), &author_card, None, None, false,
    )
    .await;

    // 通知受理人（排除作者，避免作者=受理人时重复通知）：跟进的 PR 已合并/关闭。
    let others: Vec<String> = pr
        .assignees
        .iter()
        .filter(|a| **a != pr.author)
        .cloned()
        .collect();
    let card = pr_card(pr, status, lead);
    deliver_to_assignees(
        state, &pr.repo_full_name, pr.number, true, "受理人",
        &others, &card, None, None, false,
    )
    .await;
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
