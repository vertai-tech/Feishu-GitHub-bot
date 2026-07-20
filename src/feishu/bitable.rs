use super::{ApiEnvelope, FeishuClient, API_BASE};
use serde::Deserialize;
use serde_json::{Map, Value};

/// 一条多维表格记录。
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    pub record_id: String,
    #[serde(default)]
    pub fields: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct SearchResp {
    #[serde(default)]
    items: Vec<Record>,
}

#[derive(Debug, Deserialize)]
struct CreateResp {
    record: Record,
}

impl FeishuClient {
    /// 按某个文本字段精确匹配检索记录。
    pub async fn bitable_search(
        &self,
        app_token: &str,
        table_id: &str,
        field_name: &str,
        value: &str,
    ) -> anyhow::Result<Vec<Record>> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<SearchResp> = self
            .http
            .post(format!(
                "{API_BASE}/bitable/v1/apps/{app_token}/tables/{table_id}/records/search"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "filter": {
                    "conjunction": "and",
                    "conditions": [{
                        "field_name": field_name,
                        "operator": "is",
                        "value": [value],
                    }],
                },
            }))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.into_result()?.items)
    }

    /// 新建一条记录，返回 record_id。
    pub async fn bitable_create(
        &self,
        app_token: &str,
        table_id: &str,
        fields: Value,
    ) -> anyhow::Result<String> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<CreateResp> = self
            .http
            .post(format!(
                "{API_BASE}/bitable/v1/apps/{app_token}/tables/{table_id}/records"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "fields": fields }))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.into_result()?.record.record_id)
    }

    /// 更新一条已存在的记录。
    pub async fn bitable_update(
        &self,
        app_token: &str,
        table_id: &str,
        record_id: &str,
        fields: Value,
    ) -> anyhow::Result<()> {
        let token = self.tenant_token().await?;
        let resp: ApiEnvelope<Value> = self
            .http
            .put(format!(
                "{API_BASE}/bitable/v1/apps/{app_token}/tables/{table_id}/records/{record_id}"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "fields": fields }))
            .send()
            .await?
            .json()
            .await?;
        resp.into_result()?;
        Ok(())
    }
}
