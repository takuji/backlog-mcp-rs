use backlog_api_rs::api::GetWatchingsParams;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

/// ウォッチ1件の出力。
#[derive(serde::Serialize)]
struct WatchingOut {
    id: u64,
    /// ウォッチしている課題の更新を見たか。false なら未読の更新がある。
    resource_already_read: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_content_updated: Option<String>,
}

impl From<&backlog_api_rs::models::Watching> for WatchingOut {
    fn from(w: &backlog_api_rs::models::Watching) -> Self {
        Self {
            id: w.id,
            resource_already_read: w.resource_already_read,
            issue_key: w.issue.as_ref().map(|i| i.issue_key.clone()),
            issue_summary: w.issue.as_ref().map(|i| i.summary.clone()),
            status_name: w.issue.as_ref().map(|i| i.status.name.clone()),
            note: w.note.clone(),
            last_content_updated: w.last_content_updated.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListWatchingsParams {
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
    /// ソート対象("created" / "updated" / "issueUpdated"。省略時 issueUpdated)
    pub sort: Option<String>,
    /// true にすると更新のある(未読の)ウォッチだけを返す
    pub unread_only: Option<bool>,
}

#[derive(serde::Serialize)]
struct WatchingCountOut {
    count: u64,
}

#[tool_router(router = watchings_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "自分がウォッチしている課題の一覧を取得する。resource_already_read が false のものは未読の更新がある。issue_key を get_issue に渡せる。unread_only は true のときだけ未読に絞り、false・省略時はどちらもフィルタしない(「既読だけ」を求める用途は無いため)"
    )]
    pub(crate) async fn list_watchings(
        &self,
        Parameters(params): Parameters<ListWatchingsParams>,
    ) -> Result<CallToolResult, McpError> {
        let user_id = match self.client.users().myself().await {
            Ok(user) => user.id,
            Err(e) => return Ok(backlog_error(e)),
        };
        let mut api_params = GetWatchingsParams::new();
        if let Some(count) = params.count {
            api_params = api_params.count(count);
        }
        if let Some(sort) = &params.sort {
            api_params = api_params.sort(sort);
        }
        if params.unread_only == Some(true) {
            api_params = api_params.resource_already_read(false);
        }
        match self
            .client
            .watchings()
            .list_for_user(user_id, api_params)
            .await
        {
            Ok(watchings) => {
                let out: Vec<WatchingOut> = watchings.iter().map(WatchingOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(description = "更新のある(未読の)ウォッチの件数を返す")]
    pub(crate) async fn count_watchings(&self) -> Result<CallToolResult, McpError> {
        let user_id = match self.client.users().myself().await {
            Ok(user) => user.id,
            Err(e) => return Ok(backlog_error(e)),
        };
        match self.client.watchings().unread_count_for_user(user_id).await {
            Ok(count) => json_result(&WatchingCountOut { count }),
            Err(e) => Ok(backlog_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::BacklogMcpConfig;
    use crate::server::BacklogMcpServer;
    use rmcp::handler::server::wrapper::Parameters;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_server(backlog: &MockServer) -> BacklogMcpServer {
        BacklogMcpServer::new(&BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
    }

    /// 指定したパス宛に届いたリクエストのクエリパラメータを取り出す。
    ///
    /// APIキーはヘッダで送るため、ここに現れるのはツール層が組み立てた
    /// パラメータだけになる。
    ///
    /// `query_param` マッチャーはあるキーが指定した値で「存在すること」しか
    /// 検証できず、余計なパラメータ(例えば MCP 層で露出していないはずの
    /// `order`)が混じっていても素通りしてしまう。完全一致を保証するには、
    /// 実際に届いたリクエストを捕まえてクエリ文字列そのものを比較する必要が
    /// ある(backlog-api-rs の `captured_query_pairs` と同じ考え方)。
    /// myself → watchings の2段リクエストがあるため、パスで対象を絞る。
    async fn captured_query_pairs(server: &MockServer, path: &str) -> Vec<(String, String)> {
        let requests = server.received_requests().await.unwrap();
        let request = requests
            .iter()
            .find(|r| r.url.path() == path)
            .unwrap_or_else(|| panic!("{path} 宛のリクエストが1件も届いていない"));

        // APIキーはヘッダ `Backlog-API-Key` で「ちょうど1個」送られ、クエリには
        // 一切載らない。ここを素通しにすると、APIキーがクエリ側に混ざる退行や
        // ヘッダごと落ちる退行を、このヘルパーを使う全テストが見逃してしまう。
        let api_key_headers = request.headers.get_all("backlog-api-key").iter().count();
        assert_eq!(
            api_key_headers, 1,
            "APIキーは Backlog-API-Key ヘッダでちょうど1個だけ送られるべき"
        );
        assert_eq!(
            request
                .url
                .query_pairs()
                .filter(|(k, _)| k == "apiKey")
                .count(),
            0,
            "APIキーがクエリパラメータに載っている: {:?}",
            request.url.query()
        );

        request
            .url
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// クエリパラメータの一致を「並び順」ではなく「集合(重複込み)」として
    /// 比較するためにソートする。`HashSet` にしないのは重複の有無まで
    /// 検出したいため(backlog-api-rs の `sorted_pairs` と同じ考え方)。
    fn sorted_pairs(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
        pairs.sort();
        pairs
    }

    /// 自分のユーザーIDを解決してからウォッチ一覧を取ること。
    ///
    /// APIがユーザーIDを要求するため、myself → watchings の2段になる。
    /// 両方のリクエストが起きることを検証する。
    ///
    /// フィクスチャのユーザーIDは(ありがちな `1234` ではなく)`9876` という
    /// 目立つ値にしている。`1234` だと `list_for_user(user_id, …)` を
    /// `list_for_user(1234, …)` にハードコードしても、myself 呼び出し自体は
    /// 起きるため `.expect(1)` は満たされ続け、IDが myself の結果に本当に
    /// 由来しているかを検出できなかった。`9876` ならハードコードされた
    /// `1234` は `/api/v2/users/1234/watchings` を叩いてしまい404で失敗する。
    #[tokio::test]
    async fn list_watchings_resolves_myself_first() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .and(query_param("resourceAlreadyRead", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 1, "resourceAlreadyRead": false,
                      "note": "あとで読む",
                      "issue": { "id": 4531, "projectId": 2, "issueKey": "TEST2-17",
                                 "keyId": 17,
                                 "issueType": { "id": 7, "projectId": 2, "name": "Bug",
                                                "color": "#990000", "displayOrder": 0 },
                                 "summary": "aaa",
                                 "status": { "id": 1, "projectId": 2, "name": "Open",
                                             "color": "#ed8077", "displayOrder": 1000 },
                                 "createdUser": { "id": 1, "userId": "admin",
                                                  "name": "admin", "roleType": 1 },
                                 "created": "2013-04-23T07:38:59Z" },
                      "lastContentUpdated": "2013-10-31T06:58:59Z",
                      "created": "2013-10-31T06:58:59Z" }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: None,
                sort: None,
                unread_only: Some(true),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("TEST2-17"), "got: {text}");
        assert!(text.contains("Open"), "got: {text}");
        assert!(text.contains("あとで読む"), "got: {text}");
    }

    /// unread_only を省略した場合は resourceAlreadyRead を送らないこと
    /// (既読・未読の両方が返る)。
    #[tokio::test]
    async fn list_watchings_without_unread_only_sends_no_filter() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .and(wiremock::matchers::query_param_is_missing(
                "resourceAlreadyRead",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: None,
                sort: None,
                unread_only: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    /// unread_only: false のときも resourceAlreadyRead を送らないこと
    /// (「既読だけ」を求める用途は無く、false は「絞らない」の意図)。
    #[tokio::test]
    async fn list_watchings_with_unread_only_false_sends_no_filter() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .and(wiremock::matchers::query_param_is_missing(
                "resourceAlreadyRead",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: None,
                sort: None,
                unread_only: Some(false),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn count_watchings_returns_the_count() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings/count"))
            .and(query_param("resourceAlreadyRead", "false"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(r#"{"count": 42}"#, "application/json"),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.count_watchings().await.unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 端数の値にした上でキーと値の組で照合する。丸め値の 3 だと
        // ハードコードされた `count: 3` を見逃す。
        assert!(text.contains("\\\"count\\\": 42"), "got: {text}");
    }

    /// MCPパラメータ(count/sort/unread_only)がすべて `GetWatchingsParams`
    /// のビルダーへ正しく渡ること。
    ///
    /// 3つの値をすべて異なる(取り違えても検出できる)ものにしておくことで、
    /// count と sort のキーの取り違えや、いずれかのパラメータの取りこぼしを
    /// 検出できる。
    ///
    /// クエリは `query_param` による存在確認だけでなく、実際に届いた
    /// クエリ集合を厳密に比較する。`order` はMCP層に意図的に露出していない
    /// パラメータであり(設計上の決定)、`query_param` の存在確認だけでは
    /// ツール実装が勝手に `.order(Order::Asc)` を足しても検出できないため。
    #[tokio::test]
    async fn list_watchings_threads_all_params_to_the_query() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: Some(7),
                sort: Some("created".to_string()),
                unread_only: Some(true),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        let pairs = captured_query_pairs(&backlog, "/api/v2/users/9876/watchings").await;
        assert_eq!(
            sorted_pairs(pairs),
            sorted_pairs(vec![
                ("count".to_string(), "7".to_string()),
                ("sort".to_string(), "created".to_string()),
                ("resourceAlreadyRead".to_string(), "false".to_string()),
            ])
        );
    }

    /// `WatchingOut` の各フィールドが正しいソースから正しい値で埋まること。
    ///
    /// `resource_already_read` を `true` にしたフィクスチャで検証し、他の
    /// フィールドを反転しても壊れないよう確認する(単純な反転バグの検出)。
    /// `issue_key` と `issue_summary` も異なる文字列にして取り違えを検出する。
    #[tokio::test]
    async fn watching_out_pins_each_field_without_swaps() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 777, "resourceAlreadyRead": true,
                      "note": "ノート本文マーカー",
                      "issue": { "id": 4531, "projectId": 2, "issueKey": "PROJ9-42",
                                 "keyId": 42,
                                 "issueType": { "id": 7, "projectId": 2, "name": "Bug",
                                                "color": "#990000", "displayOrder": 0 },
                                 "summary": "サマリー本文はキーと異なる",
                                 "status": { "id": 1, "projectId": 2, "name": "処理中",
                                             "color": "#ed8077", "displayOrder": 1000 },
                                 "createdUser": { "id": 1, "userId": "admin",
                                                  "name": "admin", "roleType": 1 },
                                 "created": "2013-04-23T07:38:59Z" },
                      "lastContentUpdated": "2020-01-02T03:04:05Z",
                      "created": "2013-10-31T06:58:59Z" }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: None,
                sort: None,
                unread_only: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // フィールド名と値の組で照合する。単純な contains だと issue_key と
        // issue_summary を取り違えても両方の文字列がどこかに出現するため
        // 検出できない。フィールド名を含めて初めて取り違えが失敗になる。
        assert!(text.contains("\\\"id\\\": 777"), "got: {text}");
        assert!(
            text.contains("\\\"resource_already_read\\\": true"),
            "got: {text}"
        );
        assert!(
            text.contains("\\\"issue_key\\\": \\\"PROJ9-42\\\""),
            "got: {text}"
        );
        assert!(
            text.contains("\\\"issue_summary\\\": \\\"サマリー本文はキーと異なる\\\""),
            "got: {text}"
        );
        assert!(
            text.contains("\\\"status_name\\\": \\\"処理中\\\""),
            "got: {text}"
        );
        assert!(
            text.contains("\\\"note\\\": \\\"ノート本文マーカー\\\""),
            "got: {text}"
        );
        assert!(
            text.contains("\\\"last_content_updated\\\": \\\"2020-01-02T03:04:05"),
            "got: {text}"
        );
    }

    /// `issue` が `null` のウォッチ(課題以外を対象としたウォッチ)でも
    /// エラーにならず、`issue` 由来の3フィールドが出力から欠けること。
    ///
    /// 実APIは課題以外のウォッチで `issue: null` を返すため、この分岐は
    /// 到達可能。
    #[tokio::test]
    async fn watching_out_omits_issue_fields_when_issue_is_null() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 9876, "userId": "me", "name": "私", "roleType": 2 }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9876/watchings"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 5, "resourceAlreadyRead": false, "issue": null,
                     "created": "2013-10-31T06:58:59Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_watchings(Parameters(super::ListWatchingsParams {
                count: None,
                sort: None,
                unread_only: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("\\\"id\\\": 5"), "got: {text}");
        assert!(
            text.contains("\\\"resource_already_read\\\": false"),
            "got: {text}"
        );
        assert!(!text.contains("issue_key"), "got: {text}");
        assert!(!text.contains("issue_summary"), "got: {text}");
        assert!(!text.contains("status_name"), "got: {text}");
    }
}
