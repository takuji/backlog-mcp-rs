use std::net::SocketAddr;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use crate::server::BacklogMcpServer;
use crate::{BacklogMcpConfig, Error, Result};

/// stdio でMCPサーバーを起動し、クライアントが切断するまで待つ。
///
/// 単体バイナリ(MCPクライアントがプロセスとして起動する形態)向け。
pub async fn serve_stdio(config: BacklogMcpConfig) -> Result<()> {
    let server = BacklogMcpServer::new(&config)?;
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| Error::Serve(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| Error::Serve(e.to_string()))?;
    Ok(())
}

/// `127.0.0.1:<port>` の `/mcp` でMCPサーバーを起動する(Streamable HTTP)。
///
/// Tauriアプリなどへの組み込み向け。`port = 0` でOSが空きポートを割り当てる。
/// 返されたハンドルの [`HttpServerHandle::shutdown`] で停止する。
pub async fn serve_http(config: BacklogMcpConfig, port: u16) -> Result<HttpServerHandle> {
    let server = BacklogMcpServer::new(&config)?;
    let ct = CancellationToken::new();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let local_addr = listener.local_addr()?;

    let http_config =
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token());
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        http_config,
    );
    // DNSリバインディング対策: 実際にバインドしたポートでHost/Originを検証する
    let router = axum::Router::new().nest_service("/mcp", service).layer(
        axum::middleware::from_fn_with_state(local_addr.port(), validate_local_request),
    );

    let shutdown_ct = ct.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_ct.cancelled().await })
            .await
    });

    Ok(HttpServerHandle {
        local_addr,
        ct,
        task,
    })
}

/// 起動中のHTTP MCPサーバーのハンドル。
pub struct HttpServerHandle {
    local_addr: SocketAddr,
    ct: CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl HttpServerHandle {
    /// 実際にバインドされたアドレス(`port = 0` 指定時の確認に使う)。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// graceful shutdown して停止を待つ。
    pub async fn shutdown(self) -> Result<()> {
        self.ct.cancel();
        self.task.await.map_err(|e| Error::Serve(e.to_string()))??;
        Ok(())
    }
}

/// Hostヘッダーがこのサーバー自身(127.0.0.1/localhost)を指しているか。
/// DNSリバインディング攻撃では攻撃者ドメインがHostに入るため、これで遮断する。
fn host_allowed(host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    host == "127.0.0.1"
        || host == "localhost"
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
}

/// Originヘッダーがローカル発か。ポートは任意(Tauri webviewや開発サーバーを考慮)。
fn origin_allowed(origin: &str) -> bool {
    let origin_lower = origin.to_ascii_lowercase();
    let Some(rest) = origin_lower
        .strip_prefix("http://")
        .or_else(|| origin_lower.strip_prefix("https://"))
    else {
        return false;
    };
    let host_port = rest.split('/').next().unwrap_or(rest);
    // 末尾の :ポート を剥がす([::1]のコロンと区別するため数字のみの場合に限る)
    let host = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host_port,
    };
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// DNSリバインディング対策のミドルウェア。Host/Originを検証し、不正なら403を返す。
///
/// このミドルウェアは実際にバインドしたポートを使用してHost/Originを検証し、ルータ全体 (つまり /mcp だけでなく)
/// に適用される。rmcp の StreamableHttpService も独自のポート非依存の Host チェックを持ち、
/// "Forbidden: Host header is not allowed" を返す別の403を発生させる可能性がある。
/// rmcp の allowed_origins はデフォルトで空であり、Origin 検証はこのミドルウェアで行われるため、
/// 403エラーが発生した場合は、異なる本文を持つこのミドルウェア経由かrmcp経由か確認する必要がある。
async fn validate_local_request(State(port): State<u16>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    // RFC 9112 §3.2.2 (absolute-form request-target) / RFC 9113 §8.3.1 (HTTP/2 :authority):
    // URIのauthorityが与えられている場合はそれが正であり、Hostヘッダーは無視・上書きしてはならない。
    // authorityが無い場合(HTTP/1.1のorigin-form)にのみHostヘッダーへフォールバックする。
    let host_ok = request
        .uri()
        .authority()
        .map(|a| a.as_str())
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()))
        .is_some_and(|h| host_allowed(h, port));
    let origin_ok = match headers.get(header::ORIGIN) {
        None => true,
        Some(v) => v.to_str().ok().is_some_and(origin_allowed),
    };
    if host_ok && origin_ok {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "forbidden: invalid Host or Origin header",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::{ClientHandler, ServiceExt};
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default, Clone)]
    struct TestClient;
    impl ClientHandler for TestClient {}

    /// Bounds a test body so a hung client/server interaction fails fast
    /// instead of hanging CI indefinitely.
    async fn with_test_timeout<F: std::future::Future>(fut: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(10), fut)
            .await
            .expect("test timed out")
    }

    #[tokio::test]
    async fn http_roundtrip_lists_and_calls_tools() {
        with_test_timeout(async {
            let backlog = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v2/users/myself"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 1, "userId": "admin", "name": "admin", "roleType": 1
                })))
                .mount(&backlog)
                .await;

            let config = crate::BacklogMcpConfig {
                base_url: backlog.uri(),
                api_key: "KEY".to_string(),
            };
            let handle = serve_http(config, 0).await.unwrap();
            assert_eq!(handle.local_addr().ip().to_string(), "127.0.0.1");

            let transport = StreamableHttpClientTransport::from_uri(format!(
                "http://{}/mcp",
                handle.local_addr()
            ));
            let client = TestClient.serve(transport).await.unwrap();

            let tools = client.list_tools(Default::default()).await.unwrap();
            let names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
            for expected in [
                "search_issues",
                "get_issue",
                "create_issue",
                "update_issue",
                "add_comment",
                "list_comments",
                "list_projects",
                "get_project",
                "list_users",
                "get_myself",
                "search_wikis",
                "count_wikis",
                "get_wiki",
                "create_wiki",
                "update_wiki",
                "search_documents",
                "count_documents",
                "document_tree",
                "get_document",
                "create_document",
                "list_document_comments",
                "download_issue_attachment",
                "download_wiki_attachment",
                "download_document_attachment",
                "list_notifications",
                "count_unread_notifications",
                "list_watchings",
                "count_watchings",
                "list_repositories",
                "search_pull_requests",
                "count_pull_requests",
                "get_pull_request",
                "list_pull_request_comments",
            ] {
                assert!(
                    names.contains(&expected.to_string()),
                    "missing {expected} in {names:?}"
                );
            }

            let result = client
                .call_tool(CallToolRequestParams::new("get_myself"))
                .await
                .unwrap();
            let text = serde_json::to_string(&result).unwrap();
            assert!(text.contains("admin"), "got: {text}");

            client.cancel().await.unwrap();
            handle.shutdown().await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn shutdown_stops_accepting_connections() {
        with_test_timeout(async {
            let config = crate::BacklogMcpConfig {
                base_url: "https://example.backlog.com".to_string(),
                api_key: "KEY".to_string(),
            };
            let handle = serve_http(config, 0).await.unwrap();
            let addr = handle.local_addr();
            handle.shutdown().await.unwrap();
            let connect = tokio::net::TcpStream::connect(addr).await;
            assert!(
                connect.is_err(),
                "server should stop accepting after shutdown"
            );
        })
        .await;
    }

    #[test]
    fn host_allowed_accepts_only_local_hosts() {
        assert!(host_allowed("127.0.0.1", 39150));
        assert!(host_allowed("127.0.0.1:39150", 39150));
        assert!(host_allowed("localhost", 39150));
        assert!(host_allowed("localhost:39150", 39150));
        // 別ポート・偽ホストは拒否
        assert!(!host_allowed("127.0.0.1:39151", 39150));
        assert!(!host_allowed("evil.example.com", 39150));
        assert!(!host_allowed("evil.example.com:39150", 39150));
        assert!(!host_allowed("localhost.evil.example.com:39150", 39150));
        assert!(!host_allowed("", 39150));
    }

    #[test]
    fn host_allowed_is_case_insensitive() {
        assert!(host_allowed("LOCALHOST", 39150));
        assert!(host_allowed("LocalHost:39150", 39150));
        // 大文字化しても偽ホストは依然拒否
        assert!(!host_allowed("LOCALHOST.EVIL.EXAMPLE.COM", 39150));
        assert!(!host_allowed("LOCALHOST.evil.example.com:39150", 39150));
    }

    #[test]
    fn origin_allowed_accepts_only_local_origins() {
        assert!(origin_allowed("http://127.0.0.1:5173"));
        assert!(origin_allowed("http://localhost"));
        assert!(origin_allowed("https://localhost:8080"));
        assert!(origin_allowed("http://[::1]:3000"));
        assert!(!origin_allowed("http://evil.example.com"));
        assert!(!origin_allowed("http://localhost.evil.example.com"));
        assert!(!origin_allowed("file://localhost"));
        assert!(!origin_allowed("null"));
        assert!(!origin_allowed(""));
    }

    #[test]
    fn origin_allowed_is_case_insensitive() {
        assert!(origin_allowed("http://LOCALHOST:5173"));
        assert!(origin_allowed("HTTP://localhost"));
        assert!(origin_allowed("HTTPS://LOCALHOST:8080"));
        // 大文字化しても偽オリジンは依然拒否
        assert!(!origin_allowed("http://LOCALHOST.EVIL.EXAMPLE.COM"));
        assert!(!origin_allowed("HTTP://localhost@evil.example.com"));
        assert!(!origin_allowed("HTTP://localhost.evil.example.com"));
    }

    #[tokio::test]
    async fn rejects_spoofed_host_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let config = crate::BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        };
        let handle = serve_http(config, 0).await.unwrap();
        let addr = handle.local_addr();

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: evil.example.com:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            addr.port()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 403"), "got: {response}");
        assert!(
            response.contains("forbidden: invalid Host or Origin header"),
            "got: {response}"
        );

        handle.shutdown().await.unwrap();
    }

    /// RFC 9112 §3.2.2: absolute-form のリクエストターゲットが与えられた場合、
    /// URIのauthorityが正であり、Hostヘッダーより優先される必要がある。
    /// (HTTP/2のRFC 9113 §8.3.1 における:authorityも同様。)
    /// Hostヘッダーが正当でも、authorityが偽装されていれば拒否しなければならない。
    #[tokio::test]
    async fn rejects_absolute_form_uri_with_spoofed_authority() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let config = crate::BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        };
        let handle = serve_http(config, 0).await.unwrap();
        let addr = handle.local_addr();

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST http://evil.example.com/mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            addr.port()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 403"), "got: {response}");

        handle.shutdown().await.unwrap();
    }

    /// Tauri webviewなどが送る形の Origin (127.0.0.1のHost + localhost:任意ポートのOrigin) は
    /// 許可され、403にならないこと。
    #[tokio::test]
    async fn allows_valid_tauri_webview_origin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let config = crate::BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        };
        let handle = serve_http(config, 0).await.unwrap();
        let addr = handle.local_addr();

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        })
        .to_string();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://localhost:1420\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            addr.port(),
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "middleware should allow valid Origin, expected HTTP/1.1 200, got: {response}"
        );
        assert!(
            response.to_ascii_lowercase().contains("mcp-session-id"),
            "response should contain mcp-session-id header, got: {response}"
        );

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_evil_origin_with_correct_host() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let config = crate::BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        };
        let handle = serve_http(config, 0).await.unwrap();
        let addr = handle.local_addr();

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://evil.example.com\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            addr.port()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 403"), "got: {response}");

        handle.shutdown().await.unwrap();
    }
}
