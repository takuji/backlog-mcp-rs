use backlog_api_rs::api::{AddWikiParams, GetWikisParams, UpdateWikiParams};
use backlog_api_rs::models::Wiki;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::attachments::AttachmentOut;
use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchWikisParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// ページ名の部分一致フィルタ(本文は検索できない)
    pub keyword: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CountWikisParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct GetWikiParams {
    /// WikiページのID(search_wikis の結果に含まれる)
    pub wiki_id: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CreateWikiParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// ページ名
    pub name: String,
    /// ページ本文
    pub content: String,
    /// プロジェクトメンバーにメール通知するか(省略時false)
    pub mail_notify: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct UpdateWikiToolParams {
    /// WikiページのID
    pub wiki_id: u64,
    /// 新しいページ名(変更する場合のみ)
    pub name: Option<String>,
    /// 新しい本文(変更する場合のみ)
    pub content: Option<String>,
    /// プロジェクトメンバーにメール通知するか(省略時false)
    pub mail_notify: Option<bool>,
}

/// 一覧向けのWiki要約。本文はトークン節約のため含めない。
#[derive(serde::Serialize)]
pub(crate) struct WikiSummaryOut {
    pub id: u64,
    pub name: String,
    pub tags: Vec<String>,
    pub created_user: String,
    pub created: String,
    pub updated: Option<String>,
}

impl From<&Wiki> for WikiSummaryOut {
    fn from(w: &Wiki) -> Self {
        Self {
            id: w.id,
            name: w.name.clone(),
            tags: w.tags.iter().map(|t| t.name.clone()).collect(),
            created_user: w.created_user.name.clone(),
            created: w.created.to_rfc3339(),
            updated: w.updated.map(|t| t.to_rfc3339()),
        }
    }
}

/// get_wiki 向けの詳細。本文を全文含む。
#[derive(serde::Serialize)]
pub(crate) struct WikiDetailOut {
    pub id: u64,
    pub project_id: u64,
    pub name: String,
    pub content: Option<String>,
    pub tags: Vec<String>,
    pub attachments: Vec<AttachmentOut>,
    pub created_user: String,
    pub created: String,
    pub updated_user: Option<String>,
    pub updated: Option<String>,
}

impl From<&Wiki> for WikiDetailOut {
    fn from(w: &Wiki) -> Self {
        Self {
            id: w.id,
            project_id: w.project_id,
            name: w.name.clone(),
            content: w.content.clone(),
            tags: w.tags.iter().map(|t| t.name.clone()).collect(),
            attachments: w.attachments.iter().map(AttachmentOut::from).collect(),
            created_user: w.created_user.name.clone(),
            created: w.created.to_rfc3339(),
            updated_user: w.updated_user.as_ref().map(|u| u.name.clone()),
            updated: w.updated.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct WikiCountOut {
    pub count: u64,
}

#[tool_router(router = wikis_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "プロジェクトのWikiページを検索する。keyword はページ名の部分一致で、本文は検索できない。本文を探す場合は一覧で候補を絞ってから get_wiki で読むこと。結果は要約(本文を含まない)"
    )]
    pub(crate) async fn search_wikis(
        &self,
        Parameters(params): Parameters<SearchWikisParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = GetWikisParams::new(&params.project_key);
        if let Some(keyword) = &params.keyword {
            api_params = api_params.keyword(keyword);
        }
        match self.client.wikis().list(api_params).await {
            Ok(wikis) => {
                let out: Vec<WikiSummaryOut> = wikis.iter().map(WikiSummaryOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "プロジェクトのWikiページ数を取得する")]
    pub(crate) async fn count_wikis(
        &self,
        Parameters(params): Parameters<CountWikisParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.wikis().count(&params.project_key).await {
            Ok(count) => json_result(&WikiCountOut { count }),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "Wikiページの詳細(本文を含む)を取得する")]
    pub(crate) async fn get_wiki(
        &self,
        Parameters(params): Parameters<GetWikiParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.wikis().get(params.wiki_id).await {
            Ok(wiki) => json_result(&WikiDetailOut::from(&wiki)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "Wikiページを新規作成する")]
    pub(crate) async fn create_wiki(
        &self,
        Parameters(params): Parameters<CreateWikiParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_id = match self.resolve_project_id(&params.project_key).await {
            Ok(id) => id,
            Err(e) => return Ok(backlog_error(e)),
        };
        let api_params = AddWikiParams::new(project_id, &params.name, &params.content)
            .mail_notify(params.mail_notify.unwrap_or(false));
        match self.client.wikis().add(api_params).await {
            Ok(wiki) => json_result(&WikiDetailOut::from(&wiki)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "Wikiページを更新する。指定したフィールドのみ変更される")]
    pub(crate) async fn update_wiki(
        &self,
        Parameters(params): Parameters<UpdateWikiToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = UpdateWikiParams::new();
        if let Some(name) = &params.name {
            api_params = api_params.name(name);
        }
        if let Some(content) = &params.content {
            api_params = api_params.content(content);
        }
        api_params = api_params.mail_notify(params.mail_notify.unwrap_or(false));
        match self.client.wikis().update(params.wiki_id, api_params).await {
            Ok(wiki) => json_result(&WikiDetailOut::from(&wiki)),
            Err(e) => Ok(backlog_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::BacklogMcpConfig;
    use crate::server::BacklogMcpServer;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_server(backlog: &MockServer) -> BacklogMcpServer {
        BacklogMcpServer::new(&BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
    }

    fn wiki_json() -> serde_json::Value {
        json!({
            "id": 1, "projectId": 7, "name": "Home",
            "content": "秘密の本文マーカー",
            "tags": [{ "id": 12, "name": "議事録" }],
            "attachments": [{ "id": 5, "name": "spec.pdf", "size": 1024 }],
            "sharedFiles": [], "stars": [],
            "createdUser": { "id": 1, "userId": "admin", "name": "admin", "roleType": 1 },
            "created": "2026-07-01T06:10:15Z",
            "updatedUser": null, "updated": "2026-07-20T08:09:49Z"
        })
    }

    fn project_json() -> serde_json::Value {
        json!({
            "id": 7, "projectKey": "TEST1", "name": "テスト",
            "chartEnabled": true, "subtaskingEnabled": false,
            "projectLeaderCanEditProjectLeader": false,
            "useWiki": true, "useFileSharing": true, "useWikiTreeView": true,
            "useOriginalImageSizeAtWiki": false,
            "textFormattingRule": "markdown", "archived": false,
            "displayOrder": 0, "useDevAttributes": true
        })
    }

    #[tokio::test]
    async fn search_wikis_returns_summaries_without_content() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/wikis"))
            .and(query_param("projectIdOrKey", "TEST1"))
            .and(query_param("keyword", "議事"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([wiki_json()])))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_wikis(Parameters(super::SearchWikisParams {
                project_key: "TEST1".to_string(),
                keyword: Some("議事".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("Home"), "got: {text}");
        assert!(text.contains("議事録"), "got: {text}");
        // トークン節約のため一覧に本文を含めない(設計意図の回帰防止)
        assert!(!text.contains("秘密の本文マーカー"), "got: {text}");
    }

    #[tokio::test]
    async fn search_wikis_maps_api_error_to_tool_error() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/wikis"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errors": [{ "message": "No project.", "code": 6 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_wikis(Parameters(super::SearchWikisParams {
                project_key: "NOPE".to_string(),
                keyword: None,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("No project."), "got: {text}");
    }

    #[tokio::test]
    async fn count_wikis_returns_count() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/wikis/count"))
            .and(query_param("projectIdOrKey", "TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 12 })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .count_wikis(Parameters(super::CountWikisParams {
                project_key: "TEST1".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("12"), "got: {text}");
    }

    #[tokio::test]
    async fn get_wiki_returns_full_content() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/wikis/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(wiki_json()))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_wiki(Parameters(super::GetWikiParams { wiki_id: 1 }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("秘密の本文マーカー"), "got: {text}");
        assert!(text.contains("議事録"), "got: {text}");
        // 添付のIDが取れること(ダウンロードツールを呼ぶために必要)
        assert!(text.contains("\\\"id\\\": \\\"5\\\""), "got: {text}");
        assert!(text.contains("spec.pdf"), "got: {text}");
        assert!(!text.contains("attachment_count"), "got: {text}");
    }

    #[tokio::test]
    async fn create_wiki_resolves_project_key_and_posts() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/wikis"))
            .and(body_string_contains("projectId=7"))
            .and(body_string_contains("mailNotify=false"))
            .respond_with(ResponseTemplate::new(201).set_body_json(wiki_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .create_wiki(Parameters(super::CreateWikiParams {
                project_key: "TEST1".to_string(),
                name: "Home".to_string(),
                content: "秘密の本文マーカー".to_string(),
                mail_notify: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("Home"), "got: {text}");
    }

    #[tokio::test]
    async fn update_wiki_patches_only_given_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v2/wikis/1"))
            .and(body_string_contains("name="))
            .respond_with(ResponseTemplate::new(200).set_body_json(wiki_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .update_wiki(Parameters(super::UpdateWikiToolParams {
                wiki_id: 1,
                name: Some("改題".to_string()),
                content: None,
                mail_notify: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }
}
