//! 解析 GitHub `pull_request` webhook payload。

use serde::Deserialize;

/// 原始 payload（只取我们关心的字段）。
#[derive(Debug, Deserialize)]
pub struct PullRequestPayload {
    pub action: String,
    pub number: u64,
    pub pull_request: PullRequest,
    pub repository: Repository,
    /// review_requested 事件中被请求的 reviewer
    #[serde(default)]
    pub requested_reviewer: Option<User>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub html_url: String,
    pub title: String,
    pub user: User,
    pub base: GitRef,
    pub head: GitRef,
    #[serde(default)]
    pub merged: bool,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub git_ref: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub login: String,
}

/// PR 的公共信息，供编排层生成卡片/任务。
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub repo_full_name: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub base_ref: String,
    pub head_ref: String,
    pub merged: bool,
}

/// 归一化后的事件语义。
#[derive(Debug, Clone)]
pub enum PrEvent {
    /// 新开 PR
    Opened(PrInfo),
    /// 请求某人 review
    ReviewRequested { pr: PrInfo, reviewer_login: String },
    /// PR 关闭（merged 区分是否已合并）
    Closed(PrInfo),
    /// 其它 action，忽略
    Ignored,
}

impl PullRequestPayload {
    fn pr_info(&self) -> PrInfo {
        PrInfo {
            repo_full_name: self.repository.full_name.clone(),
            number: self.number,
            title: self.pull_request.title.clone(),
            url: self.pull_request.html_url.clone(),
            author: self.pull_request.user.login.clone(),
            base_ref: self.pull_request.base.git_ref.clone(),
            head_ref: self.pull_request.head.git_ref.clone(),
            merged: self.pull_request.merged,
        }
    }

    /// 把原始 payload 归一化为 PrEvent。
    pub fn classify(&self) -> PrEvent {
        match self.action.as_str() {
            "opened" => PrEvent::Opened(self.pr_info()),
            "review_requested" => match &self.requested_reviewer {
                Some(r) => PrEvent::ReviewRequested {
                    pr: self.pr_info(),
                    reviewer_login: r.login.clone(),
                },
                // 请求的是 team 而非个人时无 requested_reviewer，忽略
                None => PrEvent::Ignored,
            },
            "closed" => PrEvent::Closed(self.pr_info()),
            _ => PrEvent::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_review_requested() {
        let json = serde_json::json!({
            "action": "review_requested",
            "number": 7,
            "pull_request": {
                "html_url": "https://github.com/o/r/pull/7",
                "title": "Add feature",
                "user": {"login": "author1"},
                "base": {"ref": "main"},
                "head": {"ref": "feat"},
                "merged": false
            },
            "repository": {"full_name": "o/r", "name": "r"},
            "requested_reviewer": {"login": "reviewer1"}
        });
        let payload: PullRequestPayload = serde_json::from_value(json).unwrap();
        match payload.classify() {
            PrEvent::ReviewRequested { pr, reviewer_login } => {
                assert_eq!(reviewer_login, "reviewer1");
                assert_eq!(pr.number, 7);
                assert_eq!(pr.repo_full_name, "o/r");
            }
            other => panic!("期望 ReviewRequested，得到 {other:?}"),
        }
    }

    #[test]
    fn classify_closed_merged() {
        let json = serde_json::json!({
            "action": "closed",
            "number": 8,
            "pull_request": {
                "html_url": "u", "title": "t",
                "user": {"login": "a"},
                "base": {"ref": "main"}, "head": {"ref": "f"},
                "merged": true
            },
            "repository": {"full_name": "o/r", "name": "r"}
        });
        let payload: PullRequestPayload = serde_json::from_value(json).unwrap();
        match payload.classify() {
            PrEvent::Closed(pr) => assert!(pr.merged),
            other => panic!("期望 Closed，得到 {other:?}"),
        }
    }

    #[test]
    fn classify_team_review_request_ignored() {
        let json = serde_json::json!({
            "action": "review_requested",
            "number": 9,
            "pull_request": {
                "html_url": "u", "title": "t",
                "user": {"login": "a"},
                "base": {"ref": "main"}, "head": {"ref": "f"}
            },
            "repository": {"full_name": "o/r", "name": "r"}
        });
        let payload: PullRequestPayload = serde_json::from_value(json).unwrap();
        assert!(matches!(payload.classify(), PrEvent::Ignored));
    }
}
