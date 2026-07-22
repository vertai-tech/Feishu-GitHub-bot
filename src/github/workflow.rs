//! 解析 GitHub `workflow_run` webhook payload（GitHub Actions CI/CD 运行完成）。

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct WorkflowRunPayload {
    pub action: String,
    pub workflow_run: WorkflowRun,
    pub repository: Repository,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowRun {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub head_branch: String,
    pub html_url: String,
    /// success / failure / cancelled / timed_out / ...（action==completed 时才有）
    #[serde(default)]
    pub conclusion: Option<String>,
    /// 发起本次运行的人（push/merge 触发者）
    #[serde(default)]
    pub triggering_actor: Option<User>,
    #[serde(default)]
    pub actor: Option<User>,
    #[serde(default)]
    pub head_commit: Option<HeadCommit>,
}

#[derive(Debug, Deserialize)]
pub struct HeadCommit {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub login: String,
}

/// CI 运行公共信息，供卡片渲染与路由。
#[derive(Debug, Clone)]
pub struct CiInfo {
    pub repo_full_name: String,
    pub workflow_name: String,
    pub branch: String,
    pub url: String,
    pub conclusion: String,
    /// 触发者 GitHub 登录名（用于私聊）
    pub triggerer: String,
    /// head commit 首行标题（可空）
    pub commit_title: String,
}

/// 归一化后的 CI 事件。
#[derive(Debug, Clone)]
pub enum CiEvent {
    Success(CiInfo),
    Failure(CiInfo),
    Ignored,
}

impl WorkflowRunPayload {
    fn info(&self, conclusion: &str) -> CiInfo {
        let run = &self.workflow_run;
        let triggerer = run
            .triggering_actor
            .as_ref()
            .or(run.actor.as_ref())
            .map(|u| u.login.clone())
            .unwrap_or_default();
        let commit_title = run
            .head_commit
            .as_ref()
            .map(|c| c.message.lines().next().unwrap_or("").to_string())
            .unwrap_or_default();
        CiInfo {
            repo_full_name: self.repository.full_name.clone(),
            workflow_name: run.name.clone().unwrap_or_else(|| "CI/CD".to_string()),
            branch: run.head_branch.clone(),
            url: run.html_url.clone(),
            conclusion: conclusion.to_string(),
            triggerer,
            commit_title,
        }
    }

    /// 仅 action==completed 时归一化：success→成功；failure/timed_out→失败；其余（取消/跳过等）忽略。
    pub fn classify(&self) -> CiEvent {
        if self.action != "completed" {
            return CiEvent::Ignored;
        }
        match self.workflow_run.conclusion.as_deref() {
            Some("success") => CiEvent::Success(self.info("success")),
            Some(c @ ("failure" | "timed_out")) => CiEvent::Failure(self.info(c)),
            _ => CiEvent::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(action: &str, conclusion: Option<&str>, repo: &str) -> WorkflowRunPayload {
        let json = serde_json::json!({
            "action": action,
            "workflow_run": {
                "name": "CI",
                "head_branch": "main",
                "html_url": "https://github.com/o/r/actions/runs/1",
                "conclusion": conclusion,
                "triggering_actor": {"login": "pusher1"},
                "head_commit": {"message": "fix: bug\n\ndetails"}
            },
            "repository": {"full_name": repo}
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn success_classified() {
        match payload("completed", Some("success"), "o/fantap-mobile").classify() {
            CiEvent::Success(ci) => {
                assert_eq!(ci.triggerer, "pusher1");
                assert_eq!(ci.commit_title, "fix: bug");
            }
            other => panic!("期望 Success，得到 {other:?}"),
        }
    }

    #[test]
    fn failure_classified() {
        assert!(matches!(
            payload("completed", Some("failure"), "o/r").classify(),
            CiEvent::Failure(_)
        ));
        assert!(matches!(
            payload("completed", Some("timed_out"), "o/r").classify(),
            CiEvent::Failure(_)
        ));
    }

    #[test]
    fn non_completed_or_cancelled_ignored() {
        assert!(matches!(
            payload("requested", None, "o/r").classify(),
            CiEvent::Ignored
        ));
        assert!(matches!(
            payload("completed", Some("cancelled"), "o/r").classify(),
            CiEvent::Ignored
        ));
    }
}
