# backlog-mcp

Backlogの課題を読み書きするMCPサーバー。ライブラリとしてTauriアプリ等に組み込めるほか、
単体のMCPサーバーとしても起動できる。Backlogへのアクセスには
[backlog-api-rs](https://github.com/takuji/backlog-api-rs) を使用。

## ツール

課題:
`search_issues` / `get_issue` / `create_issue` / `update_issue` / `add_comment` /
`list_comments`

Wiki:
`search_wikis` / `count_wikis` / `get_wiki` / `create_wiki` / `update_wiki`

ドキュメント:
`search_documents` / `count_documents` / `document_tree` / `get_document` /
`create_document` / `list_document_comments`

`document_tree` はゴミ箱の中身を返さない。

添付ファイル:
`download_issue_attachment` / `download_wiki_attachment` /
`download_document_attachment`

添付ファイルは `~/Downloads/backlog-mcp/` に保存され、返り値の `path` で開ける。
画像とテキストは内容もインラインで返す。アップロードと削除は提供しない。
保存先は環境変数 `BACKLOG_MCP_DOWNLOAD_DIR` で変更できる。
ダウンロードは受信しながらディスクに書くため大きなファイルでもメモリを消費しない(上限 500 MiB)。
保存したファイルにはインターネット由来の印(macOS: `com.apple.quarantine` /
Windows: `Zone.Identifier`)を付けるので、Gatekeeper や SmartScreen が外部由来として扱う
(exFAT/FAT32 など対応していない保存先では失敗することがあり、その場合はツールの
出力でその旨を報告する)。

通知・ウォッチ:
`list_notifications` / `count_unread_notifications` / `list_watchings` /
`count_watchings`

Git・プルリクエスト:
`list_repositories` / `search_pull_requests` / `count_pull_requests` /
`get_pull_request` / `list_pull_request_comments`

参照系:
`list_projects` / `get_project` / `list_users` / `get_myself`

課題・Wikiの削除ツールは提供しない。ドキュメントは削除ツールを提供せず、
更新はBacklog API自体に存在しない。通知・ウォッチの既読化ツールも提供しない。
Git・プルリクエストは読み取り専用で、作成・更新・マージ等の書き込み系ツールは提供しない。

## 単体起動

```sh
export BACKLOG_BASE_URL=https://<space>.backlog.com
export BACKLOG_API_KEY=<key>

backlog-mcp                       # stdio(MCPクライアントから起動される形態)
backlog-mcp --http                # http://127.0.0.1:39150/mcp
backlog-mcp --http --port 40000   # ポート指定
```

Claude Code への登録例:

```sh
# stdio
claude mcp add backlog -e BACKLOG_BASE_URL=... -e BACKLOG_API_KEY=... -- /path/to/backlog-mcp
# HTTP(--http で起動しておく)
claude mcp add --transport http backlog http://127.0.0.1:39150/mcp
```

## ライブラリとして組み込む

```rust
let handle = backlog_mcp::serve_http(config, 39150).await?;
// handle.local_addr() で実際のアドレス、handle.shutdown().await で停止
```

## セキュリティ

HTTPサーバーは 127.0.0.1 にのみバインドし、外部には公開されない。
ただしローカルエンドポイントに認証は無いため、**同一マシン上の他プロセスからは
アクセス可能**である点に注意(Figma Dev Mode MCP と同じ方式)。

リクエストの Host / Origin ヘッダーを検証し、ローカル(127.0.0.1 / localhost)以外を
指すものは 403 で拒否する(DNSリバインディング攻撃対策)。

## 開発メモ

Backlog の OpenAPI 仕様(`openapi.yaml`)と実APIの食い違いを
[docs/openapi-yaml-discrepancies.md](docs/openapi-yaml-discrepancies.md) に記録している。
新しいAPI領域に対応するときは先に読むこと。
