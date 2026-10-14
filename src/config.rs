/// backlog-mcp の設定。
#[derive(Clone)]
pub struct BacklogMcpConfig {
    /// BacklogスペースのベースURL(`https://<space>.backlog.com`)。
    pub base_url: String,
    /// BacklogのAPIキー。
    pub api_key: String,
}

impl std::fmt::Debug for BacklogMcpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacklogMcpConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"REDACTED")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_constructible_and_cloneable() {
        let config = BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "KEY".to_string(),
        };
        let cloned = config.clone();
        assert_eq!(cloned.base_url, "https://example.backlog.com");
    }

    #[test]
    fn debug_output_does_not_leak_api_key() {
        let config = BacklogMcpConfig {
            base_url: "https://example.backlog.com".to_string(),
            api_key: "SUPER_SECRET".to_string(),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("SUPER_SECRET"), "got: {debug}");
    }
}
