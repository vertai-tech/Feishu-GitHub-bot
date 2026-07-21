//! HTTP 入口与事件编排：把 GitHub 事件翻译成飞书动作，处理飞书卡片回调。

use crate::cards::{binding_card, comment_card, issue_card, pr_card, pr_review_card, unassigned_card, updated_card, IssueCardStatus, PrCardStatus};
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
            // 开单本身不通知；受理人由 assigned 事件负责（GitHub 开单带受理人时也会发 assigned）。
            info!("Issue opened：{}#{}", issue.repo_full_name, issue.number);
        }
        IssueEvent::Assigned { issue, assignee_login } => {
            let card = issue_card(&issue, IssueCardStatus::Opened);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                std::slice::from_ref(&assignee_login), &card, false,
            )
            .await;
            track_pending(state, &issue.repo_full_name, issue.number, &issue.title, &issue.url, false, &assignee_login, "受理人").await;
        }
        IssueEvent::Unassigned { issue, assignee_login } => {
            store::mark_done(state, &issue.repo_full_name, issue.number, &assignee_login).await;
            let card = unassigned_card(false, &issue.repo_full_name, issue.number, &issue.title, &issue.url);
            notify_person_private(state, &assignee_login, &card).await;
        }
        IssueEvent::Edited { issue, editor_login } => {
            store::reset_pending_on_activity(state, &issue.repo_full_name, issue.number, &editor_login).await;
            let others: Vec<String> = issue.assignees.iter().filter(|a| **a != editor_login).cloned().collect();
            let card = updated_card(false, &issue.repo_full_name, issue.number, &issue.title, &issue.url);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &others, &card, false,
            )
            .await;
        }
        IssueEvent::Closed(issue) => {
            store::mark_all_done(state, &issue.repo_full_name, issue.number).await;
            let card = issue_card(&issue, IssueCardStatus::Closed);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &issue.assignees, &card, false,
            )
            .await;
        }
        IssueEvent::Removed(issue) => {
            // 删除/转移 → 停止 SLA 跟踪，不发通知。
            store::mark_all_done(state, &issue.repo_full_name, issue.number).await;
            info!("Issue 删除/转移，停止跟踪：{}#{}", issue.repo_full_name, issue.number);
        }
        IssueEvent::Reopened(issue) => {
            let card = issue_card(&issue, IssueCardStatus::Reopened);
            deliver_to_assignees(
                state, &issue.repo_full_name, issue.number, false, "受理人",
                &issue.assignees, &card, false,
            )
            .await;
            // 重新打开 → 对受理人重启 SLA 计时。
            for login in &issue.assignees {
                track_pending(state, &issue.repo_full_name, issue.number, &issue.title, &issue.url, false, login, "受理人").await;
            }
        }
        IssueEvent::Ignored => {}
    }
    Ok(())
}

async fn handle_issue_comment(state: &AppState, c: CommentInfo) -> anyhow::Result<()> {
    // SLA：评论人若是负责人→其行 done；否则该项 pending 行重新计时。
    store::reset_pending_on_activity(state, &c.repo_full_name, c.issue_number, &c.commenter).await;

    if c.is_pr {
        // PR 评论 → 通知 PR 作者本人；作者评论自己的 PR 则跳过。
        if c.commenter == c.author {
            info!("PR 作者评论自己的 PR，跳过：{}#{}", c.repo_full_name, c.issue_number);
            return Ok(());
        }
        let card = comment_card(&c, "Pull Request 新评论", "您的 Pull Request 有新评论");
        deliver_to_assignees(
            state, &c.repo_full_name, c.issue_number, true, "创建者",
            std::slice::from_ref(&c.author), &card, false,
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
            &recipients, &card, admin_fb,
        )
        .await;
    }
    info!("评论已处理：{}#{} (is_pr={})", c.repo_full_name, c.issue_number, c.is_pr);
    Ok(())
}

async fn handle_pr_event(state: &AppState, event: PrEvent) -> anyhow::Result<()> {
    match event {
        PrEvent::Opened(pr) => {
            // 开单本身不通知；受理人由 assigned 事件负责。
            info!("PR opened：{}#{}", pr.repo_full_name, pr.number);
        }
        PrEvent::Assigned { pr, assignee_login } => {
            let card = pr_card(&pr, PrCardStatus::Open, "您有一条 Pull Request 待处理");
            deliver_to_assignees(
                state, &pr.repo_full_name, pr.number, true, "受理人",
                std::slice::from_ref(&assignee_login), &card, false,
            )
            .await;
            track_pending(state, &pr.repo_full_name, pr.number, &pr.title, &pr.url, true, &assignee_login, "受理人").await;
        }
        PrEvent::Unassigned { pr, assignee_login } => {
            store::mark_done(state, &pr.repo_full_name, pr.number, &assignee_login).await;
            let card = unassigned_card(true, &pr.repo_full_name, pr.number, &pr.title, &pr.url);
            notify_person_private(state, &assignee_login, &card).await;
        }
        PrEvent::Edited { pr, editor_login } => {
            // 正文更新：编辑者本人的行 done，其余受理人重新计时并私聊提示更新。
            store::reset_pending_on_activity(state, &pr.repo_full_name, pr.number, &editor_login).await;
            let others: Vec<String> = pr.assignees.iter().filter(|a| **a != editor_login).cloned().collect();
            let card = updated_card(true, &pr.repo_full_name, pr.number, &pr.title, &pr.url);
            deliver_to_assignees(
                state, &pr.repo_full_name, pr.number, true, "受理人",
                &others, &card, false,
            )
            .await;
        }
        PrEvent::ReviewRequested { pr, reviewer_login } => {
            handle_review_requested(state, &pr, &reviewer_login).await?;
        }
        PrEvent::ReadyForReview(pr) => {
            notify_pr_actionable(
                state, &pr,
                "Pull Request 已就绪（草案转正式），待处理",
                "您的 Pull Request 已就绪（草案转正式）",
            )
            .await;
        }
        PrEvent::Reopened(pr) => {
            notify_pr_actionable(
                state, &pr,
                "Pull Request 已重新打开，待处理",
                "您的 Pull Request 已重新打开",
            )
            .await;
        }
        PrEvent::ConvertedToDraft(pr) => {
            // 转为草案：暂停 SLA。有受理人→通知受理人暂缓；无受理人→私聊作者。
            store::mark_all_done(state, &pr.repo_full_name, pr.number).await;
            if pr.assignees.is_empty() {
                let card = pr_card(&pr, PrCardStatus::Closed, "您的 Pull Request 已转为草案");
                deliver_to_assignees(
                    state, &pr.repo_full_name, pr.number, true, "创建者",
                    std::slice::from_ref(&pr.author), &card, false,
                )
                .await;
            } else {
                let card = pr_card(&pr, PrCardStatus::Closed, "Pull Request 已转为草案，暂缓处理");
                deliver_to_assignees(
                    state, &pr.repo_full_name, pr.number, true, "受理人",
                    &pr.assignees, &card, false,
                )
                .await;
            }
        }
        PrEvent::Closed(pr) => {
            handle_closed(state, &pr).await?;
        }
        PrEvent::Ignored => {}
    }
    Ok(())
}

/// 把卡片投递给受理人（不自动建任务，收件人可用卡片上的「添加到任务」自行创建）：
/// - 无受理人 → 视 admin_fallback 回退发给管理员/群；
/// - 受理人未绑定 / 发送失败 → 回退提示管理员/群。
#[allow(clippy::too_many_arguments)]
async fn deliver_to_assignees(
    state: &AppState,
    repo: &str,
    number: u64,
    is_pr: bool,
    role: &str,
    assignees: &[String],
    card: &serde_json::Value,
    admin_fallback: bool,
) {
    let cfg = &state.cfg.feishu;
    let label = if is_pr { "Pull Request" } else { "Issue" };

    // 无收件人 → 视需要回退给管理员（带显著"管理员通知"标识）。
    if assignees.is_empty() {
        if admin_fallback {
            let notice = crate::cards::to_broadcast_notice(card);
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
                info!("已通知{label}{role} {login}");
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
    // SLA：reviewer 提交了 review → 其待办行 done。
    store::mark_done(state, &review.pr.repo_full_name, review.pr.number, &review.reviewer).await;
    let card = pr_review_card(&review.pr, &review.reviewer, &review.state);
    deliver_to_assignees(
        state,
        &review.pr.repo_full_name,
        review.pr.number,
        true,
        "创建者",
        std::slice::from_ref(&review.pr.author),
        &card,
        false,
    )
    .await;
    info!(
        "PR review 已通知作者：{}#{} ({})",
        review.pr.repo_full_name, review.pr.number, review.state
    );
    Ok(())
}

/// 私聊某人（若已绑定）一张卡片；未绑定则静默跳过（不发群）。
async fn notify_person_private(state: &AppState, login: &str, card: &serde_json::Value) {
    let cfg = &state.cfg.feishu;
    if let Ok(Some(open_id)) =
        binding::lookup_open_id(&state.feishu, &cfg.base_app_token, &cfg.binding_table_id, login)
            .await
    {
        if let Err(e) = state.feishu.send_card("open_id", &open_id, card).await {
            warn!("私聊 {login} 卡片失败: {e:#}");
        }
    }
}

/// PR 变为「可处理」（就绪 / 重新打开）时的通知：
/// 有受理人→通知受理人并重启 SLA；无受理人→私聊作者。
async fn notify_pr_actionable(state: &AppState, pr: &PrInfo, assignee_lead: &str, author_lead: &str) {
    if pr.assignees.is_empty() {
        let card = pr_card(pr, PrCardStatus::Open, author_lead);
        deliver_to_assignees(
            state, &pr.repo_full_name, pr.number, true, "创建者",
            std::slice::from_ref(&pr.author), &card, false,
        )
        .await;
    } else {
        let card = pr_card(pr, PrCardStatus::Open, assignee_lead);
        deliver_to_assignees(
            state, &pr.repo_full_name, pr.number, true, "受理人",
            &pr.assignees, &card, false,
        )
        .await;
        for login in &pr.assignees {
            track_pending(state, &pr.repo_full_name, pr.number, &pr.title, &pr.url, true, login, "受理人").await;
        }
    }
}

/// 若该负责人已绑定飞书，则登记一条 SLA 待办跟踪（用于未处理提醒）。
async fn track_pending(
    state: &AppState,
    repo: &str,
    number: u64,
    title: &str,
    url: &str,
    is_pr: bool,
    login: &str,
    role: &str,
) {
    let cfg = &state.cfg.feishu;
    if let Ok(Some(open_id)) =
        binding::lookup_open_id(&state.feishu, &cfg.base_app_token, &cfg.binding_table_id, login)
            .await
    {
        store::upsert_pending(state, repo, number, title, url, is_pr, login, &open_id, role).await;
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

    // 私聊推一张卡片（不自动建任务；可点卡片上的「添加到任务」）。
    let card = pr_card(pr, PrCardStatus::Open, "您有一条 Pull Request 待 Review");
    if let Err(e) = state.feishu.send_card("open_id", &open_id, &card).await {
        warn!("私聊 reviewer 卡片失败: {e:#}");
    }
    // 登记 SLA 待办（reviewer）。
    store::upsert_pending(
        state, &pr.repo_full_name, pr.number, &pr.title, &pr.url, true, reviewer_login, &open_id, "reviewer",
    )
    .await;
    info!("已通知 reviewer {reviewer_login}");
    Ok(())
}

async fn handle_closed(state: &AppState, pr: &PrInfo) -> anyhow::Result<()> {
    let (status, lead) = if pr.merged {
        (PrCardStatus::Merged, "您跟进的 Pull Request 已合并")
    } else {
        (PrCardStatus::Closed, "您跟进的 Pull Request 已关闭")
    };
    let status_str = if pr.merged { "merged" } else { "closed" };

    // SLA：关闭/合并 → 该 PR 所有待办跟踪行标记 done。
    store::mark_all_done(state, &pr.repo_full_name, pr.number).await;

    // 通知 PR 作者：已合并/关闭。
    let author_lead = if pr.merged { "您的 Pull Request 已合并" } else { "您的 Pull Request 已关闭" };
    let author_card = pr_card(pr, status, author_lead);
    deliver_to_assignees(
        state, &pr.repo_full_name, pr.number, true, "创建者",
        std::slice::from_ref(&pr.author), &author_card, false,
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
        &others, &card, false,
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
