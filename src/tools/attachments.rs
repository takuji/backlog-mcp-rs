use backlog_api_rs::models::{Attachment, DocumentAttachment, WikiAttachment};

/// 添付ファイルのメタデータ。
///
/// IDは文字列で統一する(ドキュメントの添付IDが文字列であり、ダウンロードツールも
/// 3種とも文字列で受けるため)。
#[derive(serde::Serialize)]
pub(crate) struct AttachmentOut {
    pub id: String,
    pub name: String,
    pub size: u64,
}

impl From<&Attachment> for AttachmentOut {
    fn from(a: &Attachment) -> Self {
        Self {
            id: a.id.to_string(),
            name: a.name.clone(),
            size: a.size,
        }
    }
}

impl From<&WikiAttachment> for AttachmentOut {
    fn from(a: &WikiAttachment) -> Self {
        Self {
            id: a.id.to_string(),
            name: a.name.clone(),
            size: a.size,
        }
    }
}

impl From<&DocumentAttachment> for AttachmentOut {
    fn from(a: &DocumentAttachment) -> Self {
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            size: a.size,
        }
    }
}

use std::path::{Path, PathBuf};

use backlog_api_rs::{AttachmentDownload, OriginMark};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};

use super::{backlog_error, json_result};
use crate::server::BacklogMcpServer;

/// インラインで内容を返す上限。これを超えるファイルは保存とメタデータのみ返す。
pub(crate) const INLINE_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// ダウンロード自体の上限。これを超えるファイルはツールエラーにする。
///
/// 受信しながらディスクに書くためメモリは食わない。ディスク容量と転送時間だけの
/// 問題になるため、AIに渡せない形式も「人間がFinderで開くために落とす」用途を
/// 満たせるだけの余裕を取っている。
pub(crate) const DOWNLOAD_LIMIT_BYTES: u64 = 500 * 1024 * 1024;

/// 相対パスなら現在の作業ディレクトリを基準に絶対パス化する。
///
/// `current_dir()` が失敗した場合は相対パスのまま返す(呼び出し側の
/// `path.is_relative()` 前提を崩さないための最善努力)。
fn absolutize(dir: PathBuf) -> PathBuf {
    if dir.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        return cwd.join(dir);
    }
    dir
}

/// 保存先ディレクトリを解決する。
///
/// 環境変数 `BACKLOG_MCP_DOWNLOAD_DIR`(プロセス起動時の設定)があればそれを、
/// 無ければ `~/Downloads/backlog-mcp/` を使う。ホームが取れない場合は一時ディレクトリ。
/// MCPの呼び出し側からは指定できない。
/// 戻り値は常に絶対パスにする(相対パスが設定されていても絶対化する)。
pub(crate) fn resolve_download_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BACKLOG_MCP_DOWNLOAD_DIR")
        && !dir.is_empty()
    {
        return absolutize(PathBuf::from(dir));
    }
    match dirs::download_dir() {
        Some(downloads) => downloads.join("backlog-mcp"),
        None => std::env::temp_dir().join("backlog-mcp"),
    }
}

/// パスに使う識別子(呼び出し側が指定する課題キー・ドキュメントID・添付ID)を安全な形にする。
/// 英数字・ドット・アンダースコア・ハイフン以外はすべて `_` にし、ドットのみの文字列は潰す。
///
/// `name` とは異なりASCII英数字以外はすべて置換する(非ASCIIの課題キー等は潰れるが、
/// 表示用の名前は別途 `name` フィールドとして元の値のまま返るため実害はない)。
fn safe_path_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // 部分文字列としての ".." も残さない(連結後の可読性・誤読対策の多重防御)。
    let cleaned = cleaned.replace("..", "_");
    // "." や ".." のようなドットのみの成分を無害化する
    if cleaned.chars().all(|c| c == '.') || cleaned.is_empty() {
        return "_".to_string();
    }
    cleaned
}

/// Windows でファイル名に使えない文字。
///
/// `/` と `\` は別途パス区切りとして扱うためここには含めない。
const WINDOWS_INVALID_CHARS: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];

/// 表示順を変える Unicode 双方向制御文字。
///
/// これらを含むファイル名は「表示される名前」と「実際の名前」を食い違わせられる。
/// 例: 実体が `invoice<U+202E>gpj.exe` のファイルは、U+202E 以降が右から左に
/// 表示されるため `invoiceexe.jpg` と見える。ユーザーは画像だと思って
/// 実行ファイルを開くことになる。
///
/// `char::is_control()` は Cc(制御コード)しか対象にせず、これらは Cf(書式)
/// なので通ってしまうため、明示的に列挙する。Unicode の `Bidi_Control=Yes`
/// を持つ12文字と一致させている(判断の余地を残さないため、並べ替えの効力が
/// ない U+061C も含める)。ZWJ(U+200D)は `Bidi_Control` ではなく、絵文字の
/// 結合に正当に使われるため含めない。
const BIDI_CONTROL_CHARS: [char; 12] = [
    '\u{061C}', // ARABIC LETTER MARK
    '\u{200E}', // LEFT-TO-RIGHT MARK
    '\u{200F}', // RIGHT-TO-LEFT MARK
    '\u{202A}', // LEFT-TO-RIGHT EMBEDDING
    '\u{202B}', // RIGHT-TO-LEFT EMBEDDING
    '\u{202C}', // POP DIRECTIONAL FORMATTING
    '\u{202D}', // LEFT-TO-RIGHT OVERRIDE
    '\u{202E}', // RIGHT-TO-LEFT OVERRIDE
    '\u{2066}', // LEFT-TO-RIGHT ISOLATE
    '\u{2067}', // RIGHT-TO-LEFT ISOLATE
    '\u{2068}', // FIRST STRONG ISOLATE
    '\u{2069}', // POP DIRECTIONAL ISOLATE
];

/// 保存するファイル名を組み立てる。
///
/// `{prefix}-{id}-{サニタイズしたname}` 形式。パス脱出・隠しファイル化・制御文字・
/// Windows で無効な文字・表示を偽装する双方向制御文字を防ぐ。
fn safe_file_name(prefix: &str, id: &str, name: &str) -> String {
    let prefix = safe_path_component(prefix);
    let id = safe_path_component(id);
    let cleaned: String = name
        .chars()
        .map(|c| {
            // パス区切りと制御文字に加えて、Windows で無効な文字も潰す。
            // プラットフォームで分岐しないことで、Windows 固有の経路も
            // macOS のテストで検証できる状態を保つ。
            // 特に ':' は NTFS の代替データストリームを作るため実害が大きい。
            if c == '/'
                || c == '\\'
                || c.is_control()
                || WINDOWS_INVALID_CHARS.contains(&c)
                || BIDI_CONTROL_CHARS.contains(&c)
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.replace("..", "_");
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    let base = if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    };
    let mut full = format!("{prefix}-{id}-{base}");
    if full.len() > 120 {
        // 拡張子(最後の "." 以降)が短くASCII英数字のみで構成されている場合は
        // 「もっともらしい拡張子」とみなし、Finderなどで開けるよう維持する。
        // それ以外(拡張子が無い/長すぎる/非ASCII)は従来通り単純truncateする。
        let plausible_ext = full.rsplit_once('.').and_then(|(_, ext)| {
            let len = ext.len();
            if (1..=16).contains(&len) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                Some(ext.to_string())
            } else {
                None
            }
        });
        match plausible_ext {
            Some(ext) => {
                let suffix_len = ext.len() + 1; // "." + ext
                let max_stem_len = 120 - suffix_len;
                let mut cut = max_stem_len.min(full.len() - suffix_len);
                while cut > 0 && !full.is_char_boundary(cut) {
                    cut -= 1;
                }
                let mut truncated = full[..cut].to_string();
                truncated.push('.');
                truncated.push_str(&ext);
                full = truncated;
            }
            None => {
                // 120バイト以内で、かつ文字境界になる位置まで切る
                let mut cut = 120;
                while cut > 0 && !full.is_char_boundary(cut) {
                    cut -= 1;
                }
                full.truncate(cut);
            }
        }
    }
    // Windows では末尾がドットまたはスペースのファイル名が無効。
    // 切り詰めが末尾にドットやスペースを作りうるため、必ず切り詰めの「後」に行う。
    let trimmed = full.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        // prefix は safe_path_component により最低1文字の非ドット・非スペース文字を
        // 含むため通常ここには来ないが、念のため。
        return "attachment".to_string();
    }
    trimmed.to_string()
}

/// ファイルがテキストとして扱えるか。
fn is_text(content_type: Option<&str>, file_name: &str) -> bool {
    if let Some(ct) = content_type {
        let ct = ct
            .split(';')
            .next()
            .unwrap_or(ct)
            .trim()
            .to_ascii_lowercase();
        if ct.starts_with("text/") {
            return true;
        }
        if matches!(
            ct.as_str(),
            "application/json" | "application/xml" | "application/yaml" | "application/toml"
        ) {
            return true;
        }
    }
    let lower = file_name.to_ascii_lowercase();
    [
        ".txt", ".md", ".csv", ".tsv", ".json", ".log", ".yaml", ".yml", ".toml", ".xml", ".html",
        ".rs", ".ts", ".js", ".py", ".sh",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// ファイルが画像か。返り値は MCP の image content に渡す mime type。
fn image_mime(content_type: Option<&str>, file_name: &str) -> Option<String> {
    if let Some(ct) = content_type {
        let ct = ct
            .split(';')
            .next()
            .unwrap_or(ct)
            .trim()
            .to_ascii_lowercase();
        if matches!(
            ct.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Some(ct);
        }
    }
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Some("image/png".to_string())
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg".to_string())
    } else if lower.ends_with(".gif") {
        Some("image/gif".to_string())
    } else if lower.ends_with(".webp") {
        Some("image/webp".to_string())
    } else {
        None
    }
}

/// ダウンロード結果のメタデータ。
#[derive(serde::Serialize)]
struct DownloadedOut {
    /// 保存された絶対パス。`open` などで開ける。
    path: String,
    name: String,
    size: u64,
    content_type: Option<String>,
    /// 内容をインラインでも返したか。
    inlined: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_omitted_reason: Option<String>,
    /// インターネット由来の印(Windows: Zone.Identifier / macOS: quarantine)が
    /// 付いたか。付いていればOSの保護機構(SmartScreen / Gatekeeper)が働く。
    marked_from_internet: bool,
    /// 印が付かなかった理由。付いた場合は省略される。
    #[serde(skip_serializing_if = "Option::is_none")]
    mark_warning: Option<String>,
}

/// ダウンロードをファイルに書き出し、`CallToolResult` を組み立てる。
///
/// 常にファイルに保存してパスを返し、画像とテキスト(インライン上限内)は
/// 保存したファイルを読み戻して内容も content に足す。
///
/// `limit` は上限をテストから差し替えられるようパラメータ化している
/// (本番の呼び出し元は常に `DOWNLOAD_LIMIT_BYTES` を渡す)。
async fn save_download_and_build_result(
    dir: &Path,
    prefix: &str,
    id: &str,
    download: AttachmentDownload,
    limit: u64,
) -> Result<CallToolResult, McpError> {
    let display_name = download
        .file_name
        .clone()
        .unwrap_or_else(|| "attachment".to_string());
    let content_type = download.content_type.clone();
    let file_name = safe_file_name(prefix, id, &display_name);
    let path = dir.join(&file_name);

    if let Err(e) = std::fs::create_dir_all(dir) {
        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "保存先ディレクトリを作成できませんでした({}): {e}",
            dir.display()
        ))]));
    }

    let saved = match download.save_to(&path, Some(limit)).await {
        Ok(saved) => saved,
        Err(backlog_api_rs::Error::TooLarge { size, limit }) => {
            // 単位を揃える: どちらもバイトで表示し、MiB表記は上限が1MiB以上の
            // ときだけ添える(整数除算で「0 MiB」になるのを避けるため)。
            let limit_desc = if limit >= 1024 * 1024 {
                format!("{limit} バイト({} MiB)", limit / (1024 * 1024))
            } else {
                format!("{limit} バイト")
            };
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "添付ファイルが大きすぎます({size} バイト)。上限は {limit_desc} です。Backlogのブラウザ画面からダウンロードしてください"
            ))]));
        }
        Err(backlog_api_rs::Error::Io(io_err)) => {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "添付ファイルの保存に失敗しました({}): {io_err}",
                path.display()
            ))]));
        }
        Err(e) => return Ok(backlog_error(e)),
    };

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut inlined = false;
    let mut reason: Option<String> = None;

    let too_big_to_inline = saved.bytes > INLINE_LIMIT_BYTES as u64;
    if let Some(mime) = image_mime(content_type.as_deref(), &display_name) {
        if too_big_to_inline {
            reason = Some(
                "画像が大きいため内容は返していません。パスのファイルを開いて確認してください"
                    .to_string(),
            );
        } else {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    blocks.push(ContentBlock::image(base64_encode(&bytes), mime));
                    inlined = true;
                }
                Err(e) => {
                    reason = Some(format!(
                        "保存したファイルを読み直せなかったため内容は返していません: {e}"
                    ));
                }
            }
        }
    } else if is_text(content_type.as_deref(), &display_name) {
        if too_big_to_inline {
            reason = Some(
                "テキストが大きいため内容は返していません。パスのファイルを開いて確認してください"
                    .to_string(),
            );
        } else {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    blocks.push(ContentBlock::text(text));
                    inlined = true;
                }
                Err(_) => {
                    reason = Some("UTF-8として読めなかったため内容は返していません。パスのファイルを開いて確認してください".to_string());
                }
            }
        }
    } else {
        reason = Some("この形式はAIから読めないため内容は返していません。パスのファイルを開いて確認してください".to_string());
    }

    let out = build_downloaded_out(
        path.display().to_string(),
        display_name,
        saved.bytes,
        content_type,
        inlined,
        reason,
        saved.origin_mark,
    );
    let mut result = json_result(&out)?;
    result.content.extend(blocks);
    Ok(result)
}

/// `DownloadedOut` を組み立てる。
///
/// `origin_mark` をパラメータ化してあるので、実際の `saved.origin_mark`
/// (macOSでは常に `Applied`)以外の値、特に `Failed` を渡したときの
/// シリアライズ結果をテストから直接検証できる。
#[allow(clippy::too_many_arguments)]
fn build_downloaded_out(
    path: String,
    name: String,
    size: u64,
    content_type: Option<String>,
    inlined: bool,
    inline_omitted_reason: Option<String>,
    origin_mark: OriginMark,
) -> DownloadedOut {
    let (marked_from_internet, mark_warning) = mark_fields(origin_mark);
    DownloadedOut {
        path,
        name,
        size,
        content_type,
        inlined,
        inline_omitted_reason,
        marked_from_internet,
        mark_warning,
    }
}

/// `OriginMark` を出力用の2フィールドに変換する。
///
/// ファイルシステムアクセスを一切行わない純粋関数にすることで、
/// モックサーバーやファイルシステムなしに3パターン全てを単体テストできるようにする。
fn mark_fields(mark: OriginMark) -> (bool, Option<String>) {
    match mark {
        OriginMark::Applied => (true, None),
        OriginMark::Unsupported => (
            false,
            Some("このOSにはダウンロード元を記録する仕組みがありません".to_string()),
        ),
        OriginMark::Failed(e) => (
            false,
            Some(format!(
                "ダウンロード元を記録できませんでした: {e}。保存先のファイルシステムが拡張属性や代替データストリームに対応していない可能性があります"
            )),
        ),
    }
}

/// base64(標準アルファベット)にエンコードする。
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DownloadIssueAttachmentParams {
    /// 課題キー(例: "PROJ-123")
    pub issue_key: String,
    /// 添付ファイルID(get_issue の attachments に含まれる)
    pub attachment_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DownloadWikiAttachmentParams {
    /// WikiページのID
    pub wiki_id: u64,
    /// 添付ファイルID(get_wiki の attachments に含まれる)
    pub attachment_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DownloadDocumentAttachmentParams {
    /// ドキュメントID
    pub document_id: String,
    /// 添付ファイルID(get_document の attachments に含まれる)
    pub attachment_id: String,
}

/// 数値の添付ファイルIDをパースする。失敗時はツールエラーを返す。
fn parse_attachment_id(id: &str) -> Result<u64, CallToolResult> {
    id.parse::<u64>().map_err(|_| {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "添付ファイルIDは数値です(受け取った値: {id})"
        ))])
    })
}

#[tool_router(router = attachments_tool_router, vis = "pub(crate)")]
impl BacklogMcpServer {
    #[tool(
        description = "課題の添付ファイルをダウンロードする。ファイルは ~/Downloads/backlog-mcp/ に保存され、返り値の path で開ける(open コマンドなど)。画像とテキストは内容も返す"
    )]
    pub(crate) async fn download_issue_attachment(
        &self,
        Parameters(params): Parameters<DownloadIssueAttachmentParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match parse_attachment_id(&params.attachment_id) {
            Ok(id) => id,
            Err(result) => return Ok(result),
        };
        match self
            .client
            .issues()
            .start_attachment_download(&params.issue_key, id)
            .await
        {
            Ok(download) => {
                save_download_and_build_result(
                    &self.download_dir,
                    &params.issue_key,
                    &params.attachment_id,
                    download,
                    DOWNLOAD_LIMIT_BYTES,
                )
                .await
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "Wikiページの添付ファイルをダウンロードする。ファイルは ~/Downloads/backlog-mcp/ に保存され、返り値の path で開ける。画像とテキストは内容も返す"
    )]
    pub(crate) async fn download_wiki_attachment(
        &self,
        Parameters(params): Parameters<DownloadWikiAttachmentParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match parse_attachment_id(&params.attachment_id) {
            Ok(id) => id,
            Err(result) => return Ok(result),
        };
        match self
            .client
            .wikis()
            .start_attachment_download(params.wiki_id, id)
            .await
        {
            Ok(download) => {
                save_download_and_build_result(
                    &self.download_dir,
                    &params.wiki_id.to_string(),
                    &params.attachment_id,
                    download,
                    DOWNLOAD_LIMIT_BYTES,
                )
                .await
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }

    #[tool(
        description = "ドキュメントの添付ファイルをダウンロードする。ファイルは ~/Downloads/backlog-mcp/ に保存され、返り値の path で開ける。画像とテキストは内容も返す"
    )]
    pub(crate) async fn download_document_attachment(
        &self,
        Parameters(params): Parameters<DownloadDocumentAttachmentParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .client
            .documents()
            .start_attachment_download(&params.document_id, &params.attachment_id)
            .await
        {
            Ok(download) => {
                save_download_and_build_result(
                    &self.download_dir,
                    &params.document_id,
                    &params.attachment_id,
                    download,
                    DOWNLOAD_LIMIT_BYTES,
                )
                .await
            }
            Err(e) => Ok(backlog_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_numeric_and_string_attachment_ids_to_strings() {
        let issue_attachment: backlog_api_rs::models::Attachment =
            serde_json::from_str(r#"{ "id": 22067, "name": "spec.pdf", "size": 8718 }"#).unwrap();
        let out = AttachmentOut::from(&issue_attachment);
        assert_eq!(out.id, "22067");
        assert_eq!(out.name, "spec.pdf");
        assert_eq!(out.size, 8718);

        let doc_attachment: backlog_api_rs::models::DocumentAttachment =
            serde_json::from_str(r#"{ "id": 22067, "name": "a.png", "size": 10 }"#).unwrap();
        assert_eq!(AttachmentOut::from(&doc_attachment).id, "22067");

        let wiki_attachment: backlog_api_rs::models::WikiAttachment =
            serde_json::from_str(r#"{ "id": 5, "name": "図面.png", "size": 1024 }"#).unwrap();
        let out = AttachmentOut::from(&wiki_attachment);
        assert_eq!(out.id, "5");
        assert_eq!(out.name, "図面.png");
    }

    use crate::server::BacklogMcpServer;
    use rmcp::handler::server::wrapper::Parameters;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_server_with_dir(backlog: &MockServer, dir: &std::path::Path) -> BacklogMcpServer {
        // 環境変数は使わない(テストは並列実行されるため競合する)。
        // 保存先はテスト専用のビルダーで直接差し替える。
        BacklogMcpServer::new(&crate::BacklogMcpConfig {
            base_url: backlog.uri(),
            api_key: "KEY".to_string(),
        })
        .unwrap()
        .with_download_dir(dir.to_path_buf())
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("backlog-mcp-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_file_name_neutralizes_dangerous_names() {
        // パス区切りと親ディレクトリ参照
        let name = safe_file_name("TEST1-1", "5", "../../etc/passwd");
        assert!(!name.contains('/'), "got: {name}");
        assert!(!name.contains(".."), "got: {name}");
        assert!(name.starts_with("TEST1-1-5-"), "got: {name}");

        // 先頭ドット(隠しファイル化)
        let name = safe_file_name("W", "1", ".bashrc");
        assert!(!name.contains("-."), "got: {name}");

        // 制御文字・NUL
        let name = safe_file_name("W", "1", "a\u{0}b\nc");
        assert!(!name.contains('\u{0}'), "got: {name}");
        assert!(!name.contains('\n'), "got: {name}");

        // 空の名前
        let name = safe_file_name("W", "1", "");
        assert!(name.contains("attachment"), "got: {name}");

        // 長すぎる名前は切り詰める
        let long = "a".repeat(500);
        let name = safe_file_name("W", "1", &long);
        assert!(name.len() <= 120, "len={} name={name}", name.len());

        // 日本語は保持する
        let name = safe_file_name("W", "1", "図面.png");
        assert!(name.contains("図面.png"), "got: {name}");
    }

    /// Windows で無効な文字を含む名前を安全にする。
    ///
    /// 特に `:` は NTFS の代替データストリーム(ADS)を作るため実害が大きい
    /// (ダウンロードは成功したように見えるのに Explorer にファイルが見えない)。
    /// プラットフォームで分岐せず全環境で同じ規則にしているので、macOS でも検証できる。
    #[test]
    fn safe_file_name_neutralizes_windows_invalid_characters() {
        let name = safe_file_name("P-1", "9", "a<b>c:d\"e|f?g*h.txt");
        for invalid in ['<', '>', ':', '"', '|', '?', '*'] {
            assert!(
                !name.contains(invalid),
                "{invalid:?} should be replaced: {name}"
            );
        }
        // 代替データストリームの区切りである ':' は特に重要
        assert!(!name.contains(':'), "got: {name}");
        // 拡張子は保たれる
        assert!(name.ends_with(".txt"), "got: {name}");
    }

    /// 双方向制御文字による「表示名と実体の食い違い」を防ぐ。
    ///
    /// U+202E(RIGHT-TO-LEFT OVERRIDE)以降は右から左に表示される。実体が
    /// `invoice<RLO>gpj.exe` のファイルは `invoiceexe.jpg` と表示されるため、
    /// ユーザーは画像だと思って実行ファイルを開いてしまう。
    /// `char::is_control()` は Cc(制御コード)だけで、これらの Cf(書式)文字は
    /// 通さないので明示的に潰す必要がある。
    #[test]
    fn safe_file_name_neutralizes_bidi_control_characters() {
        // 古典的な偽装: 実体は .exe だが .jpg と表示される
        let name = safe_file_name("P-1", "9", "invoice\u{202E}gpj.exe");
        assert!(!name.contains('\u{202E}'), "got: {name:?}");
        // 制御文字が消えたので、危険な実体の拡張子がそのまま見えること
        assert!(name.ends_with(".exe"), "got: {name:?}");

        // 対象の全文字が個別に潰されること(Unicode の Bidi_Control=Yes と一致)
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            let name = safe_file_name("P-1", "9", &format!("a{c}b.txt"));
            assert!(!name.contains(c), "{c:?} should be replaced: {name:?}");
        }

        // 正当な日本語・絵文字は壊さない(絵文字のZWJ結合は対象外)
        let name = safe_file_name("P-1", "9", "図面.png");
        assert!(name.contains("図面.png"), "got: {name}");
        let name = safe_file_name("P-1", "9", "家族👨‍👩‍👧.png");
        assert!(name.contains("家族👨‍👩‍👧.png"), "got: {name}");
    }

    /// Windows では末尾がドットまたはスペースのファイル名が無効。
    #[test]
    fn safe_file_name_strips_trailing_dots_and_spaces() {
        let name = safe_file_name("P-1", "9", "report.");
        assert!(!name.ends_with('.'), "got: {name}");

        let name = safe_file_name("P-1", "9", "report ");
        assert!(!name.ends_with(' '), "got: {name}");

        // 複数連続でも残らない
        let name = safe_file_name("P-1", "9", "report.. ..  ");
        assert!(!name.ends_with('.') && !name.ends_with(' '), "got: {name}");
    }

    /// 切り詰めが末尾にドットやスペースを作らないこと。
    ///
    /// 除去は切り詰めの後に行う必要がある(順序が逆だと切り詰めで再発する)。
    #[test]
    fn safe_file_name_strips_trailing_dots_created_by_truncation() {
        // 120バイト目付近がドットになるよう調整した、拡張子のない長い名前。
        // prefix("P-1") + "-" + id("9") + "-" = 6バイトなので、名前側は114バイト目が境界。
        for pad in 100..130 {
            let candidate = format!("{}.{}", "a".repeat(pad), "b".repeat(200));
            let name = safe_file_name("P-1", "9", &candidate);
            assert!(name.len() <= 120, "len={} name={name}", name.len());
            assert!(!name.ends_with('.'), "got: {name}");
            assert!(!name.ends_with(' '), "got: {name}");
        }
    }

    #[test]
    fn safe_file_name_preserves_extension_for_long_ascii_name() {
        let name = safe_file_name("P-1", "9", &format!("{}.png", "a".repeat(300)));
        assert!(name.ends_with(".png"), "got: {name}");
        assert!(name.len() <= 120, "len={} name={name}", name.len());
    }

    #[test]
    fn safe_file_name_preserves_extension_for_long_japanese_name() {
        let long_name = format!("{}.png", "図".repeat(100));
        let name = safe_file_name("P-1", "9", &long_name);
        assert!(name.ends_with(".png"), "got: {name}");
        assert!(name.len() <= 120, "len={} name={name}", name.len());
        assert!(
            std::str::from_utf8(name.as_bytes()).is_ok(),
            "not valid UTF-8: {name:?}"
        );
        assert!(name.contains('図'), "got: {name}");
    }

    #[test]
    fn safe_file_name_without_extension_still_truncates() {
        let long = "a".repeat(500);
        let name = safe_file_name("W", "1", &long);
        assert!(name.len() <= 120, "len={} name={name}", name.len());
        assert!(!name.contains('.'), "got: {name}");
    }

    #[test]
    fn safe_file_name_falls_back_to_plain_truncation_for_implausible_extension() {
        let long = format!("{}.thisisaverylongextension", "a".repeat(300));
        let name = safe_file_name("W", "1", &long);
        assert!(name.len() <= 120, "len={} name={name}", name.len());
        assert!(!name.ends_with(".thisisaverylongextension"), "got: {name}");
    }

    #[test]
    fn safe_file_name_sanitizes_prefix_and_id_path_traversal() {
        // prefix に ".." を含む相対パス脱出を試みるケース
        let name = safe_file_name("../pwned", "x", "a.txt");
        assert!(!name.contains('/'), "got: {name}");
        assert!(!name.contains(".."), "got: {name}");

        // prefix が絶対パスに見えるケース(Path::join がサンドボックスを無視するのを防ぐ)
        let name = safe_file_name("/tmp/evil", "x", "a.txt");
        assert!(!name.contains('/'), "got: {name}");

        // id 側も同様にサニタイズされること
        let name = safe_file_name("W", "../../etc/passwd", "a.txt");
        assert!(!name.contains('/'), "got: {name}");
        assert!(!name.contains(".."), "got: {name}");
    }

    /// `Applied` は印が付いたことを示し、警告は無い。
    #[test]
    fn mark_fields_applied_has_no_warning() {
        let (marked, warning) = super::mark_fields(OriginMark::Applied);
        assert!(marked);
        assert_eq!(warning, None);
    }

    /// `Unsupported`(OS自体に仕組みが無い)は印なし・警告ありで、
    /// その警告文言が「このOS」を名指しすること。
    #[test]
    fn mark_fields_unsupported_reports_no_mark_with_warning() {
        let (marked, warning) = super::mark_fields(OriginMark::Unsupported);
        assert!(!marked);
        let warning = warning.expect("Unsupportedでは警告が必要");
        assert!(
            warning.contains("このOSにはダウンロード元を記録する仕組みがありません"),
            "got: {warning}"
        );
    }

    /// `Failed`(書き込み自体が失敗)は印なし・警告ありで、
    /// 下層のエラー文字列を含み、かつ拡張属性と代替データストリームの
    /// 両方に言及すること(Windows は代替データストリーム、それ以外は
    /// 拡張属性なので、原因を片方だけに決め打ちしない)。
    #[test]
    fn mark_fields_failed_reports_no_mark_with_underlying_error() {
        let (marked, warning) = super::mark_fields(OriginMark::Failed(
            "Permission denied (os error 13)".to_string(),
        ));
        assert!(!marked);
        let warning = warning.expect("Failedでは警告が必要");
        assert!(
            warning.contains("Permission denied (os error 13)"),
            "got: {warning}"
        );
        assert!(
            warning.contains("拡張属性や代替データストリーム"),
            "got: {warning}"
        );
    }

    /// `DownloadedOut` の実際のシリアライズ結果を検証する。
    ///
    /// `mark_fields` の単体テストは変換関数の戻り値(タプル)しか見ておらず、
    /// `download_output_reports_the_origin_mark` はmacOS上では常に `Applied`
    /// しか通らない。そのため `Failed` のJSON出力は今まで一度も検証されて
    /// いなかった。`mark_warning` は `skip_serializing_if` が付いているため、
    /// この属性が壊れる・フィールド名が変わる・警告が構築時に落ちるといった
    /// 事故があっても、これまでのテストでは誰も気づけなかった。
    #[test]
    fn build_downloaded_out_serializes_failed_mark_warning_and_omits_it_when_applied() {
        let failed = super::build_downloaded_out(
            "/tmp/backlog-mcp/note.txt".to_string(),
            "note.txt".to_string(),
            5,
            Some("text/plain".to_string()),
            true,
            None,
            OriginMark::Failed("Permission denied (os error 13)".to_string()),
        );
        // json_result と同じシリアライザ(pretty JSON)で確認する。
        let json = serde_json::to_string_pretty(&failed).unwrap();
        assert!(
            json.contains("\"marked_from_internet\": false"),
            "got: {json}"
        );
        assert!(json.contains("\"mark_warning\""), "got: {json}");
        assert!(
            json.contains("Permission denied (os error 13)"),
            "got: {json}"
        );

        // 逆方向: `Applied` では `mark_warning` キー自体が出ないこと
        // (`skip_serializing_if` が両方向で効いていることの確認)。
        let applied = super::build_downloaded_out(
            "/tmp/backlog-mcp/note.txt".to_string(),
            "note.txt".to_string(),
            5,
            Some("text/plain".to_string()),
            true,
            None,
            OriginMark::Applied,
        );
        let json = serde_json::to_string_pretty(&applied).unwrap();
        assert!(
            json.contains("\"marked_from_internet\": true"),
            "got: {json}"
        );
        assert!(!json.contains("mark_warning"), "got: {json}");
    }

    #[test]
    fn absolutize_joins_relative_path_onto_current_dir() {
        let cwd = std::env::current_dir().unwrap();
        let result = super::absolutize(std::path::PathBuf::from("relative/subdir"));
        assert!(result.is_absolute(), "got: {result:?}");
        assert_eq!(result, cwd.join("relative/subdir"));

        // すでに絶対パスならそのまま
        let abs = std::path::PathBuf::from("/already/absolute");
        assert_eq!(super::absolutize(abs.clone()), abs);
    }

    #[tokio::test]
    async fn download_issue_attachment_saves_file_and_inlines_text() {
        let dir = temp_dir("issue-text");
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1/attachments/22067"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"hello from the attachment".to_vec())
                    .insert_header("content-type", "text/plain")
                    .insert_header("content-disposition", "attachment; filename=\"notes.txt\""),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_issue_attachment(Parameters(super::DownloadIssueAttachmentParams {
                issue_key: "TEST1-1".to_string(),
                attachment_id: "22067".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 保存パスが返ること
        assert!(text.contains("notes.txt"), "got: {text}");
        assert!(text.contains("TEST1-1-22067-notes.txt"), "got: {text}");
        // テキストは内容もインラインされること
        assert!(text.contains("hello from the attachment"), "got: {text}");
        // 実際にファイルが存在すること
        let saved = dir.join("TEST1-1-22067-notes.txt");
        assert!(saved.exists(), "file not saved: {saved:?}");
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap(),
            "hello from the attachment"
        );
    }

    #[tokio::test]
    async fn download_issue_attachment_rejects_non_numeric_id() {
        let dir = temp_dir("issue-badid");
        let backlog = MockServer::start().await;
        // APIは呼ばれてはいけない(モック未登録なので呼ばれたら失敗する)

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_issue_attachment(Parameters(super::DownloadIssueAttachmentParams {
                issue_key: "TEST1-1".to_string(),
                attachment_id: "not-a-number".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("数値"), "got: {text}");
    }

    #[tokio::test]
    async fn download_document_attachment_omits_inline_for_pdf() {
        let dir = temp_dir("doc-pdf");
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/documents/doc-1/attachments/att-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"%PDF-1.7 binary junk".to_vec())
                    .insert_header("content-type", "application/pdf")
                    .insert_header("content-disposition", "attachment; filename=\"spec.pdf\""),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_document_attachment(Parameters(super::DownloadDocumentAttachmentParams {
                document_id: "doc-1".to_string(),
                attachment_id: "att-1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("spec.pdf"), "got: {text}");
        assert!(text.contains("inline_omitted_reason"), "got: {text}");
        // PDFの中身はインラインしない
        assert!(!text.contains("binary junk"), "got: {text}");
        assert!(dir.join("doc-1-att-1-spec.pdf").exists());
    }

    #[tokio::test]
    async fn download_document_attachment_sanitizes_path_traversal_in_document_id() {
        // document_id は呼び出し側(MCPクライアント)が自由な文字列を渡せる。
        // "../escape-marker" のような値でサンドボックス外への書き込みが
        // できないことを確認する回帰テスト。
        let dir = temp_dir("doc-traversal");
        let backlog = MockServer::start().await;
        // document_id はHTTPパスの1セグメントとしてエンコードされるため、
        // 正確な文字列に依存せず正規表現でゆるく一致させる。
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(
                r"^/api/v2/documents/.*/attachments/att-1$",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"payload".to_vec())
                    .insert_header("content-type", "text/plain")
                    .insert_header("content-disposition", "attachment; filename=\"a.txt\""),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_document_attachment(Parameters(super::DownloadDocumentAttachmentParams {
                document_id: "../escape-marker".to_string(),
                attachment_id: "att-1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        // 保存されたファイルはサンドボックス(テスト用の一時ディレクトリ)内にあること
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one file in {dir:?}: {entries:?}"
        );
        assert!(
            entries[0].starts_with(&dir),
            "escaped sandbox: {:?}",
            entries[0]
        );

        // サンドボックスの親ディレクトリに "escape-marker" を含む名前の
        // ファイルが作られていないこと(脱出していないことの確認)。
        let parent = dir.parent().unwrap();
        let escaped = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("escape-marker"));
        assert!(!escaped, "found an escaped file in parent dir {parent:?}");
    }

    #[tokio::test]
    async fn download_wiki_attachment_inlines_image() {
        let dir = temp_dir("wiki-image");
        let backlog = MockServer::start().await;
        // 最小のPNG(シグネチャのみ。内容の妥当性は問わない)
        let png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        Mock::given(method("GET"))
            .and(path("/api/v2/wikis/1/attachments/5"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png.clone())
                    .insert_header("content-type", "image/png")
                    .insert_header(
                        "content-disposition",
                        "attachment; filename*=UTF-8''%E5%9B%B3%E9%9D%A2.png",
                    ),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_wiki_attachment(Parameters(super::DownloadWikiAttachmentParams {
                wiki_id: 1,
                attachment_id: "5".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        // 日本語ファイル名が保持されること
        assert!(text.contains("図面.png"), "got: {text}");
        // image content が付くこと(MCPのimage contentはmimeTypeを持つ)
        assert!(text.contains("image/png"), "got: {text}");
        assert!(dir.join("1-5-図面.png").exists());
    }

    #[tokio::test]
    async fn download_omits_inline_for_oversized_text() {
        let dir = temp_dir("oversized");
        let backlog = MockServer::start().await;
        // インライン上限を超えるテキスト
        let big = "x".repeat(super::INLINE_LIMIT_BYTES + 1);
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1/attachments/1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(big.into_bytes())
                    .insert_header("content-type", "text/plain")
                    .insert_header("content-disposition", "attachment; filename=\"big.txt\""),
            )
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let result = server
            .download_issue_attachment(Parameters(super::DownloadIssueAttachmentParams {
                issue_key: "TEST1-1".to_string(),
                attachment_id: "1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("inline_omitted_reason"), "got: {text}");
        // 保存はされる
        assert!(dir.join("TEST1-1-1-big.txt").exists());
    }

    #[tokio::test]
    async fn download_rejects_oversized_file() {
        // 注意(ブリーフからの逸脱、詳細はタスクレポート参照): ブリーフ原案は
        // 「Content-Length を上限超で偽装し、実ボディは小さいまま」というレスポンスを
        // wiremock で用意する想定だった。だが wiremock/hyper (hyper 1.11.0
        // proto/h1/role.rs) のサーバー側エンコーダは、指定した Content-Length ヘッダーが
        // 実際のボディ長(Full<Bytes> の既知サイズ)と一致しないと `debug_assert!` で
        // パニックする。これは backlog-api-rs 側の save_to テストで既に踏んでいた
        // 制約と同じで、「嘘の Content-Length」を持つHTTP応答自体を作れない。
        //
        // 代わりに、上限をパラメータ化した内部ヘルパー
        // (`save_download_and_build_result`)をテストから直接呼び、
        // 実際に転送された(嘘のない)小さなボディに対して小さな上限を与えることで
        // 同じ分岐(`Error::TooLarge` → ツールエラー)を検証する。
        // 500MiBのボディは作らない(この変更の意図に反する)。
        let dir = temp_dir("too-big");
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1/attachments/2"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"0123456789ABCDEF".to_vec()))
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let download = server
            .client
            .issues()
            .start_attachment_download("TEST1-1", 2)
            .await
            .unwrap();
        assert_eq!(download.content_length, Some(16));

        let result = super::save_download_and_build_result(&dir, "TEST1-1", "2", download, 8)
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("大きすぎます"), "got: {text}");
        // 部分ファイルも残らないこと
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "no file should remain"
        );
    }

    /// ダウンロードしたファイルに印が付き、その結果が出力に載ること。
    ///
    /// 期待値を `cfg!` で切り替えるのは、印の仕組みがない環境
    /// (Linux 等)でこのテストが落ちないようにするため。ここで検証しているのは
    /// JSON出力に載る「報告された結果」であり、ファイルに実際に印が
    /// 付いたことそのものではない(それは backlog-api-rs 側の
    /// `save_to` テストが検証済み)。
    #[tokio::test]
    async fn download_output_reports_the_origin_mark() {
        let dir = temp_dir("origin-mark");
        let backlog = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/issues/TEST1-1/attachments/2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/plain")
                    .insert_header("content-disposition", "attachment; filename=\"note.txt\"")
                    .set_body_bytes(b"hello".to_vec()),
            )
            .expect(1)
            .mount(&backlog)
            .await;

        let server = test_server_with_dir(&backlog, &dir);
        let download = server
            .client
            .issues()
            .start_attachment_download("TEST1-1", 2)
            .await
            .unwrap();

        let result = super::save_download_and_build_result(&dir, "TEST1-1", "2", download, 1024)
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = serde_json::to_string(&result).unwrap();

        // size は `SavedFile { bytes, .. }` 由来であり、ボディの5バイト
        // (b"hello")と一致すること。末尾のカンマまで含めて比較し、
        // "size": 5 が "size": 50 のような値の前方一致で誤って
        // マッチしないようにする。
        assert!(text.contains("\\\"size\\\": 5,"), "got: {text}");

        // `DownloadedOut` は snake_case のまま(camelCase 化されていない)なので
        // フィールド名は `marked_from_internet`。出力は二重にJSON化されている
        // (中身のpretty JSON文字列が`CallToolResult`のtextフィールドとして
        // さらにエスケープされる)ため、キーの閉じ引用符はエスケープされた
        // `\"` として現れる。
        let marked = cfg!(any(windows, target_os = "macos"));
        let marked_pattern = format!("marked_from_internet\\\": {marked}");
        assert!(text.contains(&marked_pattern), "got: {text}");
        if marked {
            assert!(
                !text.contains("mark_warning"),
                "印が付いたのに警告が出ている: {text}"
            );
        } else {
            assert!(
                text.contains("mark_warning"),
                "印が付かなかったのに警告がない: {text}"
            );
        }
    }
}
