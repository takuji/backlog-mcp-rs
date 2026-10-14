use backlog_api_rs::BacklogClient;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool_handler};

use crate::BacklogMcpConfig;

/// Backlogを読み書きするMCPサーバー本体。
///
/// ツールの実装は `src/tools/` 配下の `#[tool_router]` 付き impl ブロックに分かれている。
#[derive(Clone)]
pub struct BacklogMcpServer {
    pub(crate) client: BacklogClient,
    /// 接続先のBacklogのURL。
    ///
    /// `BacklogClient` は同じ値を非公開で持つが取り出せない。API の応答にも
    /// 含まれないため、`get_space` が「どこに繋がっているか」を答えるには
    /// ここに持っておく必要がある。
    pub(crate) base_url: String,
    pub(crate) download_dir: std::path::PathBuf,
    pub(crate) tool_router: ToolRouter<Self>,
}

/// このMCPサーバーが名乗る `User-Agent` の製品トークン。
///
/// `backlog-api-rs` 側のトークンはこの後ろに付く。
const USER_AGENT: &str = concat!("backlog-mcp/", env!("CARGO_PKG_VERSION"));

impl BacklogMcpServer {
    pub fn new(config: &BacklogMcpConfig) -> crate::Result<Self> {
        Ok(Self {
            // アプリに組み込まれた場合、アプリ自身も backlog-api-rs を直接使う。
            // ここで自分の名前を名乗ることで、サーバーのログから
            // 「利用者のアプリが定期取得した」と「AIがツールを呼んだ」を
            // 区別できる。ライブラリのトークンは後ろに残る。
            client: BacklogClient::builder(&config.base_url, config.api_key.clone())
                .user_agent(USER_AGENT)
                .build()?,
            base_url: config.base_url.clone(),
            download_dir: crate::tools::attachments::resolve_download_dir(),
            tool_router: Self::reference_tool_router()
                + Self::issues_tool_router()
                + Self::wikis_tool_router()
                + Self::documents_tool_router()
                + Self::attachments_tool_router()
                + Self::notifications_tool_router()
                + Self::watchings_tool_router()
                + Self::git_tool_router(),
        })
    }

    /// 添付ファイルの保存先を差し替える(テスト用)。
    #[cfg(test)]
    pub(crate) fn with_download_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.download_dir = dir;
        self
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BacklogMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Backlog(プロジェクト管理サービス)の課題とWikiを読み書きするツールを提供します。課題の作成・更新に必要なID(種別・状態・優先度など)は get_project で取得できます。Wikiの本文検索はできないため、search_wikis(ページ名の部分一致)で候補を絞り get_wiki で本文を読んでください。ドキュメントは document_tree で階層を把握し、get_document で本文を読めます(更新APIはありません)。添付ファイルは各詳細ツールの attachments でIDを調べ、download_*_attachment で取得できます(ファイルは ~/Downloads/backlog-mcp/ に保存され、返り値の path で開けます)。自分に届いた通知(お知らせ)は list_notifications / count_unread_notifications で、自分がウォッチしている課題は list_watchings / count_watchings で確認できます。Gitリポジトリは list_repositories で、プルリクエストは search_pull_requests / count_pull_requests / get_pull_request / list_pull_request_comments で確認できます。"
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServerHandler;

    fn server() -> BacklogMcpServer {
        BacklogMcpServer::new(&crate::BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
    }

    /// リクエストに `backlog-mcp/<version> backlog-api-rs/<version>` が
    /// 付くこと。
    ///
    /// 期待値はリテラルと `env!` から組む。定数から作ると被検証コードで
    /// 期待値を作ることになり、名前や順序の誤りを検出できない。
    #[tokio::test]
    async fn requests_carry_both_product_tokens() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/myself"))
            // ライブラリの版は下流からコンパイル時に知れないので、
            // 自分のトークンと順序だけを厳密に固定し、後続は形で見る
            .and(wiremock::matchers::header_regex(
                "user-agent",
                &format!(
                    r"^backlog-mcp/{} backlog-api-rs/\d+\.\d+\.\d+$",
                    env!("CARGO_PKG_VERSION").replace('.', r"\.")
                ),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1, "userId": "admin", "name": "admin", "roleType": 1
            })))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = BacklogMcpServer::new(&crate::BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap();
        let result = server.get_myself().await.unwrap();
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[test]
    fn tool_router_contains_reference_tools() {
        let s = server();
        // ツール本体を `#[tool_router]` の impl ブロックの外に書いてしまうと、
        // コンパイルもテストも通るのにツールが登録されない。ここで名前ごとに
        // 押さえておく。
        assert!(s.tool_router.has_route("get_space"));
        assert!(s.tool_router.has_route("get_myself"));
        assert!(s.tool_router.has_route("list_users"));
        assert!(s.tool_router.has_route("search_issues"));
    }

    #[test]
    fn get_info_enables_tools() {
        let info = server().get_info();
        assert!(info.capabilities.tools.is_some());
    }
}
