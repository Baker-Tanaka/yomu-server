-- アプリから送ったフィードバック（不具合・要望）。GitHub の Issue にしたら issue_url などが入る
CREATE TABLE IF NOT EXISTS feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    title TEXT NOT NULL,
    -- Issue の本文（Markdown。添付した状況を含む）
    body TEXT NOT NULL,
    -- ラベル名の JSON 配列
    labels TEXT NOT NULL,
    -- 'created'（Issue を作った）か 'pending'（未送信）
    status TEXT NOT NULL,
    issue_number INTEGER,
    issue_url TEXT,
    error TEXT
);
