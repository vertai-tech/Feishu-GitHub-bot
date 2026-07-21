//! 解析 GitHub `issues` 与 `issue_comment` webhook payload（仅用于单向通知）。

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct IssuesPayload {
    pub action: String,
    pub issue: Issue,
    pub repository: Repository,
    /// assigned 事件中被指派的受理人
    #[serde(default)]
    pub assignee: Option<User>,
    /// 触发事件的操作者
    #[serde(default)]
    pub sender: Option<User>,
    /// edited 事件的变更详情（含 body/title）
    #[serde(default)]
    pub changes: Option<serde_json::Value>,
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
    /// 当前受理人列表
    #[serde(default)]
    pub assignees: Vec<User>,
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
    /// 当前受理人 GitHub 登录名列表
    pub assignees: Vec<String>,
}

/// `issues` 事件归一化。
#[derive(Debug, Clone)]
pub enum IssueEvent {
    Opened(IssueInfo),
    Closed(IssueInfo),
    Reopened(IssueInfo),
    Assigned {
        issue: IssueInfo,
        assignee_login: String,
    },
    Unassigned {
        issue: IssueInfo,
        assignee_login: String,
    },
    Edited {
        issue: IssueInfo,
        editor_login: String,
    },
    /// 被删除 / 转移 → 停止跟踪
    Removed(IssueInfo),
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
            assignees: self.issue.assignees.iter().map(|u| u.login.clone()).collect(),
        }
    }

    pub fn classify(&self) -> IssueEvent {
        match self.action.as_str() {
            "opened" => IssueEvent::Opened(self.info()),
            "closed" => IssueEvent::Closed(self.info()),
            "deleted" | "transferred" => IssueEvent::Removed(self.info()),
            "reopened" => IssueEvent::Reopened(self.info()),
            "assigned" => match &self.assignee {
                Some(a) => IssueEvent::Assigned {
                    issue: self.info(),
                    assignee_login: a.login.clone(),
                },
                None => IssueEvent::Ignored,
            },
            "unassigned" => match &self.assignee {
                Some(a) => IssueEvent::Unassigned {
                    issue: self.info(),
                    assignee_login: a.login.clone(),
                },
                None => IssueEvent::Ignored,
            },
            "edited" if crate::github::events::body_changed(&self.changes) => match &self.sender {
                Some(s) => IssueEvent::Edited {
                    issue: self.info(),
                    editor_login: s.login.clone(),
                },
                None => IssueEvent::Ignored,
            },
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
    /// 该 Issue 的受理人（Issue 评论按此路由）
    pub assignees: Vec<String>,
    /// 评论对象是否为 PR
    pub is_pr: bool,
    /// Issue/PR 的创建者（PR 评论按此通知作者）
    pub author: String,
}

impl IssueCommentPayload {
    /// 仅当 action==created 时返回评论信息（Issue 与 PR 都返回，用 is_pr 区分）。
    pub fn as_created_comment(&self) -> Option<CommentInfo> {
        if self.action != "created" {
            return None;
        }
        Some(CommentInfo {
            repo_full_name: self.repository.full_name.clone(),
            issue_number: self.issue.number,
            issue_title: self.issue.title.clone(),
            commenter: self.comment.user.login.clone(),
            body: self.comment.body.clone(),
            comment_url: self.comment.html_url.clone(),
            assignees: self.issue.assignees.iter().map(|u| u.login.clone()).collect(),
            is_pr: self.issue.is_pull_request(),
            author: self.issue.user.login.clone(),
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
        let c = p.as_created_comment().expect("应识别为评论");
        assert_eq!(c.commenter, "bob");
        assert_eq!(c.issue_number, 5);
        assert!(!c.is_pr);
        assert_eq!(c.author, "a");
    }

    #[test]
    fn comment_on_pr_marked_is_pr() {
        let json = serde_json::json!({
            "action": "created",
            "issue": {"html_url":"iu","number":6,"title":"T","user":{"login":"prauthor"},"pull_request":{"url":"x"}},
            "comment": {"html_url":"cu","body":"hi","user":{"login":"bob"}},
            "repository": {"full_name":"o/r"}
        });
        let p: IssueCommentPayload = serde_json::from_value(json).unwrap();
        let c = p.as_created_comment().expect("PR 评论也应返回");
        assert!(c.is_pr);
        assert_eq!(c.author, "prauthor");
    }
}
