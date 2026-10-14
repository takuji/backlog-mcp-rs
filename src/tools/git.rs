use backlog_api_rs::api::{GetCommentsParams, GetPullRequestsParams, Order};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

/// `"asc"` / `"desc"` を [`Order`] に変換する。
///
/// この crate にはこれまで文字列から `Order` への変換が無く、必要になったのは
/// ここが初めて。サーバー実装は大小文字を区別せず `"desc"` 以外をすべて昇順と
/// みなすが、こちらは**綴り間違いを黙って昇順にしない**ためエラーにする。
fn parse_order(value: &str) -> Result<Order, String> {
    match value.to_ascii_lowercase().as_str() {
        "asc" => Ok(Order::Asc),
        "desc" => Ok(Order::Desc),
        other => Err(format!(
            "order には \"asc\" または \"desc\" を指定してください(受け取った値: {other})"
        )),
    }
}

/// リポジトリ1件の出力。
#[derive(serde::Serialize)]
struct RepositoryOut {
    id: u64,
    /// search_pull_requests / get_pull_request の repository_name に渡せる。
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// 最後にpushされた時刻。動いているリポジトリの判別に使う。
    #[serde(skip_serializing_if = "Option::is_none")]
    pushed_at: Option<String>,
}

impl From<&backlog_api_rs::models::GitRepository> for RepositoryOut {
    fn from(r: &backlog_api_rs::models::GitRepository) -> Self {
        Self {
            id: r.id,
            name: r.name.clone(),
            description: r.description.clone(),
            pushed_at: r.pushed_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// プルリクエスト1件の出力(一覧用。本文は含まない)。
#[derive(serde::Serialize)]
struct PullRequestSummaryOut {
    number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// 状態名(Open / Closed / Merged)。
    #[serde(skip_serializing_if = "Option::is_none")]
    status_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee_name: Option<String>,
    /// 関連課題のキー。get_issue に渡せる。紐づいていないPRでは省略される。
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    /// マージ済みかどうか(merge_at があるかで判定)。
    merged: bool,
}

impl From<&backlog_api_rs::models::PullRequest> for PullRequestSummaryOut {
    fn from(p: &backlog_api_rs::models::PullRequest) -> Self {
        Self {
            number: p.number,
            summary: p.summary.clone(),
            status_name: p.status.as_ref().and_then(|s| s.name.clone()),
            assignee_name: p.assignee.as_ref().map(|u| u.name.clone()),
            issue_key: p.issue.as_ref().map(|i| i.issue_key.clone()),
            created_user_name: p.created_user.as_ref().map(|u| u.name.clone()),
            created: p.created.map(|t| t.to_rfc3339()),
            merged: p.merge_at.is_some(),
        }
    }
}

/// プルリクエスト1件の詳細出力。
#[derive(serde::Serialize)]
struct PullRequestDetailOut {
    number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// 本文(全文)。
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_key: Option<String>,
    /// マージ先のブランチ。
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    /// マージ元のブランチ。
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merge_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merge_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    attachment_count: usize,
    /// マージ済みかどうか(merge_at があるかで判定)。
    ///
    /// 未マージのPRでは `merge_at` が `skip_serializing_if` で省略されるため、
    /// これが無いと「マージ済みか」を出力の欠落から推測するしかなくなる。
    merged: bool,
}

impl From<&backlog_api_rs::models::PullRequest> for PullRequestDetailOut {
    fn from(p: &backlog_api_rs::models::PullRequest) -> Self {
        Self {
            number: p.number,
            summary: p.summary.clone(),
            description: p.description.clone(),
            status_name: p.status.as_ref().and_then(|s| s.name.clone()),
            assignee_name: p.assignee.as_ref().map(|u| u.name.clone()),
            issue_key: p.issue.as_ref().map(|i| i.issue_key.clone()),
            base: p.base.clone(),
            branch: p.branch.clone(),
            base_commit: p.base_commit.clone(),
            branch_commit: p.branch_commit.clone(),
            merge_commit: p.merge_commit.clone(),
            close_at: p.close_at.map(|t| t.to_rfc3339()),
            merge_at: p.merge_at.map(|t| t.to_rfc3339()),
            created_user_name: p.created_user.as_ref().map(|u| u.name.clone()),
            created: p.created.map(|t| t.to_rfc3339()),
            attachment_count: p.attachments.len(),
            merged: p.merge_at.is_some(),
        }
    }
}

/// プルリクエストのコメント1件の出力。
#[derive(serde::Serialize)]
struct PullRequestCommentOut {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    /// ファイル指定のレビューコメントの対象ファイル。通常のコメントでは省略される。
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<i64>,
}

impl From<&backlog_api_rs::models::PullRequestComment> for PullRequestCommentOut {
    fn from(c: &backlog_api_rs::models::PullRequestComment) -> Self {
        Self {
            id: c.id,
            content: c.content.clone(),
            created_user_name: c.created_user.as_ref().map(|u| u.name.clone()),
            created: c.created.map(|t| t.to_rfc3339()),
            file_path: c.file_path.clone(),
            position: c.position,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListRepositoriesParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchPullRequestsParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// リポジトリ名(list_repositories で取得できる)
    pub repository_name: String,
    /// 状態IDで絞り込み(1=Open, 2=Closed, 3=Merged)
    pub status_ids: Option<Vec<u64>>,
    /// 担当者IDで絞り込み(list_users で取得できる)
    pub assignee_ids: Option<Vec<u64>>,
    /// 関連課題IDで絞り込み
    pub issue_ids: Option<Vec<u64>>,
    /// 作成者IDで絞り込み
    pub created_user_ids: Option<Vec<u64>>,
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
    /// 並び順("asc" または "desc")。**省略時は "desc"(新しい順)**。
    /// APIの既定は "asc" だが、件数上限が100のため既定のままでは最も古い100件が
    /// 返ってしまうので、このツールでは新しい順を既定にしている
    pub order: Option<String>,
    /// 並び替えの対象("number" / "issue" / "status" / "assigner" /
    /// "createdUser" / "created" / "updated"。省略時 "number")
    pub sort: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CountPullRequestsParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// リポジトリ名(list_repositories で取得できる)
    pub repository_name: String,
    /// 状態IDで絞り込み(1=Open, 2=Closed, 3=Merged)
    pub status_ids: Option<Vec<u64>>,
    /// 担当者IDで絞り込み(list_users で取得できる)
    pub assignee_ids: Option<Vec<u64>>,
    /// 関連課題IDで絞り込み
    pub issue_ids: Option<Vec<u64>>,
    /// 作成者IDで絞り込み
    pub created_user_ids: Option<Vec<u64>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct GetPullRequestParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// リポジトリ名(list_repositories で取得できる)
    pub repository_name: String,
    /// プルリクエスト番号
    pub number: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListPullRequestCommentsParams {
    /// プロジェクトキー(例: "PROJ")
    pub project_key: String,
    /// リポジトリ名(list_repositories で取得できる)
    pub repository_name: String,
    /// プルリクエスト番号
    pub number: u64,
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
}

#[derive(serde::Serialize)]
struct CountOut {
    count: u64,
}

/// 4つの絞り込みパラメータを共通の形でビルダーへ渡す。
///
/// search_pull_requests と count_pull_requests が同じ4つを受けるため、
/// 2箇所に同じ分岐を書くと片方だけ壊れても気づけない。
fn apply_pull_request_filters(
    mut params: GetPullRequestsParams,
    status_ids: Option<Vec<u64>>,
    assignee_ids: Option<Vec<u64>>,
    issue_ids: Option<Vec<u64>>,
    created_user_ids: Option<Vec<u64>>,
) -> GetPullRequestsParams {
    if let Some(ids) = status_ids {
        params = params.status_ids(ids);
    }
    if let Some(ids) = assignee_ids {
        params = params.assignee_ids(ids);
    }
    if let Some(ids) = issue_ids {
        params = params.issue_ids(ids);
    }
    if let Some(ids) = created_user_ids {
        params = params.created_user_ids(ids);
    }
    params
}

#[tool_router(router = git_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "プロジェクトのGitリポジトリ一覧を取得する。name を search_pull_requests / count_pull_requests / get_pull_request / list_pull_request_comments の repository_name に渡せる。"
    )]
    pub(crate) async fn list_repositories(
        &self,
        Parameters(params): Parameters<ListRepositoriesParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.git().repositories(&params.project_key).await {
            Ok(repos) => {
                let out: Vec<RepositoryOut> = repos.iter().map(RepositoryOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "リポジトリのプルリクエストを検索する。状態IDは 1=Open, 2=Closed, 3=Merged。既定では新しい順(order=desc)で返す。古い順が必要なら order に \"asc\" を指定する。sort で並び替えの対象を変えられる(number / issue / status / assigner / createdUser / created / updated)。本文は含まないので全文が必要なら get_pull_request を使う。関連課題があれば issue_key が入るので get_issue に渡せる。"
    )]
    pub(crate) async fn search_pull_requests(
        &self,
        Parameters(params): Parameters<SearchPullRequestsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = apply_pull_request_filters(
            GetPullRequestsParams::new(),
            params.status_ids,
            params.assignee_ids,
            params.issue_ids,
            params.created_user_ids,
        );
        if let Some(count) = params.count {
            api_params = api_params.count(count.clamp(1, 100));
        }
        if let Some(key) = &params.sort {
            api_params = api_params.sort(key);
        }
        // 省略時は新しい順。APIの既定(昇順)のままだと最も古い100件が返る。
        let order = match params.order.as_deref() {
            Some(value) => match parse_order(value) {
                Ok(order) => order,
                Err(message) => {
                    return Ok(CallToolResult::error(vec![
                        rmcp::model::ContentBlock::text(message),
                    ]));
                }
            },
            None => Order::Desc,
        };
        api_params = api_params.order(order);
        match self
            .client
            .git()
            .pull_requests(&params.project_key, &params.repository_name, api_params)
            .await
        {
            Ok(prs) => {
                let out: Vec<PullRequestSummaryOut> =
                    prs.iter().map(PullRequestSummaryOut::from).collect();
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "条件に合うプルリクエストの件数を返す。状態IDは 1=Open, 2=Closed, 3=Merged。"
    )]
    pub(crate) async fn count_pull_requests(
        &self,
        Parameters(params): Parameters<CountPullRequestsParams>,
    ) -> Result<CallToolResult, McpError> {
        let api_params = apply_pull_request_filters(
            GetPullRequestsParams::new(),
            params.status_ids,
            params.assignee_ids,
            params.issue_ids,
            params.created_user_ids,
        );
        match self
            .client
            .git()
            .pull_requests_count(&params.project_key, &params.repository_name, api_params)
            .await
        {
            Ok(count) => json_result(&CountOut { count }),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "プルリクエスト1件の詳細を取得する。本文・ブランチ・コミットハッシュを含む。"
    )]
    pub(crate) async fn get_pull_request(
        &self,
        Parameters(params): Parameters<GetPullRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .client
            .git()
            .pull_request(&params.project_key, &params.repository_name, params.number)
            .await
        {
            Ok(pr) => json_result(&PullRequestDetailOut::from(&pr)),
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "プルリクエストのコメント一覧を取得する。file_path が入っているものはファイル指定のレビューコメントで、position がその行位置。"
    )]
    pub(crate) async fn list_pull_request_comments(
        &self,
        Parameters(params): Parameters<ListPullRequestCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = GetCommentsParams::new();
        if let Some(count) = params.count {
            api_params = api_params.count(count.clamp(1, 100));
        }
        match self
            .client
            .git()
            .pull_request_comments(
                &params.project_key,
                &params.repository_name,
                params.number,
                api_params,
            )
            .await
        {
            Ok(comments) => {
                let out: Vec<PullRequestCommentOut> =
                    comments.iter().map(PullRequestCommentOut::from).collect();
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_server(backlog: &MockServer) -> BacklogMcpServer {
        BacklogMcpServer::new(&BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
    }

    /// 指定したモックサーバーに届いたリクエストのクエリパラメータを取り出す。
    ///
    /// APIキーはヘッダで送るため、ここに現れるのはツール層が組み立てた
    /// パラメータだけになる。
    ///
    /// `query_param` マッチャーはあるキーが指定した値で「存在すること」しか
    /// 検証できず、余計なパラメータが混じっていても素通りしてしまう。
    /// 完全一致を保証するには、実際に届いたリクエストを捕まえてクエリ文字列
    /// そのものを比較する必要がある(backlog-api-rs の `captured_query_pairs`
    /// と同じ考え方)。各テストで1エンドポイントしか叩かないため、1件目の
    /// リクエストを見れば十分。
    async fn captured_query_pairs(server: &MockServer) -> Vec<(String, String)> {
        let requests = server.received_requests().await.unwrap();
        let request = requests.first().expect("リクエストが1件も届いていない");

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

    /// MCPパラメータがすべてライブラリのビルダーへ渡ること。
    ///
    /// 値をすべて異なる数にしてあるので、キー名の取り違えや取りこぼしを検出できる。
    /// `query_param` は余分なパラメータを拒否しないため、届いたクエリ集合を
    /// 完全一致で比較する。
    #[tokio::test]
    async fn search_pull_requests_threads_all_params() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: Some(vec![1, 2]),
                assignee_ids: Some(vec![30]),
                issue_ids: Some(vec![40]),
                created_user_ids: Some(vec![50]),
                count: Some(60),
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        assert_eq!(
            sorted_pairs(captured_query_pairs(&backlog).await),
            sorted_pairs(vec![
                ("statusId[]".to_string(), "1".to_string()),
                ("statusId[]".to_string(), "2".to_string()),
                ("assigneeId[]".to_string(), "30".to_string()),
                ("issueId[]".to_string(), "40".to_string()),
                ("createdUserId[]".to_string(), "50".to_string()),
                ("count".to_string(), "60".to_string()),
                // order は省略時も必ず送られる(既定は desc)
                ("order".to_string(), "desc".to_string()),
            ])
        );
    }

    /// パラメータを省略した場合はクエリを付けないこと。
    #[tokio::test]
    async fn search_pull_requests_without_filters_sends_only_the_default_order() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        // 絞り込みも count も送らないが、order だけは既定の desc が入る。
        // count を勝手に 20 で埋めたり offset(0) を足したりしていれば落ちる。
        assert_eq!(
            captured_query_pairs(&backlog).await,
            vec![("order".to_string(), "desc".to_string())]
        );
    }

    /// 関連課題を持たないプルリクエストでツールがエラーにならず、
    /// `issue_key` が出力から省かれること。
    ///
    /// 実データでは100件中51件が `issue: null` であり、openapi.yaml と
    /// `backlog-js` の両方がこれを必須と書いている。信じていたら過半数で失敗した。
    #[tokio::test]
    async fn search_pull_requests_handles_pull_request_without_issue() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 10, "projectId": 2, "repositoryId": 3, "number": 7,
                       "summary": "設定を見直す",
                       "status": { "id": 1, "name": "Open" },
                       "assignee": null, "issue": null, "mergeAt": null,
                       "createdUser": { "id": 1, "userId": "admin",
                                        "name": "管理者", "roleType": 1 },
                       "created": "2013-05-30T09:11:36Z",
                       "attachments": [], "stars": [] }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 末尾に "\n" まで含めて閉じないと "70" のような値も拾ってしまう
        assert!(text.contains(r#"\"number\": 7,\n"#), "got: {text}");
        assert!(
            text.contains(r#"\"summary\": \"設定を見直す\""#),
            "got: {text}"
        );
        assert!(text.contains(r#"\"status_name\": \"Open\""#), "got: {text}");
        assert!(
            text.contains(r#"\"created_user_name\": \"管理者\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created\": \"2013-05-30T09:11:36+00:00\""#),
            "got: {text}"
        );
        assert!(text.contains(r#"\"merged\": false"#), "got: {text}");
        // 関連課題が無いのでキーは省略される
        assert!(!text.contains("issue_key"), "got: {text}");
        assert!(!text.contains("assignee_name"), "got: {text}");
    }

    /// マージ済みのプルリクエストで `merged` が true になり、`issue_key` が入ること。
    ///
    /// `merged` は `merge_at` の有無から決まる。両方向を固定する。
    #[tokio::test]
    async fn search_pull_requests_marks_merged_pull_requests() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 11, "projectId": 2, "repositoryId": 3, "number": 8,
                       "summary": "マージ済みのPR",
                       "status": { "id": 3, "name": "Merged" },
                       "issue": { "id": 4531, "projectId": 2, "issueKey": "PROJ-17",
                                  "keyId": 17,
                                  "issueType": { "id": 7, "projectId": 2, "name": "バグ",
                                                 "color": "#990000", "displayOrder": 0 },
                                  "summary": "課題の件名",
                                  "status": { "id": 1, "projectId": 2, "name": "未対応",
                                              "color": "#ed8077", "displayOrder": 1000 },
                                  "createdUser": { "id": 1, "userId": "admin",
                                                   "name": "管理者", "roleType": 1 },
                                  "created": "2013-04-23T07:38:59Z" },
                       "mergeAt": "2013-06-01T00:00:00Z",
                       "attachments": [], "stars": [] }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains(r#"\"merged\": true"#), "got: {text}");
        assert!(
            text.contains(r#"\"issue_key\": \"PROJ-17\""#),
            "got: {text}"
        );
    }

    /// リポジトリ一覧で `description` が null のものが混ざっても通ること。
    ///
    /// 実データでは102件中15件が `null`。openapi.yaml も `backlog-js` も
    /// これを必須と書いている。
    #[tokio::test]
    async fn list_repositories_handles_null_description() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/PROJ/git/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 1, "projectId": 2, "name": "repo-a",
                      "description": null, "pushedAt": null },
                    { "id": 2, "projectId": 2, "name": "repo-b",
                      "description": "説明あり",
                      "pushedAt": "2026-08-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_repositories(Parameters(super::ListRepositoriesParams {
                project_key: "PROJ".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 末尾に "\n" まで含めて閉じないと "10" のような値も拾ってしまう
        assert!(text.contains(r#"\"id\": 1,\n"#), "got: {text}");
        assert!(text.contains(r#"\"name\": \"repo-a\""#), "got: {text}");
        assert!(text.contains(r#"\"name\": \"repo-b\""#), "got: {text}");
        assert!(
            text.contains(r#"\"description\": \"説明あり\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"pushed_at\": \"2026-08-01T00:00:00+00:00\""#),
            "got: {text}"
        );
    }

    /// 詳細取得で17フィールドすべてが正しい値で返り、どのペアも
    /// 入れ替わっていないこと。
    ///
    /// `number` と `id`、`status_name` と `summary`、`assignee_name` と
    /// `created_user_name`、`close_at` と `merge_at`、`created` と `updated`
    /// が取り違えても検出できるよう、すべて異なる値にしてある。
    #[tokio::test]
    async fn get_pull_request_returns_details_without_swaps() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/7",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"{ "id": 10, "projectId": 2, "repositoryId": 3, "number": 7,
                     "summary": "件名です", "description": "本文です",
                     "base": "master", "branch": "topic/fix",
                     "baseCommit": "aaa111", "branchCommit": "bbb222",
                     "mergeCommit": "ccc333",
                     "status": { "id": 1, "name": "Open" },
                     "assignee": { "id": 2, "userId": "reviewer",
                                   "name": "担当者", "roleType": 2 },
                     "issue": { "id": 4531, "projectId": 2, "issueKey": "PROJ-17",
                                "keyId": 17,
                                "issueType": { "id": 7, "projectId": 2, "name": "バグ",
                                               "color": "#990000", "displayOrder": 0 },
                                "summary": "課題の件名",
                                "status": { "id": 1, "projectId": 2, "name": "未対応",
                                            "color": "#ed8077", "displayOrder": 1000 },
                                "createdUser": { "id": 1, "userId": "admin",
                                                 "name": "管理者", "roleType": 1 },
                                "created": "2013-04-23T07:38:59Z" },
                     "closeAt": "2026-01-01T00:00:00Z",
                     "mergeAt": "2026-02-02T00:00:00Z",
                     "createdUser": { "id": 1, "userId": "author",
                                      "name": "作成者", "roleType": 1 },
                     "created": "2013-05-30T09:11:36Z",
                     "updated": "2013-06-30T09:11:36Z",
                     "attachments": [{ "id": 1 }], "stars": [] }"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_pull_request(Parameters(super::GetPullRequestParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                number: 7,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        // 末尾に "\n" まで含めて閉じないと "70" のような値も拾ってしまう
        assert!(text.contains(r#"\"number\": 7,\n"#), "got: {text}");
        assert!(text.contains(r#"\"summary\": \"件名です\""#), "got: {text}");
        assert!(
            text.contains(r#"\"description\": \"本文です\""#),
            "got: {text}"
        );
        assert!(text.contains(r#"\"status_name\": \"Open\""#), "got: {text}");
        assert!(
            text.contains(r#"\"assignee_name\": \"担当者\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"issue_key\": \"PROJ-17\""#),
            "got: {text}"
        );
        assert!(text.contains(r#"\"base\": \"master\""#), "got: {text}");
        assert!(text.contains(r#"\"branch\": \"topic/fix\""#), "got: {text}");
        assert!(
            text.contains(r#"\"base_commit\": \"aaa111\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"branch_commit\": \"bbb222\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"merge_commit\": \"ccc333\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"close_at\": \"2026-01-01T00:00:00+00:00\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"merge_at\": \"2026-02-02T00:00:00+00:00\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created_user_name\": \"作成者\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created\": \"2013-05-30T09:11:36+00:00\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"attachment_count\": 1,\n"#),
            "got: {text}"
        );
        // mergeAt が値を持つので merged は true。
        // 末尾に "\n" まで含めて閉じないと "true" 以外の値も拾ってしまう
        assert!(text.contains(r#"\"merged\": true\n"#), "got: {text}");
    }

    /// 関連課題や担当者、マージ関連の時刻を持たないプルリクエストで、
    /// それぞれのフィールドが出力から省略されること。
    #[tokio::test]
    async fn get_pull_request_omits_null_optional_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/7",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 10, "projectId": 2, "repositoryId": 3, "number": 7,
                     "summary": "件名です", "description": "本文です",
                     "base": "master", "branch": "topic/fix",
                     "baseCommit": "aaa111", "branchCommit": "bbb222",
                     "mergeCommit": null,
                     "status": { "id": 1, "name": "Open" },
                     "assignee": null, "issue": null,
                     "closeAt": null, "mergeAt": null,
                     "attachments": [{ "id": 1 }], "stars": [] }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_pull_request(Parameters(super::GetPullRequestParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                number: 7,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // null の項目は省略される
        assert!(!text.contains("merge_commit"), "got: {text}");
        assert!(!text.contains("assignee_name"), "got: {text}");
        assert!(!text.contains("issue_key"), "got: {text}");
        assert!(!text.contains("close_at"), "got: {text}");
        assert!(!text.contains("merge_at"), "got: {text}");
        assert!(!text.contains("created_user_name"), "got: {text}");
        // mergeAt が null なので merged は false(省略されず false と出る)。
        assert!(text.contains(r#"\"merged\": false\n"#), "got: {text}");
    }

    /// クローズ済み(closeAt が値を持つ)だが未マージ(mergeAt が null)の
    /// プルリクエストで `merged` が false になること。
    ///
    /// `merged` の derivation を `p.close_at.is_some()` に取り違えても、
    /// `close_at` と `merge_at` が両方nullか両方値ありの fixture しか無いと
    /// 検出できない。ここでは両者をわざと異なる値(closeAt だけ値あり)にして
    /// `merge_at` 由来であることを固定する。
    #[tokio::test]
    async fn get_pull_request_merged_reflects_merge_at_not_close_at() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/7",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{ "id": 10, "projectId": 2, "repositoryId": 3, "number": 7,
                     "status": { "id": 2, "name": "Closed" },
                     "closeAt": "2026-01-01T00:00:00Z", "mergeAt": null,
                     "attachments": [], "stars": [] }"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .get_pull_request(Parameters(super::GetPullRequestParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                number: 7,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(
            text.contains(r#"\"close_at\": \"2026-01-01T00:00:00+00:00\""#),
            "got: {text}"
        );
        assert!(!text.contains("merge_at"), "got: {text}");
        assert!(text.contains(r#"\"merged\": false\n"#), "got: {text}");
    }

    /// `order` と `sort` が指定どおりクエリに乗ること。
    ///
    /// この2つは**公式APIリファレンスに記載が無い**が、サーバー実装の
    /// `PullRequestQueryForm` が受け付ける。記載が無いために当初「並び順は
    /// 指定できない」と設計判断してしまい、最も古い100件しか取れない状態で
    /// 出荷していた。
    #[tokio::test]
    async fn search_pull_requests_threads_order_and_sort() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: Some("asc".to_string()),
                sort: Some("updated".to_string()),
            }))
            .await
            .unwrap();

        assert_eq!(
            sorted_pairs(captured_query_pairs(&backlog).await),
            sorted_pairs(vec![
                ("order".to_string(), "asc".to_string()),
                ("sort".to_string(), "updated".to_string()),
            ])
        );
    }

    /// `order` に明示的に `"desc"` を渡した場合も `desc` が送られること。
    ///
    /// 既定値の経路は `parse_order` を通らないため、この明示指定が無いと
    /// `parse_order` の `"desc"` の腕を `Order::Asc` に取り違えても誰も気づかない
    /// (実際にその変異が生き残ることを確認して追加した)。
    #[tokio::test]
    async fn search_pull_requests_accepts_an_explicit_desc_order() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: Some("DESC".to_string()),
                sort: None,
            }))
            .await
            .unwrap();

        // 大文字でも受ける(サーバー実装が大小文字を区別しないため)
        assert_eq!(
            captured_query_pairs(&backlog).await,
            vec![("order".to_string(), "desc".to_string())]
        );
    }

    /// `order` の綴りが違う場合はツールエラーにすること。
    ///
    /// サーバー実装は `"desc"` 以外をすべて昇順とみなすため、綴り間違いが
    /// 黙って「古い順」になってしまう。呼び出し側が気づけるようにエラーにする。
    #[tokio::test]
    async fn search_pull_requests_rejects_an_invalid_order() {
        let backlog = MockServer::start().await;
        // リクエストが飛んではいけない
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(0)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: Some("descending".to_string()),
                sort: None,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("descending"), "got: {text}");
    }

    /// 件数取得がフィルタを送り、件数を返すこと。
    #[tokio::test]
    async fn count_pull_requests_returns_the_count() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/count",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(r#"{"count": 42}"#, "application/json"),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .count_pull_requests(Parameters(super::CountPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: Some(vec![1]),
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        // count は CountOut の唯一のフィールドなのでカンマは付かない。
        // 末尾に "\n" まで含めて閉じないと "420" のような値も拾ってしまう
        assert!(text.contains(r#"\"count\": 42\n"#), "got: {text}");
        assert_eq!(
            captured_query_pairs(&backlog).await,
            vec![("statusId[]".to_string(), "1".to_string())]
        );
    }

    /// 件数取得が4つの絞り込みパラメータすべてをビルダーへ渡すこと。
    ///
    /// `search_pull_requests_threads_all_params` と同じ形。値をすべて異なる
    /// 数にしてあるので、キー名の取り違えや取りこぼしを検出できる。
    /// count_pull_requests は count/offset を送ってはならないので、それも
    /// 含めて届いたクエリ集合を完全一致で比較する。
    #[tokio::test]
    async fn count_pull_requests_threads_all_filters() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/count",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(r#"{"count": 1}"#, "application/json"),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .count_pull_requests(Parameters(super::CountPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: Some(vec![1, 2]),
                assignee_ids: Some(vec![30]),
                issue_ids: Some(vec![40]),
                created_user_ids: Some(vec![50]),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        assert_eq!(
            sorted_pairs(captured_query_pairs(&backlog).await),
            sorted_pairs(vec![
                ("statusId[]".to_string(), "1".to_string()),
                ("statusId[]".to_string(), "2".to_string()),
                ("assigneeId[]".to_string(), "30".to_string()),
                ("issueId[]".to_string(), "40".to_string()),
                ("createdUserId[]".to_string(), "50".to_string()),
            ])
        );
    }

    /// ファイル指定のレビューコメントで `file_path` と `position` が返ること。
    ///
    /// この4フィールドは openapi.yaml にあるが `backlog-js` には無い。
    /// `backlog-js` だけを見てモデルを書くと落とす情報である。
    ///
    /// 6フィールドすべて(id/content/created_user_name/created/file_path/
    /// position)を検証し、どの行のどのフィールドが取り違わっていないこと
    /// も確認する。2件のコメントで id・投稿者名・時刻をすべて異なる値に
    /// してあるので、行またぎやフィールドの取り違えを検出できる
    /// (`get_pull_request_returns_details_without_swaps` と同じ考え方)。
    #[tokio::test]
    async fn list_pull_request_comments_returns_file_path_for_inline_comments() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/7/comments",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 1, "content": "この行は不要では",
                      "filePath": "src/main.rs", "position": 42,
                      "createdUser": { "id": 1, "userId": "reviewer",
                                       "name": "レビュアー", "roleType": 2 },
                      "created": "2013-05-30T09:11:36Z" },
                    { "id": 2, "content": "全体としては良さそう",
                      "filePath": null, "position": null,
                      "createdUser": { "id": 2, "userId": "author",
                                       "name": "作成者", "roleType": 1 },
                      "created": "2013-06-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_pull_request_comments(Parameters(super::ListPullRequestCommentsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                number: 7,
                count: Some(50),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // id は構造体の先頭フィールドで直後に content が続くのでカンマが付く。
        // 末尾に "\n" まで含めて閉じないと "10" や "20" のような値も拾ってしまう
        assert!(text.contains(r#"\"id\": 1,\n"#), "got: {text}");
        assert!(text.contains(r#"\"id\": 2,\n"#), "got: {text}");
        assert!(
            text.contains(r#"\"content\": \"この行は不要では\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"content\": \"全体としては良さそう\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created_user_name\": \"レビュアー\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created_user_name\": \"作成者\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created\": \"2013-05-30T09:11:36+00:00\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"created\": \"2013-06-01T00:00:00+00:00\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"file_path\": \"src/main.rs\""#),
            "got: {text}"
        );
        // position は構造体の最後のフィールドなのでカンマは付かない。
        // 末尾に "\n" まで含めて閉じないと "420" のような値も拾ってしまう
        assert!(text.contains(r#"\"position\": 42\n"#), "got: {text}");
        assert_eq!(
            captured_query_pairs(&backlog).await,
            vec![("count".to_string(), "50".to_string())]
        );
    }

    /// 通常のコメント(ファイル指定ではないもの)で `file_path` と `position`
    /// がキーごと出力から省略されること。
    ///
    /// 上のテストは1件目にファイル指定コメントを混ぜているため、出力全体に
    /// 対して「file_path という文字列を含まない」という検証はできない
    /// (1件目に本物の file_path があるため)。`skip_serializing_if` を
    /// 外しても検出できるよう、通常コメントのみを1件返すフィクスチャで
    /// 省略を確認する(`get_pull_request_omits_null_optional_fields` と同じ
    /// 考え方)。
    #[tokio::test]
    async fn list_pull_request_comments_omits_null_optional_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests/7/comments",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 3, "content": "普通のコメント",
                      "filePath": null, "position": null,
                      "createdUser": { "id": 1, "userId": "admin",
                                       "name": "管理者", "roleType": 1 },
                      "created": "2013-05-30T09:11:36Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_pull_request_comments(Parameters(super::ListPullRequestCommentsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                number: 7,
                count: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // null の項目はキーごと省略される
        assert!(!text.contains("file_path"), "got: {text}");
        assert!(!text.contains("position"), "got: {text}");
    }

    /// リポジトリの `description` と `pushed_at` が null のとき、キーごと
    /// 出力から省略されること。
    ///
    /// `list_repositories_handles_null_description` は説明ありのリポジトリも
    /// 一緒に返すため、出力全体に対して「description という文字列を含まない」
    /// という検証はできない(2件目に本物の description があるため)。
    /// `skip_serializing_if` を外しても検出できるよう、null しか持たない
    /// リポジトリ単独のフィクスチャで省略を確認する
    /// (`get_pull_request_omits_null_optional_fields` と同じ考え方)。
    #[tokio::test]
    async fn list_repositories_omits_null_optional_fields() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/projects/PROJ/git/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 1, "projectId": 2, "name": "repo-a",
                      "description": null, "pushedAt": null }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_repositories(Parameters(super::ListRepositoriesParams {
                project_key: "PROJ".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // null の項目はキーごと省略される
        assert!(!text.contains("description"), "got: {text}");
        assert!(!text.contains("pushed_at"), "got: {text}");
    }

    /// 検索結果が2件以上あるとき、両方がマッピングされること。
    ///
    /// 既存のフィクスチャがどれも1件しか返さなかったため、マッピングの
    /// `iter().map(...)` を `take(1)` に変えても全テストが通ってしまう
    /// 抜け穴だった。2件目のPRだけが持つ値を検証することで、それを閉じる。
    #[tokio::test]
    async fn search_pull_requests_maps_all_results_not_just_the_first() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 11, "projectId": 2, "repositoryId": 3, "number": 8,
                       "summary": "マージ済みのPR",
                       "status": { "id": 3, "name": "Merged" },
                       "issue": { "id": 4531, "projectId": 2, "issueKey": "PROJ-17",
                                  "keyId": 17,
                                  "issueType": { "id": 7, "projectId": 2, "name": "バグ",
                                                 "color": "#990000", "displayOrder": 0 },
                                  "summary": "課題の件名",
                                  "status": { "id": 1, "projectId": 2, "name": "未対応",
                                              "color": "#ed8077", "displayOrder": 1000 },
                                  "createdUser": { "id": 1, "userId": "admin",
                                                   "name": "管理者", "roleType": 1 },
                                  "created": "2013-04-23T07:38:59Z" },
                       "mergeAt": "2013-06-01T00:00:00Z",
                       "attachments": [], "stars": [] },
                     { "id": 12, "projectId": 2, "repositoryId": 3, "number": 9,
                       "summary": "オープン中のPR",
                       "status": { "id": 1, "name": "Open" },
                       "assignee": null, "issue": null, "mergeAt": null,
                       "createdUser": { "id": 1, "userId": "admin",
                                        "name": "管理者", "roleType": 1 },
                       "created": "2013-05-30T09:11:36Z",
                       "attachments": [], "stars": [] }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 1件目(マージ済み)
        assert!(text.contains(r#"\"number\": 8,\n"#), "got: {text}");
        assert!(text.contains(r#"\"merged\": true"#), "got: {text}");
        // 2件目(オープン中)。take(1) では失われる
        assert!(text.contains(r#"\"number\": 9,\n"#), "got: {text}");
        assert!(text.contains(r#"\"merged\": false"#), "got: {text}");
    }

    /// Backlog APIがエラーを返したとき、ツールが `is_error` を立てて返すこと。
    ///
    /// `Err(e) => Ok(backlog_error(e))` を空の成功に置き換えても他の
    /// テストは全部通ってしまう抜け穴だったため、エラー応答専用に検証する。
    #[tokio::test]
    async fn search_pull_requests_returns_error_on_api_failure() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/projects/PROJ/git/repositories/example-repo/pullRequests",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errors": [{ "message": "No such repository.", "code": 6 }]
            })))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .search_pull_requests(Parameters(super::SearchPullRequestsParams {
                project_key: "PROJ".to_string(),
                repository_name: "example-repo".to_string(),
                status_ids: None,
                assignee_ids: None,
                issue_ids: None,
                created_user_ids: None,
                count: None,
                order: None,
                sort: None,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("No such repository."), "got: {text}");
    }
}
