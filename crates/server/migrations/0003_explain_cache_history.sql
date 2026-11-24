-- 完了した解説のキャッシュ。key = sha256(モデル, mode, text, context, プロンプトのバージョン)
CREATE TABLE IF NOT EXISTS explain_cache (
    key TEXT PRIMARY KEY NOT NULL,
    model TEXT NOT NULL,
    mode TEXT NOT NULL,
    text TEXT NOT NULL,
    markdown TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) WITHOUT ROWID;

-- 論文ごとの履歴（調べた単語・解説した文）
CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    paper_id TEXT NOT NULL,
    paper_title TEXT NOT NULL,
    mode TEXT NOT NULL,
    text TEXT NOT NULL,
    context TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (paper_id, mode, text)
);
CREATE INDEX IF NOT EXISTS history_updated ON history (updated_at DESC);
