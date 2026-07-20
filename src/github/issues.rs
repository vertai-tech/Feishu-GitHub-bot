//! 解析 GitHub `issues` 与 `issue_comment` webhook payload（仅用于单向通知）。

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct IssuesPayload {
    pub action: String,
    pub issue: Issue,
    pub repository: Repository,
}

#[derive(Debug, Deserialize)]
pub struct IssueCommentPayload {
    pub action: String,
    pub issue: Issue,
    pub comment: Comment,
    pub repository: Repository,
}

#[derive(Debug, Deserialize)]
pub struct Issue {
    pub html_url: String,
    pub number: u64,
    pub title: String,
    pub user: User,
    /// 该 "issue" 其实是 PR 时才有此字段（issue_comment 事件对 PR 也会触发）
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

impl Issue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub html_url: String,
    pub body: String,
    pub user: User,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub login: String,
}

/// Issue 公共信息，供卡片渲染。
#[derive(Debug, Clone)]
pub struct IssueInfo {
    pub repo_full_name: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
}

/// `issues` 事件归一化。
#[derive(Debug, Clone)]
pub enum IssueEvent {
    Opened(IssueInfo),
    Closed(IssueInfo),
    Reopened(IssueInfo),
    Ignored,
}

impl IssuesPayload {
    fn info(&self) -> IssueInfo {
        IssueInfo {
            repo_full_name: self.repository.full_name.clone(),
            number: self.issue.number,
            title: self.issue.title.clone(),
            url: self.issue.html_url.clone(),
            author: self.issue.user.login.clone(),
        }
    }

    pub fn classify(&self) -> IssueEvent {
        match self.action.as_str() {
            "opened" => IssueEvent::Opened(self.info()),
            "closed" => IssueEvent::Closed(self.info()),
            "reopened" => IssueEvent::Reopened(self.info()),
            _ => IssueEvent::Ignored,
        }
    }
}

/// 一条新评论的信息。
#[derive(Debug, Clone)]
pub struct CommentInfo {
    pub repo_full_name: String,
    pub issue_number: u64,
    pub issue_title: String,
    pub commenter: String,
    pub body: String,
    pub comment_url: String,
}

impl IssueCommentPayload {
    /// 仅当 action==created 且评论对象是纯 Issue（非 PR）时返回 Some。
    pub fn as_created_issue_comment(&self) -> Option<CommentInfo> {
        if self.action != "created" || self.issue.is_pull_request() {
            return None;
        }
        Some(CommentInfo {
            repo_full_name: self.repository.full_name.clone(),
            issue_number: self.issue.number,
            issue_title: self.issue.title.clone(),
            commenter: self.comment.user.login.clone(),
            body: self.comment.body.clone(),
            comment_url: self.comment.html_url.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_issue_opened() {
        let json = serde_json::json!({
            "action": "opened",
            "issue": {"html_url":"u","number":3,"title":"Bug","user":{"login":"alice"}},
            "repository": {"full_name":"o/r"}
        });
        let p: IssuesPayload = serde_json::from_value(json).unwrap();
        match p.classify() {
            IssueEvent::Opened(i) => { assert_eq!(i.number, 3); assert_eq!(i.author, "alice"); }
            other => panic!("期望 Opened，得到 {other:?}"),
        }
    }

    #[test]
    fn issue_comment_created_on_issue() {
        let json = serde_json::json!({
            "action": "created",
            "issue": {"html_url":"iu","number":5,"title":"T","user":{"login":"a"}},
            "comment": {"html_url":"cu","body":"hi","user":{"login":"bob"}},
            "repository": {"full_name":"o/r"}
        });
        let p: IssueCommentPayload = serde_json::from_value(json).unwrap();
        let c = p.as_created_issue_comment().expect("应识别为 issue 评论");
        assert_eq!(c.commenter, "bob");
        assert_eq!(c.issue_number, 5);
    }

    #[test]
    fn issue_comment_on_pr_skipped() {
        let json = serde_json::json!({
            "action": "created",
            "issue": {"html_url":"iu","number":6,"title":"T","user":{"login":"a"},"pull_request":{"url":"x"}},
            "comment": {"html_url":"cu","body":"hi","user":{"login":"bob"}},
            "repository": {"full_name":"o/r"}
        });
        let p: IssueCommentPayload = serde_json::from_value(json).unwrap();
        assert!(p.as_created_issue_comment().is_none());
    }
}
