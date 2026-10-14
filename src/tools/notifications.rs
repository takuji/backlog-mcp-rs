use backlog_api_rs::api::GetNotificationsParams;
use backlog_api_rs::models::NotificationReason;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};

use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

/// 理由コードの日本語の説明文。
///
/// ライブラリは `NotificationReason` を返すだけで、表示文言は持たない
/// (表示は提示層の責務)。未知のコードは**コード番号を文言に含める**。
/// 「その他」だけに丸めると調査の手がかりが消える。
///
/// **この `match` に `_ =>` のワイルドカード腕を足さないこと。**
/// `NotificationReason` は `#[non_exhaustive]` を意図的に付けていないので、
/// backlog-api-rs 側に新しい理由コードが増えると、この `match` が
/// **コンパイルエラーになる**ことでラベルの追記を強制する仕組みになっている。
/// ワイルドカード腕を足すとコンパイルは通ってしまい、新しい理由コードは
/// 全部無言で「その他の通知」に丸められる(コード自体は
/// `NotificationReason::Other(u32)` に残るので実害はすぐには出ないが、
/// 専用の日本語ラベルを書くきっかけが永久に失われる)。
///
/// `#[deny(unreachable_patterns)]` は上の禁止を機械的に補強するためのもの。
/// このリポジトリには CI も `#![deny(warnings)]` も無いため、ワイルドカード腕を
/// 足しても既定では警告が出るだけで誰も気づかない。`deny` にしておけば
/// ハードエラーになり、無効化するには `#[allow]` をこのコメントの隣に
/// 明示的に書く必要が生じる。
#[deny(unreachable_patterns)]
fn reason_description(reason: NotificationReason) -> String {
    match reason {
        NotificationReason::AssignedToIssue => "課題の担当者に設定されました".to_string(),
        NotificationReason::IssueCommented => "課題にコメントがありました".to_string(),
        NotificationReason::IssueCreated => "課題が追加されました".to_string(),
        NotificationReason::IssueUpdated => "課題が更新されました".to_string(),
        NotificationReason::FileAdded => "ファイルが追加されました".to_string(),
        NotificationReason::ProjectUserAdded => "プロジェクトメンバーに追加されました".to_string(),
        NotificationReason::AssignedToPullRequest => {
            "プルリクエストの担当者に設定されました".to_string()
        }
        NotificationReason::PullRequestCommented => {
            "プルリクエストにコメントがありました".to_string()
        }
        NotificationReason::PullRequestAdded => "プルリクエストが追加されました".to_string(),
        NotificationReason::PullRequestUpdated => "プルリクエストが更新されました".to_string(),
        NotificationReason::DocumentCommented => "ドキュメントにコメントがありました".to_string(),
        NotificationReason::DocumentCommentReplied => {
            "ドキュメントのコメントに返信がありました".to_string()
        }
        NotificationReason::DocumentMentioned => "ドキュメントでメンションされました".to_string(),
        // 16 と 18 はどちらも課題の一括登録に伴う通知だが、受け取る理由が違う。
        // サーバー実装では 16 が一括登録された課題の担当者に、18 がお知らせ先として
        // 明示的に指定されたユーザーに送られる(担当者は 18 の対象から除外される)。
        NotificationReason::IssueMultiCreated => {
            "課題が一括登録され、担当者になりました".to_string()
        }
        NotificationReason::IssueMultiCreatedNotified => {
            "課題の一括登録のお知らせ先に指定されました".to_string()
        }
        NotificationReason::Other(code) => {
            format!("その他の通知(コード {code})")
        }
    }
}

/// コメント本文の抜粋を作る。
///
/// AIのコンテキストを守るため全文は返さない(全文が必要なら `get_issue` や
/// `list_comments` を使う)。Tauriアプリと同じ規則:
/// trim → 改行を空白に置換 → 80文字で切り、超えていたら末尾に `…`。
/// 結果が空なら `None`。
fn comment_snippet(content: Option<&str>) -> Option<String> {
    const MAX_CHARS: usize = 80;
    // CRLF はまず1つの区切りとしてまとめて置換し、その後に残る単独の \r / \n を
    // 置換する(先に個別置換すると CRLF が空白2つに割れて二重スペースになる)。
    // Backlogのweb画面から投稿されたコメントはCRLFで来るため、\n だけを見ると
    // \r が末尾に残ってJSONに `\r` のまま漏れる。
    let text = content?
        .trim()
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ");
    if text.is_empty() {
        return None;
    }
    let truncated: String = text.chars().take(MAX_CHARS).collect();
    if text.chars().count() > MAX_CHARS {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

/// 通知1件の出力。
#[derive(serde::Serialize)]
pub(crate) struct NotificationOut {
    id: u64,
    /// 生の理由コード。
    reason: u32,
    /// 理由の説明(日本語)。
    reason_description: String,
    already_read: bool,
    resource_already_read: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_name: Option<String>,
    /// プルリクエストの通知の場合のプロジェクトキー。`repository_name` /
    /// `pull_request_number` とあわせて get_pull_request に渡せる。
    ///
    /// **これが無いと通知からPRへたどれない。** get_pull_request は
    /// `{project_key, repository_name, number}` の3つを要求するが、通知が
    /// 持つ `issue_key` はPRに紐づかない通知(実データで過半数)では
    /// 使えない。`project_name`(表示名)を代わりに渡しても一致するとは
    /// 限らず、Backlogはプロジェクト表示名の一意性を保証しない。
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<String>,
    /// 課題への通知の場合。get_issue に渡せる。
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_summary: Option<String>,
    /// プルリクエストへの通知の場合(専用ツールは無いので文字情報のみ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_request_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_request_summary: Option<String>,
    /// プルリクエストの通知の場合のリポジトリ名。get_pull_request に渡せる。
    #[serde(skip_serializing_if = "Option::is_none")]
    repository_name: Option<String>,
    /// ドキュメントへの通知(理由 14 / 15 / 17)の場合。get_document に渡せる。
    #[serde(skip_serializing_if = "Option::is_none")]
    document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_title: Option<String>,
    /// コメント本文の抜粋(80文字)。
    #[serde(skip_serializing_if = "Option::is_none")]
    comment_snippet: Option<String>,
    created: String,
}

impl From<&backlog_api_rs::models::Notification> for NotificationOut {
    fn from(n: &backlog_api_rs::models::Notification) -> Self {
        // 通知の種類によってコメントが入るフィールドが違うため、優先順に見ていく。
        // 課題 -> プルリクエスト -> ドキュメント -> ドキュメントのコメントへの返信。
        // 各フィールドの中では `plain`(プレーンテキスト)を `content`(書式付き)
        // より優先する。ドキュメントのコメントは両方を持ち、書式付きのほうは
        // マークアップを含むため抜粋には向かない。
        let snippet = [
            n.comment.as_ref(),
            n.pull_request_comment.as_ref(),
            n.document_comment.as_ref(),
            n.document_comment_reply.as_ref(),
        ]
        .into_iter()
        .flatten()
        .find_map(|c| {
            comment_snippet(c.plain.as_deref()).or_else(|| comment_snippet(c.content.as_deref()))
        });
        Self {
            id: n.id,
            reason: n.reason,
            reason_description: reason_description(n.reason_kind()),
            already_read: n.already_read,
            resource_already_read: n.resource_already_read,
            sender_name: n.sender.as_ref().map(|u| u.name.clone()),
            project_name: n.project.as_ref().map(|p| p.name.clone()),
            project_key: n.project.as_ref().map(|p| p.project_key.clone()),
            issue_key: n.issue.as_ref().map(|i| i.issue_key.clone()),
            issue_summary: n.issue.as_ref().map(|i| i.summary.clone()),
            pull_request_number: n.pull_request.as_ref().and_then(|p| p.number),
            pull_request_summary: n.pull_request.as_ref().and_then(|p| p.summary.clone()),
            repository_name: n.repository.as_ref().and_then(|r| r.name.clone()),
            document_id: n.document.as_ref().and_then(|d| d.id.clone()),
            document_title: n.document.as_ref().and_then(|d| d.title.clone()),
            comment_snippet: snippet,
            created: n.created.to_rfc3339(),
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListNotificationsParams {
    /// 取得件数(1-100、省略時20)
    pub count: Option<u64>,
    /// このIDより新しい通知に絞る
    pub min_id: Option<u64>,
    /// このIDより古い通知に絞る(古い通知をたどるページングに使う)
    pub max_id: Option<u64>,
    /// 送信者のユーザーIDで絞る(list_users で取得できる)
    pub sender_id: Option<u64>,
}

#[derive(serde::Serialize)]
struct UnreadCountOut {
    count: u64,
}

/// 読めなかった通知。
#[derive(serde::Serialize)]
pub(crate) struct SkippedOut {
    /// 応答配列の何番目か(0始まり)
    pub index: usize,
    pub id: Option<String>,
    /// なぜ読めなかったか(serde のメッセージ)
    pub reason: String,
}

/// `list_notifications` の出力。
///
/// `skipped` は**空のときは出さない**。通常時の出力を増やさず、
/// 存在すること自体を信号にする。
#[derive(serde::Serialize)]
pub(crate) struct NotificationsOut {
    pub notifications: Vec<NotificationOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedOut>,
}

#[tool_router(router = notifications_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "自分に届いた通知(お知らせ)を新しい順に取得する。課題への通知には issue_key が入るので get_issue に渡せ、ドキュメントへの通知には document_id が入るので get_document に渡せる。reason_description に通知の理由が日本語で入る。未読の絞り込みはAPI側に無いため、未読だけ知りたい場合は各通知の already_read を見て自分で判定すること(件数だけでよければ count_unread_notifications を使う)。プルリクエストの通知には project_key と repository_name と pull_request_number が入るので、この3つをそのまま get_pull_request に渡せる(project_key が無いと get_pull_request は呼べない)。応答は {notifications, skipped} の形で、skipped は想定外の形で読めなかった通知がある場合にだけ現れる。skipped があるときは、その件数だけ通知が欠けていることを利用者に伝えること。"
    )]
    pub(crate) async fn list_notifications(
        &self,
        Parameters(params): Parameters<ListNotificationsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut api_params = GetNotificationsParams::new();
        if let Some(count) = params.count {
            api_params = api_params.count(count);
        }
        if let Some(min_id) = params.min_id {
            api_params = api_params.min_id(min_id);
        }
        if let Some(max_id) = params.max_id {
            api_params = api_params.max_id(max_id);
        }
        if let Some(sender_id) = params.sender_id {
            api_params = api_params.sender_id(sender_id);
        }
        match self.client.notifications().list(api_params).await {
            Ok(list) => {
                let out = NotificationsOut {
                    notifications: list.items.iter().map(NotificationOut::from).collect(),
                    skipped: list
                        .skipped
                        .iter()
                        .map(|s| SkippedOut {
                            index: s.index,
                            id: s.id.clone(),
                            reason: s.reason.clone(),
                        })
                        .collect(),
                };
                json_result(&out)
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "未読の通知件数を返す。通知そのものが未読のものを数える(対象の課題を直接開いて読んだかは条件に含めない)。"
    )]
    pub(crate) async fn count_unread_notifications(&self) -> Result<CallToolResult, McpError> {
        match self.client.notifications().unread_count().await {
            Ok(count) => json_result(&UnreadCountOut { count }),
            Err(e) => Ok(backlog_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BacklogMcpConfig;
    use crate::server::BacklogMcpServer;
    use crate::tools::output_json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `ListNotificationsParams` は `Default` を derive していないため、
    /// 既存テストと同じ「全フィールド `None`」の作り方で代用する。
    fn no_params() -> Parameters<ListNotificationsParams> {
        Parameters(ListNotificationsParams {
            count: None,
            min_id: None,
            max_id: None,
            sender_id: None,
        })
    }

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

    /// 名前付きバリアント13種すべての文言をピン留めする(`Other` は別テスト)。
    ///
    /// 個別の `assert_eq!` だと一部の腕しか実行されず、既存の腕の文言を
    /// 別の腕のものと取り違えても(コピペミス)気づけない。テーブル駆動に
    /// することで13腕すべてを1回で検証する。
    #[test]
    fn describes_known_reasons_in_japanese() {
        for (reason, expected) in [
            (
                NotificationReason::AssignedToIssue,
                "課題の担当者に設定されました",
            ),
            (
                NotificationReason::IssueCommented,
                "課題にコメントがありました",
            ),
            (NotificationReason::IssueCreated, "課題が追加されました"),
            (NotificationReason::IssueUpdated, "課題が更新されました"),
            (NotificationReason::FileAdded, "ファイルが追加されました"),
            (
                NotificationReason::ProjectUserAdded,
                "プロジェクトメンバーに追加されました",
            ),
            (
                NotificationReason::AssignedToPullRequest,
                "プルリクエストの担当者に設定されました",
            ),
            (
                NotificationReason::PullRequestCommented,
                "プルリクエストにコメントがありました",
            ),
            (
                NotificationReason::PullRequestAdded,
                "プルリクエストが追加されました",
            ),
            (
                NotificationReason::PullRequestUpdated,
                "プルリクエストが更新されました",
            ),
            (
                NotificationReason::DocumentCommented,
                "ドキュメントにコメントがありました",
            ),
            (
                NotificationReason::DocumentCommentReplied,
                "ドキュメントのコメントに返信がありました",
            ),
            (
                NotificationReason::DocumentMentioned,
                "ドキュメントでメンションされました",
            ),
            (
                NotificationReason::IssueMultiCreated,
                "課題が一括登録され、担当者になりました",
            ),
            (
                NotificationReason::IssueMultiCreatedNotified,
                "課題の一括登録のお知らせ先に指定されました",
            ),
        ] {
            assert_eq!(reason_description(reason), expected, "reason: {reason:?}");
        }
    }

    /// 未知のコードは説明文にコード番号を含めること。
    ///
    /// 理由コードは後から増える(14 / 15 / 17 がその実績)。「その他」だけに
    /// 丸めるとコードが失われ、次に増えたときの調査ができない。
    #[test]
    fn unknown_reason_description_includes_the_code() {
        let text = reason_description(NotificationReason::Other(21));
        assert!(text.contains("21"), "got: {text}");
        let text_nine = reason_description(NotificationReason::Other(9));
        assert!(text_nine.contains('9'), "got: {text_nine}");
    }

    #[test]
    fn snippet_collapses_newlines_and_trims() {
        assert_eq!(
            comment_snippet(Some("  一行目\n二行目  ")),
            Some("一行目 二行目".to_string())
        );
    }

    /// CRLF は空白1つにまとめ、単独の `\r` / `\n` も取り除くこと。
    ///
    /// Backlogのweb画面から投稿されたコメントはCRLFで来る。`\n` だけを
    /// 置換すると `\r` がそのままJSONに漏れる。
    #[test]
    fn snippet_collapses_crlf_and_lone_cr() {
        assert_eq!(
            comment_snippet(Some("one\r\ntwo")),
            Some("one two".to_string())
        );
        assert_eq!(
            comment_snippet(Some("one\rtwo")),
            Some("one two".to_string())
        );
        // CRLF と単独の \n が混在するケース。CRLF が空白2つに割れないこと。
        assert_eq!(
            comment_snippet(Some("one\r\ntwo\nthree")),
            Some("one two three".to_string())
        );
    }

    #[test]
    fn snippet_returns_none_for_empty_or_missing() {
        assert_eq!(comment_snippet(None), None);
        assert_eq!(comment_snippet(Some("")), None);
        assert_eq!(comment_snippet(Some("   \n  ")), None);
    }

    /// 80文字で切り、超過時だけ `…` を付けること。
    ///
    /// 境界のちょうど80文字では `…` を付けない。文字数は**バイトではなく
    /// 文字**で数える(日本語のコメントが途中で壊れないため)。
    #[test]
    fn snippet_truncates_at_eighty_characters() {
        let exactly_eighty: String = "あ".repeat(80);
        let got = comment_snippet(Some(&exactly_eighty)).unwrap();
        assert_eq!(got.chars().count(), 80);
        assert!(!got.ends_with('…'), "got: {got}");

        let eighty_one: String = "あ".repeat(81);
        let got = comment_snippet(Some(&eighty_one)).unwrap();
        assert!(got.ends_with('…'), "got: {got}");
        // 80文字 + 省略記号1文字
        assert_eq!(got.chars().count(), 81);
    }

    /// 課題への通知で issue_key が入り、抜粋が作られること。
    #[tokio::test]
    async fn list_notifications_returns_issue_key_and_snippet() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 299, "alreadyRead": false, "reason": 2,
                      "resourceAlreadyRead": false,
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
                      "comment": { "id": 7007, "content": "hoge" },
                      "created": "2013-10-31T06:58:59Z" }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("TEST2-17"), "got: {text}");
        assert!(text.contains("課題にコメントがありました"), "got: {text}");
        assert!(text.contains("hoge"), "got: {text}");
    }

    /// ネストしたリソースが無い通知(理由 6)でもツールがエラーにならず、
    /// issue_key が省略されること。
    ///
    /// openapi.yaml がこれらを required としているため、モデルを信じて
    /// 実装していたら一覧が丸ごと取れなくなる箇所。
    #[tokio::test]
    async fn list_notifications_handles_notification_without_issue() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 300, "alreadyRead": false, "reason": 6,
                      "resourceAlreadyRead": false,
                      "project": null, "issue": null, "comment": null,
                      "pullRequest": null, "pullRequestComment": null,
                      "created": "2026-07-30T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(
            text.contains("プロジェクトメンバーに追加されました"),
            "got: {text}"
        );
        // 課題が無いのでキーは省略される
        assert!(!text.contains("issue_key"), "got: {text}");
    }

    /// コメントが無くプルリクエストへのコメントがある場合に抜粋が作られること。
    #[tokio::test]
    async fn list_notifications_uses_pull_request_comment_for_snippet() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 301, "alreadyRead": false, "reason": 11,
                      "resourceAlreadyRead": false,
                      "comment": null,
                      "pullRequest": { "number": 12, "summary": "fix things" },
                      "pullRequestComment": { "id": 5, "content": "レビューしました" },
                      "created": "2026-07-30T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("レビューしました"), "got: {text}");
        assert!(text.contains("fix things"), "got: {text}");
    }

    /// `comment` と `pull_request_comment` の両方が非nullの場合、`comment` が
    /// 優先されること。
    ///
    /// 設計上 `comment` が優先されるべきだが、これまでのフィクスチャは
    /// どちらか一方しか持たなかったため、`.or_else` の順序を入れ替えて
    /// `pull_request_comment` を優先させても全テストが通っていた。
    /// 両方を異なる内容で与えることで優先順位を実際に検証する。
    #[tokio::test]
    async fn list_notifications_prefers_comment_over_pull_request_comment() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 302, "alreadyRead": false, "reason": 2,
                      "resourceAlreadyRead": false,
                      "comment": { "id": 1, "content": "課題コメントが優先されるはず" },
                      "pullRequest": { "number": 12, "summary": "fix things" },
                      "pullRequestComment": { "id": 5, "content": "PRコメントは劣後するはず" },
                      "created": "2026-07-30T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("課題コメントが優先されるはず"), "got: {text}");
        assert!(!text.contains("PRコメントは劣後するはず"), "got: {text}");
    }

    /// ドキュメントへの通知で document_id / document_title が入り、
    /// 抜粋が `documentComment` から作られること。
    ///
    /// ドキュメント関連の通知は `comment` を使わず(実APIで理由17を観測)、
    /// 専用の `document` / `documentComment` / `documentCommentReply` を使う。
    /// これらは openapi にも公式APIリファレンスにも記載が無い。
    /// `document_id` は `get_document` にそのまま渡せる。
    #[tokio::test]
    async fn list_notifications_returns_document_fields_and_snippet() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 400, "alreadyRead": false, "reason": 14,
                      "resourceAlreadyRead": false,
                      "comment": null,
                      "document": { "id": "019343454047a1adb2113d7792bb4351",
                                    "title": "設計メモ" },
                      "documentComment": { "id": "cmt-1",
                                           "content": "<p>書式付きは劣後するはず</p>",
                                           "plain": "プレーン本文が優先されるはず" },
                      "created": "2026-07-31T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // フィールド名と値を対にして固定する(値だけだと入れ替えを検出できない)
        assert!(
            text.contains(r#"\"document_id\": \"019343454047a1adb2113d7792bb4351\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"document_title\": \"設計メモ\""#),
            "got: {text}"
        );
        assert!(
            text.contains("ドキュメントにコメントがありました"),
            "got: {text}"
        );
        // plain が content より優先されること
        assert!(text.contains("プレーン本文が優先されるはず"), "got: {text}");
        assert!(!text.contains("書式付きは劣後するはず"), "got: {text}");
    }

    /// ドキュメントのコメントへの返信(理由 15)でも抜粋が作られること。
    ///
    /// `documentCommentReply` は抜粋の優先順の最後にあり、これまで
    /// どのフィクスチャも持っていなかったため無検証だった。
    #[tokio::test]
    async fn list_notifications_uses_document_comment_reply_for_snippet() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 401, "alreadyRead": false, "reason": 15,
                      "resourceAlreadyRead": false,
                      "comment": null, "documentComment": null,
                      "document": { "id": "doc-9", "title": "議事録" },
                      "documentCommentReply": { "id": "rep-1", "plain": "返信の本文" },
                      "created": "2026-07-31T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("返信の本文"), "got: {text}");
        assert!(
            text.contains("ドキュメントのコメントに返信がありました"),
            "got: {text}"
        );
    }

    /// ドキュメントを持たない通知では document_id / document_title が
    /// 出力から省かれること。
    #[tokio::test]
    async fn list_notifications_omits_document_fields_when_absent() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 402, "alreadyRead": false, "reason": 6,
                      "resourceAlreadyRead": false,
                      "created": "2026-07-31T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("document_id"), "got: {text}");
        assert!(!text.contains("document_title"), "got: {text}");
    }

    /// MCPパラメータ(count/min_id/max_id/sender_id)がすべて `GetNotificationsParams`
    /// のビルダーへ正しく渡ること。
    ///
    /// 4つの値をすべて異なる数にしておくことで、`min_id`/`max_id` の取り違え
    /// (パラメータ名の入れ替え)や、いずれかのパラメータの取りこぼし、
    /// `sender_id` を `count` に渡してしまうような誤配線を検出できる。
    ///
    /// クエリは `query_param` による存在確認だけでなく、実際に届いた
    /// クエリ集合を厳密に比較する。`order` はMCP層に意図的に露出していない
    /// パラメータであり(設計上の決定)、`query_param` の存在確認だけでは
    /// ツール実装が勝手に `.order(Order::Asc)` を足しても検出できないため。
    #[tokio::test]
    async fn list_notifications_threads_all_params_to_the_query() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: Some(3),
                min_id: Some(1),
                max_id: Some(2),
                sender_id: Some(4),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        let pairs = captured_query_pairs(&backlog, "/api/v2/notifications").await;
        assert_eq!(
            sorted_pairs(pairs),
            sorted_pairs(vec![
                ("count".to_string(), "3".to_string()),
                ("minId".to_string(), "1".to_string()),
                ("maxId".to_string(), "2".to_string()),
                ("senderId".to_string(), "4".to_string()),
            ])
        );
    }

    /// `NotificationOut` の各フィールドが正しいソースから正しい値で埋まること。
    ///
    /// `already_read` と `resource_already_read` は意味が異なる隣接フィールドの
    /// ため、フィクスチャで意図的に異なる値にして取り違えを検出できるようにする。
    /// `issue_summary` も `issue_key` と異なる文字列にして、summaryがkeyから
    /// 供給されるような誤配線を検出する。
    #[tokio::test]
    async fn notification_out_pins_each_field_without_swaps() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r##"[{ "id": 555, "alreadyRead": true, "reason": 2,
                      "resourceAlreadyRead": false,
                      "project": { "id": 9, "projectKey": "PROJ9",
                                   "name": "プロジェクトきゅう",
                                   "chartEnabled": true, "subtaskingEnabled": true,
                                   "projectLeaderCanEditProjectLeader": false,
                                   "useWiki": true, "useFileSharing": true,
                                   "useWikiTreeView": true,
                                   "useOriginalImageSizeAtWiki": false,
                                   "textFormattingRule": "markdown", "archived": false,
                                   "displayOrder": 1, "useDevAttributes": true },
                      "issue": { "id": 4531, "projectId": 9, "issueKey": "PROJ9-42",
                                 "keyId": 42,
                                 "issueType": { "id": 7, "projectId": 9, "name": "Bug",
                                                "color": "#990000", "displayOrder": 0 },
                                 "summary": "サマリー本文はキーと異なる",
                                 "status": { "id": 1, "projectId": 9, "name": "Open",
                                             "color": "#ed8077", "displayOrder": 1000 },
                                 "createdUser": { "id": 1, "userId": "admin",
                                                  "name": "admin", "roleType": 1 },
                                 "created": "2013-04-23T07:38:59Z" },
                      "pullRequest": { "number": 77, "summary": "PR概要" },
                      "sender": { "id": 2, "userId": "eguchi", "name": "eguchi-distinct",
                                  "roleType": 2 },
                      "created": "2020-01-02T03:04:05Z" }]"##,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("\\\"id\\\": 555"), "got: {text}");
        assert!(text.contains("\\\"reason\\\": 2"), "got: {text}");
        assert!(text.contains("\\\"already_read\\\": true"), "got: {text}");
        assert!(
            text.contains("\\\"resource_already_read\\\": false"),
            "got: {text}"
        );
        assert!(text.contains("eguchi-distinct"), "got: {text}");
        assert!(text.contains("プロジェクトきゅう"), "got: {text}");
        assert!(text.contains("PROJ9-42"), "got: {text}");
        assert!(text.contains("サマリー本文はキーと異なる"), "got: {text}");
        assert!(
            text.contains("\\\"pull_request_number\\\": 77"),
            "got: {text}"
        );
        assert!(text.contains("2020-01-02T03:04:05"), "got: {text}");
    }

    /// プルリクエストの通知で `repository_name` が返ること。
    ///
    /// **これが無いと通知からPRへたどれない。** プルリクエストのエンドポイントは
    /// プロジェクトキーとリポジトリ名を要求するため、`repository_name` が
    /// `get_pull_request` への導線になる。
    #[tokio::test]
    async fn list_notifications_returns_repository_name() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 500, "alreadyRead": false, "reason": 11,
                      "resourceAlreadyRead": false,
                      "repository": { "id": 3, "name": "example-repo" },
                      "pullRequest": { "number": 12, "summary": "設定を見直す" },
                      "pullRequestComment": { "id": 5, "content": "レビューしました" },
                      "created": "2026-08-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(
            text.contains(r#"\"repository_name\": \"example-repo\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"pull_request_number\": 12"#),
            "got: {text}"
        );
        assert!(
            text.contains("プルリクエストにコメントがありました"),
            "got: {text}"
        );
    }

    /// リポジトリを持たない通知では `repository_name` が省かれること。
    #[tokio::test]
    async fn list_notifications_omits_repository_name_when_absent() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 501, "alreadyRead": false, "reason": 6,
                      "resourceAlreadyRead": false,
                      "created": "2026-08-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("repository_name"), "got: {text}");
    }

    /// プルリクエストの通知で `project_key` が返ること。
    ///
    /// **これが無いと `get_pull_request` を呼べない。** `get_pull_request` は
    /// `{project_key, repository_name, number}` の3つを要求するが、これまで
    /// `NotificationOut` は `project_name`(表示名)しか返していなかった。
    /// `issue_key` はPRに紐づかない通知(実データで過半数)では使えず、
    /// `project_name` を代わりに渡しても一致するとは限らない
    /// (Backlogはプロジェクト表示名の一意性を保証しない)。
    #[tokio::test]
    async fn list_notifications_returns_project_key_for_pull_request_notification() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 900, "alreadyRead": false, "reason": 11,
                      "resourceAlreadyRead": false,
                      "project": { "id": 9, "projectKey": "PROJ",
                                   "name": "Backlog開発",
                                   "chartEnabled": true, "subtaskingEnabled": true,
                                   "projectLeaderCanEditProjectLeader": false,
                                   "useWiki": true, "useFileSharing": true,
                                   "useWikiTreeView": true,
                                   "useOriginalImageSizeAtWiki": false,
                                   "textFormattingRule": "markdown", "archived": false,
                                   "displayOrder": 1, "useDevAttributes": true },
                      "repository": { "id": 3, "name": "backlog-scala" },
                      "pullRequest": { "number": 12, "summary": "設定を見直す" },
                      "pullRequestComment": { "id": 5, "content": "レビューしました" },
                      "created": "2026-08-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // フィールド名と値を対にして固定する(project_name との入れ替えを検出するため)
        assert!(text.contains(r#"\"project_key\": \"PROJ\""#), "got: {text}");
        assert!(
            text.contains(r#"\"repository_name\": \"backlog-scala\""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"\"pull_request_number\": 12"#),
            "got: {text}"
        );
    }

    /// プロジェクトを持たない通知では `project_key` が省かれること。
    #[tokio::test]
    async fn list_notifications_omits_project_key_when_project_is_absent() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 901, "alreadyRead": false, "reason": 6,
                      "resourceAlreadyRead": false,
                      "created": "2026-08-01T00:00:00Z" }]"#,
                "application/json",
            ))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server
            .list_notifications(Parameters(super::ListNotificationsParams {
                count: None,
                min_id: None,
                max_id: None,
                sender_id: None,
            }))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("project_key"), "got: {text}");
    }

    /// 正常時は skipped が出力に現れないこと。
    ///
    /// 通常時の出力を増やさず、`skipped` が存在すること自体を信号にする。
    #[tokio::test]
    async fn list_notifications_omits_skipped_when_empty() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[{ "id": 1, "alreadyRead": false, "reason": 2,
                      "resourceAlreadyRead": false, "created": "2026-07-30T07:55:07Z" }]"#,
                "application/json",
            ))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.list_notifications(no_params()).await.unwrap();
        let out = output_json(&result);
        assert_eq!(out["notifications"].as_array().unwrap().len(), 1);
        assert!(out.get("skipped").is_none(), "got: {out}");
    }

    /// 壊れた要素があると skipped に index / id / reason が入ること。
    #[tokio::test]
    async fn list_notifications_reports_the_skipped_item() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"[
                  { "id": 1, "alreadyRead": false, "reason": 2,
                    "resourceAlreadyRead": false, "created": "2026-07-30T07:55:07Z" },
                  { "id": 99, "alreadyRead": false }
                ]"#,
                "application/json",
            ))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let out = output_json(&server.list_notifications(no_params()).await.unwrap());
        assert_eq!(out["notifications"].as_array().unwrap().len(), 1);
        assert_eq!(out["notifications"][0]["id"], 1);
        let skipped = out["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["index"], 1);
        assert_eq!(skipped[0]["id"], "99");
        assert!(
            skipped[0]["reason"].as_str().unwrap().contains("reason"),
            "serde のメッセージが入っていない: {}",
            skipped[0]["reason"]
        );
    }

    /// 出力のトップレベルがオブジェクトであること(配列から変わった)。
    #[tokio::test]
    async fn list_notifications_returns_an_object_not_an_array() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let out = output_json(&server.list_notifications(no_params()).await.unwrap());
        assert!(out.is_object(), "got: {out}");
        assert!(out["notifications"].is_array());
    }

    #[tokio::test]
    async fn count_unread_notifications_returns_the_count() {
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/notifications/count"))
            .and(query_param("alreadyRead", "false"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(r#"{"count": 42}"#, "application/json"),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server(&backlog).await;
        let result = server.count_unread_notifications().await.unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 端数の値にした上でキーと値の組で照合する。丸め値の 7 だと
        // ハードコードされた `count: 7` を見逃す。
        assert!(text.contains("\\\"count\\\": 42"), "got: {text}");
    }
}
