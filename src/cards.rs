//! 飞书交互卡片 JSON 模板。
//!
//! 文案以「受理人视角、可操作」为原则（如"您有一条 Issue 待处理"）。
//! 视觉上统一：头部图标+色条、加粗引导语、两列结构化字段、分割线、底部说明、主按钮。

use crate::github::events::PrInfo;
use crate::github::issues::{CommentInfo, IssueInfo};
use serde_json::{json, Value};

/// 两列短字段中的一格：`**标题**\n内容`。
fn short_field(title: &str, content: &str) -> Value {
    json!({
        "is_short": true,
        "text": { "tag": "lark_md", "content": format!("**{title}**\n{content}") }
    })
}

/// 底部灰色说明行。
fn footer(text: &str) -> Value {
    json!({
        "tag": "note",
        "elements": [{ "tag": "plain_text", "content": text }]
    })
}

/// 底部按钮行：一个跳转按钮 + 一个「添加到任务」回调按钮。
/// `task_value` 会在点击时随 card.action.trigger 回传，用于建任务。
fn button_row(link_text: &str, link_url: &str, task_value: Value) -> Value {
    json!({
        "tag": "action",
        "actions": [
            {
                "tag": "button",
                "text": { "tag": "plain_text", "content": link_text },
                "type": "primary",
                "url": link_url
            },
            {
                "tag": "button",
                "text": { "tag": "plain_text", "content": "添加到任务" },
                "type": "default",
                "value": task_value
            }
        ]
    })
}

/// 组装一张标准通知卡片。
#[allow(clippy::too_many_arguments)]
fn notification_card(
    template: &str,
    header_title: &str,
    lead: &str,
    subject: &str,
    fields: Vec<Value>,
    extra: Vec<Value>,
    button_text: &str,
    button_url: &str,
    task_value: Value,
    footer_text: &str,
) -> Value {
    let mut elements = vec![
        json!({ "tag": "div", "text": { "tag": "lark_md", "content": format!("**{lead}**") } }),
        json!({ "tag": "div", "text": { "tag": "lark_md", "content": subject } }),
        json!({ "tag": "div", "fields": fields }),
    ];
    elements.extend(extra);
    elements.push(json!({ "tag": "hr" }));
    elements.push(button_row(button_text, button_url, task_value));
    elements.push(footer(footer_text));

    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "title": { "tag": "plain_text", "content": header_title },
            "template": template
        },
        "elements": elements
    })
}

/// 「添加到任务」按钮回传的 value：点击时据此建任务。
fn task_value(kind: &str, repo: &str, title: &str, url: &str) -> Value {
    json!({
        "action": "add_to_task",
        "kind": kind,     // 如 "Issue #2" / "PR #12"
        "repo": repo,
        "title": title,
        "url": url
    })
}

/// PR 状态，用于卡片头部颜色与文案。
pub enum PrCardStatus {
    Open,
    Merged,
    Closed,
}

impl PrCardStatus {
    fn header(&self) -> (&'static str, &'static str) {
        // (模板色, 头部标题)
        match self {
            PrCardStatus::Open => ("blue", "Pull Request"),
            PrCardStatus::Merged => ("green", "PR 已合并"),
            PrCardStatus::Closed => ("grey", "PR 已关闭"),
        }
    }
}

/// PR 通知卡片。`lead` 是随场景变化的待办式引导语
/// （如"您有一条 PR 待 Review" / "有一条新 PR 待关注"）。
pub fn pr_card(pr: &PrInfo, status: PrCardStatus, lead: &str) -> Value {
    let (template, header_title) = status.header();
    notification_card(
        template,
        header_title,
        lead,
        &format!("**#{} {}**", pr.number, pr.title),
        vec![
            short_field("仓库", &format!("`{}`", pr.repo_full_name)),
            short_field("提交人", &pr.author),
        ],
        vec![json!({
            "tag": "div",
            "text": { "tag": "lark_md", "content": format!("**分支**　`{}` → `{}`", pr.head_ref, pr.base_ref) }
        })],
        "查看 PR",
        &pr.url,
        task_value(&format!("PR #{}", pr.number), &pr.repo_full_name, &pr.title, &pr.url),
        "来自 GitHub · 点按钮查看详情",
    )
}

/// Issue 状态，决定头部与引导语。
pub enum IssueCardStatus {
    Opened,
    Closed,
    Reopened,
}

impl IssueCardStatus {
    /// (模板色, 头部标题, 待办式引导语)
    fn parts(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            IssueCardStatus::Opened => ("orange", "Issue", "您有一条 Issue 待处理"),
            IssueCardStatus::Closed => ("grey", "Issue 已关闭", "您受理的 Issue 已关闭"),
            IssueCardStatus::Reopened => {
                ("orange", "Issue 重新打开", "您受理的 Issue 已重新打开，待处理")
            }
        }
    }
}

/// Issue 通知卡片。
pub fn issue_card(issue: &IssueInfo, status: IssueCardStatus) -> Value {
    let (template, header_title, lead) = status.parts();
    notification_card(
        template,
        header_title,
        lead,
        &format!("**#{} {}**", issue.number, issue.title),
        vec![
            short_field("仓库", &format!("`{}`", issue.repo_full_name)),
            short_field("提出人", &issue.author),
        ],
        vec![],
        "查看 Issue",
        &issue.url,
        task_value(&format!("Issue #{}", issue.number), &issue.repo_full_name, &issue.title, &issue.url),
        "来自 GitHub · 点按钮查看详情",
    )
}

/// Issue 新评论卡片。评论正文过长时截断。
pub fn issue_comment_card(c: &CommentInfo) -> Value {
    const MAX: usize = 300;
    let mut body: String = c.body.chars().take(MAX).collect();
    if c.body.chars().count() > MAX {
        body.push_str("…");
    }
    // 用引用块呈现评论内容。
    let quote = body
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    notification_card(
        "blue",
        "Issue 新评论",
        "您受理的 Issue 有一条新评论待处理",
        &format!("**#{} {}**", c.issue_number, c.issue_title),
        vec![
            short_field("仓库", &format!("`{}`", c.repo_full_name)),
            short_field("评论人", &c.commenter),
        ],
        vec![json!({
            "tag": "div",
            "text": { "tag": "lark_md", "content": quote }
        })],
        "查看评论",
        &c.comment_url,
        task_value(&format!("Issue #{}", c.issue_number), &c.repo_full_name, &c.issue_title, &c.comment_url),
        "来自 GitHub · 点按钮查看详情",
    )
}

/// 用示例数据生成每一种卡片，供 `dump-cards` 预览。返回 (名称, 卡片 JSON)。
pub fn sample_cards() -> Vec<(&'static str, Value)> {
    let pr = PrInfo {
        repo_full_name: "damesck/testrepo".into(),
        number: 12,
        title: "重构登录模块".into(),
        url: "https://github.com/damesck/testrepo/pull/12".into(),
        author: "zhang-san".into(),
        base_ref: "main".into(),
        head_ref: "feat/login".into(),
        merged: false,
        assignees: vec![],
    };
    let issue = IssueInfo {
        repo_full_name: "damesck/testrepo".into(),
        number: 2,
        title: "登录页在移动端错位".into(),
        url: "https://github.com/damesck/testrepo/issues/2".into(),
        author: "zhang-san".into(),
        assignees: vec![],
    };
    let comment = CommentInfo {
        repo_full_name: "damesck/testrepo".into(),
        issue_number: 2,
        issue_title: "登录页在移动端错位".into(),
        commenter: "li-si".into(),
        body: "我这边也能复现，iOS Safari 上按钮溢出了容器。\n附一张截图待补。".into(),
        comment_url: "https://github.com/damesck/testrepo/issues/2#issuecomment-2".into(),
    };

    vec![
        ("Issue 新建", issue_card(&issue, IssueCardStatus::Opened)),
        ("Issue 关闭", issue_card(&issue, IssueCardStatus::Closed)),
        ("Issue 重新打开", issue_card(&issue, IssueCardStatus::Reopened)),
        ("Issue 新评论", issue_comment_card(&comment)),
        ("PR 新建", pr_card(&pr, PrCardStatus::Open, "有一条新 PR 待关注")),
        ("PR 待 Review", pr_card(&pr, PrCardStatus::Open, "您有一条 PR 待 Review")),
        ("PR 已合并", pr_card(&pr, PrCardStatus::Merged, "您跟进的 PR 已合并")),
        ("PR 已关闭", pr_card(&pr, PrCardStatus::Closed, "您跟进的 PR 已关闭")),
        ("绑定卡片", binding_card()),
    ]
}

/// 绑定卡片：输入 GitHub 用户名 + 提交按钮（回传 card.action.trigger）。
pub fn binding_card() -> Value {
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "header": {
            "title": { "tag": "plain_text", "content": "绑定 GitHub 账号" },
            "template": "blue"
        },
        "body": {
            "elements": [
                {
                    "tag": "markdown",
                    "content": "输入你的 GitHub 用户名并绑定到飞书账号；之后指派给你的 PR review 会自动提醒你。"
                },
                {
                    "tag": "form",
                    "name": "bind_form",
                    "elements": [
                        {
                            "tag": "input",
                            "name": "github_username",
                            "label": { "tag": "plain_text", "content": "GitHub 用户名" },
                            "placeholder": { "tag": "plain_text", "content": "例如 octocat" }
                        },
                        {
                            "tag": "button",
                            "text": { "tag": "plain_text", "content": "绑定" },
                            "type": "primary",
                            "form_action_type": "submit",
                            "name": "bind_btn",
                            "behaviors": [
                                { "type": "callback", "value": { "action": "bind_github" } }
                            ]
                        }
                    ]
                }
            ]
        }
    })
}
