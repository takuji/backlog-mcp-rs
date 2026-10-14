pub(crate) mod attachments;
pub(crate) mod documents;
pub(crate) mod git;
pub(crate) mod issues;
pub(crate) mod notifications;
pub(crate) mod reference;
pub(crate) mod watchings;
pub(crate) mod wikis;

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, ContentBlock};

/// 値をpretty JSONにして成功レスポンスを作る。
pub(crate) fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serialize error: {e}"), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Backlog APIのエラーをツールエラー(is_error=true)に変換する。
/// AIがリトライ判断できるようエラーコード名とメッセージを含める。
pub(crate) fn backlog_error(err: backlog_api_rs::Error) -> CallToolResult {
    let message = match &err {
        backlog_api_rs::Error::Api { status, errors } => {
            let details: Vec<String> = errors
                .iter()
                .map(|e| format!("{:?}: {}", e.code, e.message))
                .collect();
            format!(
                "Backlog API error (status {status}): {}",
                details.join("; ")
            )
        }
        other => other.to_string(),
    };
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// `YYYY-MM-DD` 形式の日付をパースする。失敗時はツールエラーを返す。
pub(crate) fn parse_date(s: &str) -> Result<chrono::NaiveDate, CallToolResult> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "invalid date: {s} (expected YYYY-MM-DD)"
        ))])
    })
}

/// ツールの出力(pretty JSON のテキストコンテント)を値としてパースする。
///
/// `text.contains("...")` では、その文字列が**別のキーの下**にあっても通って
/// しまい、フィールドの取り違えを検出できない。`tools` 配下の複数モジュールの
/// テストから使う共有ヘルパーなので、各ファイルの `mod tests` ではなくここに置く
/// (`reference.rs` の `mod tests` は非公開のため、そこに置くと他モジュールから
/// 到達できない)。
#[cfg(test)]
pub(crate) fn output_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result
        .content
        .iter()
        .find_map(|c| c.as_text().map(|t| t.text.clone()))
        .expect("テキストコンテントが無い");
    serde_json::from_str(&text).expect("出力がJSONとして読めない")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize)]
    struct Sample {
        name: String,
    }

    #[test]
    fn json_result_serializes_to_text_content() {
        let result = json_result(&Sample {
            name: "hello".to_string(),
        })
        .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("hello"), "got: {text}");
    }

    #[test]
    fn backlog_error_includes_code_and_message() {
        let err = backlog_api_rs::Error::Api {
            status: 404,
            errors: vec![],
        };
        let result = backlog_error(err);
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("404"), "got: {text}");
    }

    #[test]
    fn parse_date_accepts_iso_and_rejects_garbage() {
        assert!(parse_date("2026-07-27").is_ok());
        let err = parse_date("July 27").unwrap_err();
        assert_eq!(err.is_error, Some(true));
    }
}
