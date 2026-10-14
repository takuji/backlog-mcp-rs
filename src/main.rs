use backlog_mcp::{BacklogMcpConfig, serve_http, serve_stdio};

const DEFAULT_PORT: u16 = 39150;

#[derive(Debug, PartialEq)]
enum Mode {
    Stdio,
    Http { port: u16 },
}

fn parse_args(args: &[String]) -> Result<Mode, String> {
    let mut http = false;
    let mut port: Option<u16> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--http" => http = true,
            "--port" => {
                let value = iter.next().ok_or("--port requires a value")?;
                port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid port: {value}"))?,
                );
            }
            other => {
                return Err(format!(
                    "unknown argument: {other} (usage: backlog-mcp [--http [--port N]])"
                ));
            }
        }
    }
    match (http, port) {
        (true, port) => Ok(Mode::Http {
            port: port.unwrap_or(DEFAULT_PORT),
        }),
        (false, None) => Ok(Mode::Stdio),
        (false, Some(_)) => Err("--port requires --http".to_string()),
    }
}

fn env_var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("環境変数 {name} を設定してください");
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match parse_args(&args) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let config = BacklogMcpConfig {
        base_url: env_var("BACKLOG_BASE_URL"),
        api_key: env_var("BACKLOG_API_KEY"),
    };
    let result = match mode {
        Mode::Stdio => serve_stdio(config).await,
        Mode::Http { port } => run_http(config, port).await,
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run_http(config: BacklogMcpConfig, port: u16) -> backlog_mcp::Result<()> {
    let handle = serve_http(config, port).await?;
    eprintln!(
        "backlog-mcp listening on http://{}/mcp",
        handle.local_addr()
    );
    eprintln!("press Ctrl-C to stop");
    tokio::signal::ctrl_c()
        .await
        .map_err(backlog_mcp::Error::Io)?;
    handle.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_means_stdio() {
        assert_eq!(parse_args(&args(&[])).unwrap(), Mode::Stdio);
    }

    #[test]
    fn http_flag_uses_default_port() {
        assert_eq!(
            parse_args(&args(&["--http"])).unwrap(),
            Mode::Http { port: 39150 }
        );
    }

    #[test]
    fn http_with_port() {
        assert_eq!(
            parse_args(&args(&["--http", "--port", "40000"])).unwrap(),
            Mode::Http { port: 40000 }
        );
    }

    #[test]
    fn port_without_http_is_error() {
        assert!(parse_args(&args(&["--port", "40000"])).is_err());
    }

    #[test]
    fn unknown_flag_is_error() {
        assert!(parse_args(&args(&["--tcp"])).is_err());
        assert!(parse_args(&args(&["--http", "--port", "abc"])).is_err());
    }
}
