/// backlog-mcp のエラー型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Backlogクライアントの生成に失敗(ベースURL不正など)。
    #[error("backlog client error: {0}")]
    Client(#[from] backlog_api_rs::Error),

    /// TCPバインドなどのIOエラー。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// MCPサーバーの起動・実行エラー。
    /// rmcpのエラー型はトランスポートごとにジェネリックなため文字列に落とす。
    #[error("mcp serve error: {0}")]
    Serve(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_includes_context() {
        let err = Error::Serve("boom".to_string());
        assert_eq!(err.to_string(), "mcp serve error: boom");

        let io = Error::from(std::io::Error::other("bind failed"));
        assert!(io.to_string().contains("bind failed"));
    }
}
