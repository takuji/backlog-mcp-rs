use backlog_api_rs::models::{Project, User};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListUsersParams {
    /// ユーザー名の部分一致フィルタ(省略時は全ユーザー)
    pub keyword: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct UserOut {
    pub id: u64,
    pub user_id: Option<String>,
    pub name: String,
    pub mail_address: Option<String>,
}

impl From<&User> for UserOut {
    fn from(u: &User) -> Self {
        Self {
            id: u.id,
            user_id: u.user_id.clone(),
            name: u.name.clone(),
            mail_address: u.mail_address.clone(),
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct GetProjectParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
}

#[derive(serde::Serialize)]
pub(crate) struct ProjectOut {
    pub id: u64,
    pub project_key: String,
    pub name: String,
    pub text_formatting_rule: String,
    pub archived: bool,
}

impl From<&Project> for ProjectOut {
    fn from(p: &Project) -> Self {
        Self {
            id: p.id,
            project_key: p.project_key.clone(),
            name: p.name.clone(),
            text_formatting_rule: p.text_formatting_rule.clone(),
            archived: p.archived,
        }
    }
}

/// スペースの同定と日時解釈に必要な情報。
///
/// ライブラリの `Space` は9フィールドを持つが、ここでは5つだけ返す。
/// LLM が読む出力ではフィールド1つごとにトークンと注意を消費するため、
/// 用途のあるものに絞る(既存の `ProjectOut` と同じ方針)。
///
/// **`text_formatting_rule` は意図的に含めない。** スペースの値はプロジェクトが
/// 未設定のときのフォールバックで、本文の書式を決めるのはプロジェクト側である
/// (サーバー実装 `Space.scala` の `resolveTextFormattingRule`)。実測では
/// スペースが `backlog` でも325プロジェクト中218が `markdown` だった。
/// 正しい値は `get_project` が返す(API 側で解決済み)。
#[derive(serde::Serialize)]
pub(crate) struct SpaceOut {
    pub space_key: String,
    /// 接続先のURL。API の応答には含まれないので設定から入れる。
    pub base_url: String,
    pub name: String,
    pub lang: Option<String>,
    pub timezone: Option<String>,
}

/// 参照データの id と name のペア。
#[derive(serde::Serialize)]
pub(crate) struct IdName {
    pub id: u64,
    pub name: String,
}

#[derive(serde::Serialize)]
pub(crate) struct ProjectMetadata {
    pub project: ProjectOut,
    pub issue_types: Vec<IdName>,
    pub statuses: Vec<IdName>,
    pub categories: Vec<IdName>,
    pub versions: Vec<IdName>,
    pub priorities: Vec<IdName>,
}

#[tool_router(router = reference_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "接続しているBacklogスペースの情報を取得する。どのスペースを見ているか分からないときに使う。timezone はこのスペースの表示タイムゾーンで、APIが返す日時は常にUTC(末尾Z)なので、「今日」「期限切れ」を判断するときはこのタイムゾーンで解釈すること。本文をMarkdownとBacklog記法のどちらで書くかは**このツールでは分からない**。書式はプロジェクトごとに決まるので get_project の text_formatting_rule を見ること"
    )]
    pub(crate) async fn get_space(&self) -> Result<CallToolResult, McpError> {
        match self.client.space().info().await {
            Ok(space) => json_result(&SpaceOut {
                space_key: space.space_key,
                base_url: self.base_url.clone(),
                name: space.name,
                lang: space.lang,
                timezone: space.timezone,
            }),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "APIキーのユーザー自身(自分)の情報を取得する")]
    pub(crate) async fn get_myself(&self) -> Result<CallToolResult, McpError> {
        match self.client.users().myself().await {
            Ok(user) => json_result(&UserOut::from(&user)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "スペースのユーザー一覧を取得する(課題の担当者IDを調べるのに使う)")]
    pub(crate) async fn list_users(
        &self,
        Parameters(params): Parameters<ListUsersParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.users().list().await {
            Ok(users) => {
                let out: Vec<UserOut> = users
                    .iter()
                    .filter(|u| match &params.keyword {
                        Some(k) => u.name.contains(k.as_str()),
                        None => true,
                    })
                    .map(UserOut::from)
                    .collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "参加しているプロジェクトの一覧(キーと名前)を取得する")]
    pub(crate) async fn list_projects(&self) -> Result<CallToolResult, McpError> {
        match self.client.projects().list().await {
            Ok(projects) => {
                let out: Vec<ProjectOut> = projects.iter().map(ProjectOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "プロジェクトの詳細と、課題の作成・更新に必要な参照データ(課題種別・状態・カテゴリー・バージョン・優先度のID一覧)をまとめて取得する"
    )]
    pub(crate) async fn get_project(
        &self,
        Parameters(params): Parameters<GetProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let key = &params.project_key;
        let projects = self.client.projects();
        // try_join! に渡す Future が借用するため、一時値のままにせず束縛しておく
        let space = self.client.space();
        let result: backlog_api_rs::Result<ProjectMetadata> = async {
            // 6つの取得は互いに独立しているので並行に投げる。逐次だと
            // 往復6回分の待ち時間になるが、並行なら一番遅い1本分で済む。
            let (project, issue_types, statuses, categories, versions, priorities) = tokio::try_join!(
                projects.get(key),
                projects.issue_types(key),
                projects.statuses(key),
                projects.categories(key),
                projects.versions(key),
                space.priorities(),
            )?;
            Ok(ProjectMetadata {
                project: ProjectOut::from(&project),
                issue_types: issue_types
                    .into_iter()
                    .map(|t| IdName {
                        id: t.id,
                        name: t.name,
                    })
                    .collect(),
                statuses: statuses
                    .into_iter()
                    .map(|s| IdName {
                        id: s.id,
                        name: s.name,
                    })
                    .collect(),
                categories: categories
                    .into_iter()
                    .map(|c| IdName {
                        id: c.id,
                        name: c.name,
                    })
                    .collect(),
                versions: versions
                    .into_iter()
                    .map(|v| IdName {
                        id: v.id,
                        name: v.name,
                    })
                    .collect(),
                priorities: priorities
                    .into_iter()
                    .map(|p| IdName {
                        id: p.id,
                        name: p.name,
                    })
                    .collect(),
            })
        }
        .await;
        match result {
            Ok(metadata) => json_result(&metadata),
            Err(e) => Ok(backlog_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::BacklogMcpConfig;
    use crate::server::BacklogMcpServer;
    use crate::tools::output_json;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) async fn test_server(backlog: &MockServer) -> BacklogMcpServer {
        BacklogMcpServer::new(&BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
    }

    fn user_json(id: u64, name: &str) -> serde_json::Value {
        json!({ "id": id, "userId": name, "name": name, "roleType": 2,
                "mailAddress": format!("{name}@example.com") })
    }

    #[tokio::test]
    async fn get_myself_returns_user_json() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .and(header("Backlog-API-Key", "KEY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user_json(1, "admin")))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.get_myself().await.unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("admin"), "got: {text}");
    }

    #[tokio::test]
    async fn get_myself_maps_api_error_to_tool_error() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "errors": [{ "message": "Authentication failure.", "code": 11 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.get_myself().await.unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("Authentication failure."), "got: {text}");
    }

    #[tokio::test]
    async fn list_users_filters_by_keyword() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([user_json(1, "alice"), user_json(2, "bob")])),
            )
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_users(rmcp::handler::server::wrapper::Parameters(
                super::ListUsersParams {
                    keyword: Some("ali".to_string()),
                },
            ))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("alice"), "got: {text}");
        assert!(!text.contains("bob"), "got: {text}");
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
    async fn list_projects_returns_key_and_name() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([project_json()])))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.list_projects().await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("TEST1"), "got: {text}");
    }

    #[tokio::test]
    async fn get_project_bundles_reference_data() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project_json()))
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1/issueTypes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 8, "projectId": 7, "name": "バグ", "color": "#990000", "displayOrder": 0 }
            ])))
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1/statuses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "projectId": 7, "name": "未対応", "color": "#ed8077", "displayOrder": 1000 }
            ])))
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 12, "name": "開発", "displayOrder": 0 }
            ])))
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/TEST1/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 3, "projectId": 7, "name": "v1", "description": null,
                  "startDate": null, "releaseDueDate": null, "archived": false, "displayOrder": 0 }
            ])))
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/priorities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 2, "name": "高" }, { "id": 3, "name": "中" }, { "id": 4, "name": "低" }
            ])))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_project(rmcp::handler::server::wrapper::Parameters(
                super::GetProjectParams {
                    project_key: "TEST1".to_string(),
                },
            ))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        for expected in ["バグ", "未対応", "開発", "v1", "高"] {
            assert!(text.contains(expected), "missing {expected} in: {text}");
        }
    }

    /// 6つの取得が並行に走ることを、応答遅延の合計で検証する。
    ///
    /// 各エンドポイントに 300ms の遅延を入れる。逐次なら 1.8 秒以上かかるが、
    /// 並行なら 1 秒未満で終わる。閾値は 1 秒に置き、遅いマシンでも
    /// 「逐次に戻った」場合だけが落ちるようにしている。
    #[tokio::test]
    async fn get_project_fetches_concurrently() {
        use std::time::{Duration, Instant};

        let delay = Duration::from_millis(300);
        let backlog = MockServer::start().await;
        let endpoints: [(&str, serde_json::Value); 6] = [
            ("/api/v2/projects/TEST1", project_json()),
            ("/api/v2/projects/TEST1/issueTypes", json!([])),
            ("/api/v2/projects/TEST1/statuses", json!([])),
            ("/api/v2/projects/TEST1/categories", json!([])),
            ("/api/v2/projects/TEST1/versions", json!([])),
            ("/api/v2/priorities", json!([])),
        ];
        for (p, body) in endpoints {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(body)
                        .set_delay(delay),
                )
                .expect(1)
                .mount(&backlog)
                .await;
        }

        let server = test_server(&backlog).await;
        let started = Instant::now();
        let result = server
            .get_project(rmcp::handler::server::wrapper::Parameters(
                super::GetProjectParams {
                    project_key: "TEST1".to_string(),
                },
            ))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_ne!(result.is_error, Some(true));
        assert!(
            elapsed < Duration::from_secs(1),
            "逐次実行に戻った可能性がある(所要 {elapsed:?}、遅延 {delay:?} × 6)"
        );
    }

    fn space_json() -> serde_json::Value {
        json!({
            "spaceKey": "EXAMPLE",
            "name": "サンプルスペース",
            "ownerId": 77,
            "lang": "ja",
            "timezone": "Asia/Tokyo",
            "reportSendTime": "08:00:00",
            "textFormattingRule": "backlog",
            "created": "2013-05-30T09:11:36Z",
            "updated": "2014-06-15T10:20:45Z"
        })
    }

    /// スペース情報が正しいパスから取得でき、5フィールドが正しく埋まること。
    #[tokio::test]
    async fn get_space_returns_the_space_identity() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/space"))
            .and(header("Backlog-API-Key", "KEY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(space_json()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.get_space().await.unwrap();
        assert_ne!(result.is_error, Some(true));

        let out = output_json(&result);
        assert_eq!(out["space_key"], "EXAMPLE");
        assert_eq!(out["name"], "サンプルスペース");
        assert_eq!(out["lang"], "ja");
        assert_eq!(out["timezone"], "Asia/Tokyo");
        // base_url は API 応答ではなく設定由来
        assert_eq!(out["base_url"], backlog.uri());
    }

    /// **`base_url` は API 応答ではなく設定から来ること。**
    ///
    /// 応答に紛らわしいキーを混ぜても、設定の値が出ることを固定する。
    #[tokio::test]
    async fn get_space_takes_the_base_url_from_the_config_not_the_response() {
        let backlog = MockServer::start().await;
        let mut body = space_json();
        body["baseUrl"] = json!("https://impostor.example.com");
        body["url"] = json!("https://impostor.example.com");
        Mock::given(method("GET"))
            .and(path("/api/v2/space"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let out = output_json(&server.get_space().await.unwrap());
        assert_eq!(out["base_url"], backlog.uri());
        let text = serde_json::to_string(&out).unwrap();
        assert!(!text.contains("impostor"), "応答のURLが漏れている: {text}");
    }

    /// **出力してはいけないフィールドが出ていないこと。**
    ///
    /// とくに `text_formatting_rule`: スペースの値はプロジェクトが未設定のときの
    /// フォールバックにすぎず、実測ではスペースが `backlog` でも325プロジェクト中
    /// 218が `markdown` だった。正しい値は `get_project` が返す。
    #[tokio::test]
    async fn get_space_omits_the_fields_that_would_mislead_or_add_noise() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/space"))
            .respond_with(ResponseTemplate::new(200).set_body_json(space_json()))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let out = output_json(&server.get_space().await.unwrap());
        for key in [
            "text_formatting_rule",
            "textFormattingRule",
            "owner_id",
            "ownerId",
            "report_send_time",
            "reportSendTime",
            "created",
            "updated",
        ] {
            assert!(out.get(key).is_none(), "{key} が出力されている: {out}");
        }
        // 値そのものが別のキー名で漏れていないことも見る
        let text = serde_json::to_string(&out).unwrap();
        for value in ["08:00:00", "2013-05-30", "2014-06-15", "77"] {
            assert!(!text.contains(value), "{value} が漏れている: {text}");
        }
        // 出力は5フィールドちょうど
        assert_eq!(out.as_object().unwrap().len(), 5, "got: {out}");
    }

    /// 任意フィールドが欠けていても失敗しないこと。
    ///
    /// ライブラリの `Space` は `space_key` と `name` 以外を `Option` にしている
    /// (観測が1スペースのみのため)。
    #[tokio::test]
    async fn get_space_tolerates_missing_optional_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/space"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "spaceKey": "X", "name": "n"
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.get_space().await.unwrap();
        assert_ne!(result.is_error, Some(true));
        let out = output_json(&result);
        assert_eq!(out["space_key"], "X");
        assert!(out["lang"].is_null());
        assert!(out["timezone"].is_null());
    }

    /// APIエラーがツールエラーとして返ること。
    #[tokio::test]
    async fn get_space_maps_api_error_to_tool_error() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/space"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "errors": [{ "message": "Authentication failure.", "code": 11 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.get_space().await.unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("Authentication failure."), "got: {text}");
    }
}
