use super::{ApiEnvelope, FeishuClient, API_BASE};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CreatedTask {
    pub task: TaskInfo,
}

#[derive(Debug, Deserialize)]
pub struct TaskInfo {
    pub guid: String,
}

impl FeishuClient {
    /// 创建一个飞书任务并把 `assignee_open_id` 设为负责人。返回 task guid。
    pub async fn create_task(
        &self,
        summary: &str,
        description: &str,
        assignee_open_id: &str,
    ) -> anyhow::Result<String> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<CreatedTask> = self
            .http
            .post(format!("{API_BASE}/task/v2/tasks"))
            .query(&[("user_id_type", "open_id")])
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "summary": summary,
                "description": description,
                "members": [{
                    "id": assignee_open_id,
                    "type": "user",
                    "role": "assignee",
                }],
            }))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.into_result()?.task.guid)
    }

    /// 把任务标记为完成（PR 合并/关闭时收尾）。
    pub async fn complete_task(&self, task_guid: &str) -> anyhow::Result<()> {
        let token = self.tenant_token().await?;
        let now_ms = chrono::Utc::now().timestamp_millis().to_string();
        let resp: ApiEnvelope<serde_json::Value> = self
            .http
            .patch(format!("{API_BASE}/task/v2/tasks/{task_guid}"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "task": { "completed_at": now_ms },
                "update_fields": ["completed_at"],
            }))
            .send()
            .await?
            .json()
            .await?;
        resp.into_result()?;
        Ok(())
    }
}
