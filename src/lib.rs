//! Backlogの課題を読み書きするMCPサーバー機能ライブラリ。
//!
//! Tauriアプリへの組み込み(HTTP)と、単体バイナリ(stdio / HTTP)の両方で使える。
//!
//! ```no_run
//! use backlog_mcp::{BacklogMcpConfig, serve_http};
//!
//! # async fn example() -> backlog_mcp::Result<()> {
//! let config = BacklogMcpConfig {
//!     base_url: "https://example.backlog.com".to_string(),
//!     api_key: std::env::var("BACKLOG_API_KEY").unwrap(),
//! };
//! // 127.0.0.1:39150 の /mcp で待ち受ける
//! let handle = serve_http(config, 39150).await?;
//! println!("listening on http://{}/mcp", handle.local_addr());
//! // アプリ終了時に
//! handle.shutdown().await?;
//! # Ok(())
//! # }
//! ```

mod config;
mod error;
mod serve;
pub(crate) mod server;
pub(crate) mod tools;

pub use config::BacklogMcpConfig;
pub use error::{Error, Result};
pub use serve::{HttpServerHandle, serve_http, serve_stdio};
pub use server::BacklogMcpServer;
