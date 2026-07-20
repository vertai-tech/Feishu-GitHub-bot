use super::{ApiEnvelope, FeishuClient, API_BASE};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct SentMessage {
    pub message_id: String,
}

impl FeishuClient {
    /// 发送一条交互卡片。`receive_id_type` 取 "chat_id"（群）或 "open_id"（私聊）。
    /// 返回 message_id，供后续更新卡片使用。
    pub async fn send_card(
        &self,
        receive_id_type: &str,
        receive_id: &str,
        card: &Value,
    ) -> anyhow::Result<String> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<SentMessage> = self
            .http
            .post(format!("{API_BASE}/im/v1/messages"))
            .query(&[("receive_id_type", receive_id_type)])
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "receive_id": receive_id,
                "msg_type": "interactive",
                // content 需为卡片 JSON 的字符串形式
                "content": serde_json::to_string(card)?,
            }))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.into_result()?.message_id)
    }

    /// 发送纯文本消息。
    pub async fn send_text(
        &self,
        receive_id_type: &str,
        receive_id: &str,
        text: &str,
    ) -> anyhow::Result<String> {
        let token = self.tenant_token().await?;
        let content = serde_json::json!({ "text": text }).to_string();
        let resp: ApiEnvelope<SentMessage> = self
            .http
            .post(format!("{API_BASE}/im/v1/messages"))
            .query(&[("receive_id_type", receive_id_type)])
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "receive_id": receive_id,
                "msg_type": "text",
                "content": content,
            }))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.into_result()?.message_id)
    }

    /// 就地更新一条已发送的交互卡片（PR 状态变化时用）。
    pub async fn patch_card(&self, message_id: &str, card: &Value) -> anyhow::Result<()> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<Value> = self
            .http
            .patch(format!("{API_BASE}/im/v1/messages/{message_id}"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "content": serde_json::to_string(card)?,
            }))
            .send()
            .await?
            .json()
            .await?;
        resp.into_result()?;
        Ok(())
    }
}
