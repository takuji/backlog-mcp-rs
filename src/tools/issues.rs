use backlog_api_rs::api::{
    AddCommentParams, AddIssueParams, GetCommentsParams as ApiCommentsParams, GetIssuesParams,
    Order, UpdateIssueParams,
};
use backlog_api_rs::models::{Comment, Issue};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::attachments::AttachmentOut;
use super::{backlog_error, json_result, parse_date};
use crate::server::BacklogMcpServer;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchIssuesParams {
    /// プロジェクトキー(例: "PROJ")。省略時は全プロジェクトを検索
    pub project_key: Option<String>,
    /// 検索キーワード
    pub keyword: Option<String>,
    /// 状態IDで絞り込み(get_project で取得できる)
    pub status_ids: Option<Vec<u64>>,
    /// 担当者IDで絞り込み(list_users で取得できる)
    pub assignee_ids: Option<Vec<u64>>,
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct GetIssueParams {
    /// 課題キー(例: "PROJ-123")
    pub issue_key: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListCommentsParams {
    /// 課題キー(例: "PROJ-123")
    pub issue_key: String,
    /// 取得件数(1-100、省略時20)。新しい順に返す
    pub count: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CreateIssueParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// 課題の件名
    pub summary: String,
    /// 課題種別ID(get_project で取得できる)
    pub issue_type_id: u64,
    /// 優先度ID(2=高, 3=中, 4=低。省略時3)
    pub priority_id: Option<u64>,
    /// 課題の説明
    pub description: Option<String>,
    /// 担当者のユーザーID(list_users で取得できる)
    pub assignee_id: Option<u64>,
    /// 期限日(YYYY-MM-DD)
    pub due_date: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct UpdateIssueToolParams {
    /// 課題キー(例: "PROJ-123")
    pub issue_key: String,
    /// 件名(変更する場合のみ)
    pub summary: Option<String>,
    /// 説明(変更する場合のみ)
    pub description: Option<String>,
    /// 状態ID(get_project で取得できる)
    pub status_id: Option<u64>,
    /// 担当者のユーザーID
    pub assignee_id: Option<u64>,
    /// 優先度ID(2=高, 3=中, 4=低)
    pub priority_id: Option<u64>,
    /// 期限日(YYYY-MM-DD)
    pub due_date: Option<String>,
    /// 更新と同時に投稿するコメント
    pub comment: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddCommentToolParams {
    /// 課題キー(例: "PROJ-123")
    pub issue_key: String,
    /// コメント本文
    pub content: String,
}

/// 一覧向けの課題要約。
#[derive(serde::Serialize)]
pub(crate) struct IssueSummaryOut {
    pub id: u64,
    pub issue_key: String,
    pub summary: String,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub due_date: Option<String>,
    pub updated: Option<String>,
}

impl From<&Issue> for IssueSummaryOut {
    fn from(i: &Issue) -> Self {
        Self {
            id: i.id,
            issue_key: i.issue_key.clone(),
            summary: i.summary.clone(),
            issue_type: i.issue_type.name.clone(),
            status: i.status.name.clone(),
            assignee: i.assignee.as_ref().map(|u| u.name.clone()),
            priority: i.priority.as_ref().map(|p| p.name.clone()),
            due_date: i.due_date.map(|d| d.format("%Y-%m-%d").to_string()),
            updated: i.updated.map(|t| t.to_rfc3339()),
        }
    }
}

/// get_issue 向けの詳細情報。
#[derive(serde::Serialize)]
pub(crate) struct IssueDetailOut {
    pub id: u64,
    pub issue_key: String,
    pub summary: String,
    pub description: Option<String>,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub categories: Vec<String>,
    pub attachments: Vec<AttachmentOut>,
    pub milestones: Vec<String>,
    pub parent_issue_id: Option<u64>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub estimated_hours: Option<f64>,
    pub actual_hours: Option<f64>,
    pub created_user: String,
    pub created: String,
    pub updated: Option<String>,
}

impl From<&Issue> for IssueDetailOut {
    fn from(i: &Issue) -> Self {
        Self {
            id: i.id,
            issue_key: i.issue_key.clone(),
            summary: i.summary.clone(),
            description: i.description.clone(),
            issue_type: i.issue_type.name.clone(),
            status: i.status.name.clone(),
            assignee: i.assignee.as_ref().map(|u| u.name.clone()),
            priority: i.priority.as_ref().map(|p| p.name.clone()),
            categories: i.category.iter().map(|c| c.name.clone()).collect(),
            attachments: i.attachments.iter().map(AttachmentOut::from).collect(),
            milestones: i.milestone.iter().map(|m| m.name.clone()).collect(),
            parent_issue_id: i.parent_issue_id,
            start_date: i.start_date.map(|d| d.format("%Y-%m-%d").to_string()),
            due_date: i.due_date.map(|d| d.format("%Y-%m-%d").to_string()),
            estimated_hours: i.estimated_hours,
            actual_hours: i.actual_hours,
            created_user: i.created_user.name.clone(),
            created: i.created.to_rfc3339(),
            updated: i.updated.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct CommentOut {
    pub id: u64,
    pub author: String,
    pub created: String,
    pub content: Option<String>,
}

impl From<&Comment> for CommentOut {
    fn from(c: &Comment) -> Self {
        Self {
            id: c.id,
            author: c.created_user.name.clone(),
            created: c.created.to_rfc3339(),
            content: c.content.clone(),
        }
    }
}

impl BacklogMcpServer {
    /// プロジェクトキーをIDに解決する。
    pub(crate) async fn resolve_project_id(&self, key: &str) -> backlog_api_rs::Result<u64> {
        Ok(self.client.projects().get(key).await?.id)
    }
}

#[tool_router(router = issues_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(description = "Backlogの課題を検索する。結果は要約(キー・件名・状態・担当者など)の一覧")]
    pub(crate) async fn search_issues(
        &self,
        Parameters(params): Parameters<SearchIssuesParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = GetIssuesParams::new().count(params.count.unwrap_or(20).clamp(1, 100));
        if let Some(key) = &params.project_key {
            match self.resolve_project_id(key).await {
                Ok(id) => api_params = api_params.project_ids([id]),
                Err(e) => return Ok(backlog_error(e)),
            }
        }
        if let Some(keyword) = &params.keyword {
            api_params = api_params.keyword(keyword);
        }
        if let Some(ids) = &params.status_ids {
            api_params = api_params.status_ids(ids.iter().copied());
        }
        if let Some(ids) = &params.assignee_ids {
            api_params = api_params.assignee_ids(ids.iter().copied());
        }
        match self.client.issues().list(api_params).await {
            Ok(issues) => {
                let out: Vec<IssueSummaryOut> = issues.iter().map(IssueSummaryOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "課題の詳細(説明を含む)を取得する")]
    pub(crate) async fn get_issue(
        &self,
        Parameters(params): Parameters<GetIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.issues().get(&params.issue_key).await {
            Ok(issue) => json_result(&IssueDetailOut::from(&issue)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "課題のコメント一覧を取得する(新しい順)")]
    pub(crate) async fn list_comments(
        &self,
        Parameters(params): Parameters<ListCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let api_params = ApiCommentsParams::new()
            .count(params.count.unwrap_or(20).clamp(1, 100))
            .order(Order::Desc);
        match self
            .client
            .issues()
            .comments(&params.issue_key, api_params)
            .await
        {
            Ok(comments) => {
                let out: Vec<CommentOut> = comments.iter().map(CommentOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "Backlogに課題を作成する。issue_type_id は get_project で事前に取得すること。優先度は省略時「中」"
    )]
    pub(crate) async fn create_issue(
        &self,
        Parameters(params): Parameters<CreateIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_id = match self.resolve_project_id(&params.project_key).await {
            Ok(id) => id,
            Err(e) => return Ok(backlog_error(e)),
        };
        let mut api_params = AddIssueParams::new(
            project_id,
            &params.summary,
            params.issue_type_id,
            params.priority_id.unwrap_or(3),
        );
        if let Some(description) = &params.description {
            api_params = api_params.description(description);
        }
        if let Some(assignee_id) = params.assignee_id {
            api_params = api_params.assignee_id(assignee_id);
        }
        if let Some(due_date) = &params.due_date {
            match parse_date(due_date) {
                Ok(date) => api_params = api_params.due_date(date),
                Err(result) => return Ok(result),
            }
        }
        match self.client.issues().add(api_params).await {
            Ok(issue) => json_result(&IssueDetailOut::from(&issue)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "課題を更新する。指定したフィールドのみ変更される。状態変更は status_id(get_project で取得)で行う"
    )]
    pub(crate) async fn update_issue(
        &self,
        Parameters(params): Parameters<UpdateIssueToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = UpdateIssueParams::new();
        if let Some(summary) = &params.summary {
            api_params = api_params.summary(summary);
        }
        if let Some(description) = &params.description {
            api_params = api_params.description(description);
        }
        if let Some(status_id) = params.status_id {
            api_params = api_params.status_id(status_id);
        }
        if let Some(assignee_id) = params.assignee_id {
            api_params = api_params.assignee_id(assignee_id);
        }
        if let Some(priority_id) = params.priority_id {
            api_params = api_params.priority_id(priority_id);
        }
        if let Some(due_date) = &params.due_date {
            match parse_date(due_date) {
                Ok(date) => api_params = api_params.due_date(date),
                Err(result) => return Ok(result),
            }
        }
        if let Some(comment) = &params.comment {
            api_params = api_params.comment(comment);
        }
        match self
            .client
            .issues()
            .update(&params.issue_key, api_params)
            .await
        {
            Ok(issue) => json_result(&IssueDetailOut::from(&issue)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "課題にコメントを追加する")]
    pub(crate) async fn add_comment(
        &self,
        Parameters(params): Parameters<AddCommentToolParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .client
            .issues()
            .add_comment(&params.issue_key, AddCommentParams::new(&params.content))
            .await
        {
            Ok(comment) => json_result(&CommentOut::from(&comment)),
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

    pub(crate) fn issue_json() -> serde_json::Value {
        json!({
            "id": 1, "projectId": 7, "issueKey": "TEST1-1", "keyId": 1,
            "issueType": { "id": 8, "projectId": 7, "name": "バグ", "color": "#990000", "displayOrder": 0 },
            "summary": "ログインできない", "description": "再現手順...",
            "resolution": null, "priority": { "id": 2, "name": "高" },
            "status": { "id": 1, "projectId": 7, "name": "未対応", "color": "#ed8077", "displayOrder": 1000 },
            "assignee": { "id": 2, "userId": "eguchi", "name": "eguchi", "roleType": 2 },
            "category": [], "versions": [], "milestone": [],
            "startDate": null, "dueDate": "2026-08-01T00:00:00Z",
            "estimatedHours": null, "actualHours": null, "parentIssueId": null,
            "createdUser": { "id": 1, "userId": "admin", "name": "admin", "roleType": 1 },
            "created": "2026-07-01T06:10:15Z",
            "updatedUser": null, "updated": "2026-07-20T08:09:49Z",
            "customFields": [],
            "attachments": [{ "id": 1, "name": "IMGP0088.JPG", "size": 85079 }],
            "sharedFiles": [], "stars": []
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
    async fn search_issues_resolves_project_key_and_returns_summaries() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues"))
            .and(query_param("projectId[]", "7"))
            .and(query_param("keyword", "ログイン"))
            .and(query_param("count", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue_json()])))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_issues(Parameters(super::SearchIssuesParams {
                project_key: Some("TEST1".to_string()),
                keyword: Some("ログイン".to_string()),
                status_ids: None,
                assignee_ids: None,
                count: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("TEST1-1"), "got: {text}");
        assert!(text.contains("未対応"), "got: {text}");
        assert!(text.contains("\\\"id\\\": 1"), "got: {text}");
        // 要約なので description は含まない
        assert!(!text.contains("再現手順"), "got: {text}");
    }

    #[tokio::test]
    async fn search_issues_with_unknown_project_returns_tool_error() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/NOPE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errors": [{ "message": "No project.", "code": 6 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_issues(Parameters(super::SearchIssuesParams {
                project_key: Some("NOPE".to_string()),
                keyword: None,
                status_ids: None,
                assignee_ids: None,
                count: None,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("No project."), "got: {text}");
    }

    #[tokio::test]
    async fn get_issue_returns_detail_with_description() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_issue(Parameters(super::GetIssueParams {
                issue_key: "TEST1-1".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("再現手順"), "got: {text}");
        assert!(text.contains("2026-08-01"), "got: {text}");
        assert!(text.contains("\\\"id\\\": 1"), "got: {text}");
        assert!(text.contains("IMGP0088.JPG"), "got: {text}");
        // 添付のIDが取れること(ダウンロードツールを呼ぶために必要)
        assert!(text.contains("\\\"id\\\": \\\"1\\\""), "got: {text}");
    }

    #[tokio::test]
    async fn list_comments_returns_author_and_content() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1/comments"))
            .and(query_param("count", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": 100, "projectId": 7, "issueId": 1,
                "content": "対応中です", "changeLog": [],
                "createdUser": { "id": 2, "userId": "eguchi", "name": "eguchi", "roleType": 2 },
                "created": "2026-07-21T01:00:00Z", "updated": "2026-07-21T01:00:00Z",
                "stars": [], "notifications": []
            }])))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_comments(Parameters(super::ListCommentsParams {
                issue_key: "TEST1-1".to_string(),
                count: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("対応中です"), "got: {text}");
        assert!(text.contains("eguchi"), "got: {text}");
    }

    #[tokio::test]
    async fn create_issue_posts_form_with_defaults() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .mount(&backlog)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/issues"))
            .and(body_string_contains("projectId=7"))
            .and(body_string_contains("issueTypeId=8"))
            .and(body_string_contains("priorityId=3"))
            .and(body_string_contains("dueDate=2026-08-01"))
            .respond_with(ResponseTemplate::new(201).set_body_json(issue_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .create_issue(Parameters(super::CreateIssueParams {
                project_key: "TEST1".to_string(),
                summary: "ログインできない".to_string(),
                issue_type_id: 8,
                priority_id: None,
                description: None,
                assignee_id: None,
                due_date: Some("2026-08-01".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("TEST1-1"), "got: {text}");
    }

    #[tokio::test]
    async fn create_issue_rejects_bad_date_without_calling_api() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .mount(&backlog)
            .await;
        // POST /api/v2/issues のモックは登録しない = 呼ばれたらテスト失敗

        let server = test_server(&backlog).await;
        let result = server
            .create_issue(Parameters(super::CreateIssueParams {
                project_key: "TEST1".to_string(),
                summary: "x".to_string(),
                issue_type_id: 8,
                priority_id: None,
                description: None,
                assignee_id: None,
                due_date: Some("8月1日".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("invalid date"), "got: {text}");
    }

    #[tokio::test]
    async fn update_issue_patches_only_given_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v2/issues/TEST1-1"))
            .and(body_string_contains("statusId=3"))
            .and(body_string_contains("comment=%E5%AF%BE%E5%BF%9C%E6%B8%88"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .update_issue(Parameters(super::UpdateIssueToolParams {
                issue_key: "TEST1-1".to_string(),
                summary: None,
                description: None,
                status_id: Some(3),
                assignee_id: None,
                priority_id: None,
                due_date: None,
                comment: Some("対応済".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn add_comment_posts_content() {
        let backlog = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/issues/TEST1-1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 101, "projectId": 7, "issueId": 1,
                "content": "承知しました", "changeLog": [],
                "createdUser": { "id": 1, "userId": "admin", "name": "admin", "roleType": 1 },
                "created": "2026-07-27T01:00:00Z", "updated": "2026-07-27T01:00:00Z",
                "stars": [], "notifications": []
            })))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .add_comment(Parameters(super::AddCommentToolParams {
                issue_key: "TEST1-1".to_string(),
                content: "承知しました".to_string(),
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("承知しました"), "got: {text}");
    }
}
