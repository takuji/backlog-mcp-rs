use backlog_api_rs::api::{AddDocumentParams, GetDocumentsParams};
use backlog_api_rs::models::{Document, DocumentComment, DocumentTreeNode};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::attachments::AttachmentOut;
use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchDocumentsParams {
    /// プロジェクトキー(例: "PROJ")。省略時は全プロジェクトを検索
    pub project_key: Option<String>,
    /// 検索キーワード
    pub keyword: Option<String>,
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
}

/// プロジェクトキーのみを取るツール(count_documents / document_tree)のパラメータ。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ProjectKeyParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
}

/// ドキュメントIDのみを取るツール(get_document / list_document_comments)のパラメータ。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct GetDocumentParams {
    /// ドキュメントID(文字列。search_documents や document_tree の結果に含まれる)
    pub document_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CreateDocumentParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// ドキュメントのタイトル
    pub title: String,
    /// 本文(Markdownとして解釈される)
    pub content: String,
    /// タイトル前に表示する絵文字(1文字)
    pub emoji: Option<String>,
    /// 指定すると、そのドキュメントの子として作成される
    pub parent_id: Option<String>,
}

/// 実APIは絵文字なしのノードに `""` を返す。出力ノイズになるので None に落とす。
fn normalize_emoji(emoji: &Option<String>) -> Option<String> {
    emoji
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

/// 一覧向けのドキュメント要約。本文はトークン節約のため含めない。
#[derive(serde::Serialize)]
pub(crate) struct DocumentSummaryOut {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    pub tags: Vec<String>,
    pub created_user: String,
    pub created: String,
    pub updated: Option<String>,
}

impl From<&Document> for DocumentSummaryOut {
    fn from(d: &Document) -> Self {
        Self {
            id: d.id.clone(),
            title: d.title.clone(),
            emoji: normalize_emoji(&d.emoji),
            tags: d.tags.iter().map(|t| t.name.clone()).collect(),
            created_user: d.created_user.name.clone(),
            created: d.created.to_rfc3339(),
            updated: d.updated.map(|t| t.to_rfc3339()),
        }
    }
}

/// get_document 向けの詳細。本文はプレーンテキストのみ(リッチテキスト構造は返さない)。
#[derive(serde::Serialize)]
pub(crate) struct DocumentDetailOut {
    pub id: String,
    pub project_id: u64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    pub plain: Option<String>,
    pub tags: Vec<String>,
    pub attachments: Vec<AttachmentOut>,
    pub created_user: String,
    pub created: String,
    pub updated_user: Option<String>,
    pub updated: Option<String>,
}

impl From<&Document> for DocumentDetailOut {
    fn from(d: &Document) -> Self {
        Self {
            id: d.id.clone(),
            project_id: d.project_id,
            title: d.title.clone(),
            emoji: normalize_emoji(&d.emoji),
            plain: d.plain.clone(),
            tags: d.tags.iter().map(|t| t.name.clone()).collect(),
            attachments: d.attachments.iter().map(AttachmentOut::from).collect(),
            created_user: d.created_user.name.clone(),
            created: d.created.to_rfc3339(),
            updated_user: d.updated_user.as_ref().map(|u| u.name.clone()),
            updated: d.updated.map(|t| t.to_rfc3339()),
        }
    }
}

/// ドキュメントツリーのノード(IDと名前のみ)。
#[derive(serde::Serialize)]
pub(crate) struct DocumentTreeOut {
    pub id: String,
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    pub children: Vec<DocumentTreeOut>,
}

impl From<&DocumentTreeNode> for DocumentTreeOut {
    fn from(n: &DocumentTreeNode) -> Self {
        Self {
            id: n.id.clone(),
            name: n.name.clone(),
            emoji: normalize_emoji(&n.emoji),
            children: n.children.iter().map(DocumentTreeOut::from).collect(),
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct DocumentCommentOut {
    pub id: String,
    pub author: Option<String>,
    pub created: String,
    pub plain: Option<String>,
}

impl From<&DocumentComment> for DocumentCommentOut {
    fn from(c: &DocumentComment) -> Self {
        Self {
            id: c.id.clone(),
            author: c.created_user.as_ref().map(|u| u.name.clone()),
            created: c.created.to_rfc3339(),
            plain: c.plain.clone(),
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct DocumentCountOut {
    pub count: u64,
}

#[tool_router(router = documents_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "Backlogのドキュメントを検索する。keyword はタイトルと本文の両方を対象にした全文検索(ページ名しか検索できない search_wikis とは異なる)。結果は要約(ID・タイトル・タグなど)で本文を含まないため、本文を読むには get_document を使う"
    )]
    pub(crate) async fn search_documents(
        &self,
        Parameters(params): Parameters<SearchDocumentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = GetDocumentsParams::new()
            .offset(0)
            .count(params.count.unwrap_or(20).clamp(1, 100));
        if let Some(key) = &params.project_key {
            match self.resolve_project_id(key).await {
                Ok(id) => api_params = api_params.project_ids([id]),
                Err(e) => return Ok(backlog_error(e)),
            }
        }
        if let Some(keyword) = &params.keyword {
            api_params = api_params.keyword(keyword);
        }
        match self.client.documents().list(api_params).await {
            Ok(docs) => {
                let out: Vec<DocumentSummaryOut> =
                    docs.iter().map(DocumentSummaryOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "プロジェクトのドキュメント数を取得する")]
    pub(crate) async fn count_documents(
        &self,
        Parameters(params): Parameters<ProjectKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.documents().count(&params.project_key).await {
            Ok(count) => json_result(&DocumentCountOut { count }),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "プロジェクトのドキュメントの階層構造(ID・名前のみ)を取得する。全体像の把握や目的のドキュメントを探すのに使う。ゴミ箱の中身は含まない"
    )]
    pub(crate) async fn document_tree(
        &self,
        Parameters(params): Parameters<ProjectKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.documents().tree(&params.project_key).await {
            Ok(tree) => json_result(&DocumentTreeOut::from(&tree.active_tree)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "ドキュメントの詳細を取得する。本文はプレーンテキストで返す(リッチテキストの構造は返さない)"
    )]
    pub(crate) async fn get_document(
        &self,
        Parameters(params): Parameters<GetDocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.documents().get(&params.document_id).await {
            Ok(doc) => json_result(&DocumentDetailOut::from(&doc)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "ドキュメントを新規作成する。content は Markdown として解釈される。ドキュメントの更新APIはBacklogに存在しないため、作成後の変更はできない"
    )]
    pub(crate) async fn create_document(
        &self,
        Parameters(params): Parameters<CreateDocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_id = match self.resolve_project_id(&params.project_key).await {
            Ok(id) => id,
            Err(e) => return Ok(backlog_error(e)),
        };
        let mut api_params = AddDocumentParams::new(project_id)
            .title(&params.title)
            .content(&params.content);
        if let Some(emoji) = &params.emoji {
            api_params = api_params.emoji(emoji);
        }
        if let Some(parent_id) = &params.parent_id {
            api_params = api_params.parent_id(parent_id);
        }
        match self.client.documents().add(api_params).await {
            Ok(doc) => json_result(&DocumentDetailOut::from(&doc)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "ドキュメントのコメント一覧を取得する")]
    pub(crate) async fn list_document_comments(
        &self,
        Parameters(params): Parameters<GetDocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.documents().comments(&params.document_id).await {
            Ok(comments) => {
                let out: Vec<DocumentCommentOut> =
                    comments.iter().map(DocumentCommentOut::from).collect();
                json_result(&out)
            }
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

    #[test]
    fn normalize_emoji_drops_empty_and_whitespace_only() {
        assert_eq!(super::normalize_emoji(&Some(String::new())), None);
        assert_eq!(super::normalize_emoji(&Some("   ".to_string())), None);
        assert_eq!(super::normalize_emoji(&None), None);
        assert_eq!(
            super::normalize_emoji(&Some("📝".to_string())),
            Some("📝".to_string())
        );
    }

    /// `document_json()` の emoji フィールドだけ差し替えたもの。
    /// 実APIは絵文字なしのノードに `""` を返すため、その挙動を再現するのに使う。
    fn document_json_with_emoji(emoji: &str) -> serde_json::Value {
        let mut doc = document_json();
        doc["emoji"] = json!(emoji);
        doc
    }

    fn document_json() -> serde_json::Value {
        json!({
            "id": "01939983409c79d5a06a49859789e38f",
            "projectId": 7,
            "title": "設計メモ",
            "plain": "プレーン本文マーカー",
            "json": "{\"type\":\"doc\",\"richMarker\":\"リッチ構造マーカー\"}",
            "statusId": 1,
            "emoji": "📝",
            "attachments": [
                { "id": 22067, "name": "spec.pdf", "size": 2048,
                  "createdUser": { "id": 1, "userId": "admin", "name": "admin", "roleType": 1 },
                  "created": "2026-07-01T06:10:15Z" }
            ],
            "tags": [{ "id": 3, "name": "設計" }],
            "createdUser": { "id": 1, "userId": "admin", "name": "admin", "roleType": 1 },
            "created": "2026-07-01T06:10:15Z",
            "updatedUser": null,
            "updated": "2026-07-20T08:09:49Z"
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
    async fn search_documents_returns_summaries_without_content() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents"))
            .and(query_param("projectId[]", "7"))
            .and(query_param("count", "20"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([document_json()])))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_documents(Parameters(super::SearchDocumentsParams {
                project_key: Some("TEST1".to_string()),
                keyword: None,
                count: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("設計メモ"), "got: {text}");
        // 一覧は本文を含めない(トークン節約の設計意図)
        assert!(!text.contains("プレーン本文マーカー"), "got: {text}");
        assert!(!text.contains("リッチ構造マーカー"), "got: {text}");
        // 実際の絵文字は保持される
        assert!(text.contains(r#"\"emoji\": \"📝\""#), "got: {text}");
    }

    #[tokio::test]
    async fn search_documents_omits_empty_emoji() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([document_json_with_emoji("")])),
            )
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_documents(Parameters(super::SearchDocumentsParams {
                project_key: None,
                keyword: None,
                count: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 実APIが返す空文字の絵文字は emoji キーごと出力から消える
        assert!(!text.contains("emoji"), "got: {text}");
    }

    #[tokio::test]
    async fn search_documents_maps_api_error_to_tool_error() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "errors": [{ "message": "Authentication failure.", "code": 11 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_documents(Parameters(super::SearchDocumentsParams {
                project_key: None,
                keyword: None,
                count: None,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("Authentication failure."), "got: {text}");
    }

    #[tokio::test]
    async fn count_documents_returns_count() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/count"))
            .and(query_param("projectIdOrKey", "TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 34 })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .count_documents(Parameters(super::ProjectKeyParams {
                project_key: "TEST1".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("34"), "got: {text}");
    }

    #[tokio::test]
    async fn document_tree_returns_active_tree() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/tree"))
            .and(query_param("projectIdOrKey", "TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "projectId": 7,
                "activeTree": {
                    "id": "Active",
                    "children": [
                        { "id": "doc-child-1", "name": "子ページ", "emoji": "📁", "children": [] }
                    ]
                },
                "trashTree": {
                    "id": "Trash",
                    "children": [
                        { "id": "doc-trashed", "name": "ゴミ箱の中身マーカー", "children": [] }
                    ]
                }
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .document_tree(Parameters(super::ProjectKeyParams {
                project_key: "TEST1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("子ページ"), "got: {text}");
        assert!(text.contains("doc-child-1"), "got: {text}");
        // ゴミ箱は返さない
        assert!(!text.contains("ゴミ箱の中身マーカー"), "got: {text}");
        // 実際の絵文字は保持される(出力はpretty-printされ、
        // CallToolResult全体をシリアライズする際にさらにエスケープされる)
        assert!(text.contains(r#"\"emoji\": \"📁\""#), "got: {text}");
    }

    #[tokio::test]
    async fn document_tree_omits_empty_emoji() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/tree"))
            .and(query_param("projectIdOrKey", "TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "projectId": 7,
                "activeTree": {
                    "id": "Active",
                    "children": [
                        {
                            "id": "doc-no-emoji",
                            "name": "絵文字なしノード",
                            "emoji": "",
                            "children": []
                        },
                        {
                            "id": "doc-with-emoji",
                            "name": "絵文字ありノード",
                            "emoji": "📝",
                            "children": []
                        }
                    ]
                }
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .document_tree(Parameters(super::ProjectKeyParams {
                project_key: "TEST1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 空文字の絵文字を持つノードでは emoji キー自体が出力されない
        // (キーが1回だけ、実絵文字を持つノードの分だけ出現することを確認する)
        assert_eq!(text.matches(r#"\"emoji\""#).count(), 1, "got: {text}");
        // emoji が空文字のまま出力されることは決してない
        assert!(!text.contains(r#"\"emoji\": \"\""#), "got: {text}");
        // 実際の絵文字を持つノードは emoji キーが残る
        assert!(text.contains(r#"\"emoji\": \"📝\""#), "got: {text}");
        assert!(text.contains("絵文字なしノード"), "got: {text}");
        assert!(text.contains("絵文字ありノード"), "got: {text}");
    }

    #[tokio::test]
    async fn get_document_returns_plain_but_not_json() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/doc-abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document_json()))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_document(Parameters(super::GetDocumentParams {
                document_id: "doc-abc123".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("プレーン本文マーカー"), "got: {text}");
        // リッチテキストの構造は返さない
        assert!(!text.contains("リッチ構造マーカー"), "got: {text}");
        // 実際の絵文字は保持される
        assert!(text.contains(r#"\"emoji\": \"📝\""#), "got: {text}");
        assert!(text.contains("22067"), "got: {text}");
        assert!(!text.contains("attachment_count"), "got: {text}");
    }

    #[tokio::test]
    async fn get_document_omits_empty_emoji() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/doc-abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document_json_with_emoji("")))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_document(Parameters(super::GetDocumentParams {
                document_id: "doc-abc123".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        // 実APIが返す空文字の絵文字は emoji キーごと出力から消える
        assert!(!text.contains("emoji"), "got: {text}");
    }

    #[tokio::test]
    async fn create_document_resolves_project_key_and_posts_markdown() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/documents"))
            .and(body_string_contains("projectId=7"))
            .and(body_string_contains("title="))
            .respond_with(ResponseTemplate::new(201).set_body_json(document_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .create_document(Parameters(super::CreateDocumentParams {
                project_key: "TEST1".to_string(),
                title: "設計メモ".to_string(),
                content: "# 見出し".to_string(),
                emoji: None,
                parent_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("設計メモ"), "got: {text}");
    }

    #[tokio::test]
    async fn list_document_comments_returns_plain_comments() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/doc-abc123/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "cmt-1", "documentId": "doc-abc123",
                "content": "リッチコメントマーカー", "plain": "プレーンコメントマーカー",
                "created": "2026-07-21T01:00:00Z", "updated": null,
                "createdUser": { "id": 2, "userId": "eguchi", "name": "eguchi", "roleType": 2 }
            }])))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_document_comments(Parameters(super::GetDocumentParams {
                document_id: "doc-abc123".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("プレーンコメントマーカー"), "got: {text}");
        assert!(!text.contains("リッチコメントマーカー"), "got: {text}");
        assert!(text.contains("eguchi"), "got: {text}");
    }
}
