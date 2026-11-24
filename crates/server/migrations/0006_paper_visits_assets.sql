-- 開いた論文の記録（「見た論文」の一覧）。キャッシュとは別に持ち、キャッシュを消しても記録は残る
CREATE TABLE IF NOT EXISTS paper_visits (
    paper_id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    source_url TEXT NOT NULL,
    first_opened_at INTEGER NOT NULL,
    last_opened_at INTEGER NOT NULL,
    open_count INTEGER NOT NULL DEFAULT 1
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS paper_visits_last ON paper_visits (last_opened_at DESC);

-- これまでにキャッシュした論文は、開いたことがある論文として登録しておく
INSERT OR IGNORE INTO paper_visits (paper_id, title, source_url, first_opened_at, last_opened_at, open_count)
SELECT id, title, source_url, fetched_at, fetched_at, 1 FROM paper_cache;

-- 論文の図（画像）のキャッシュ。サーバー経由で配信し、2 回目からは arXiv にアクセスしない
CREATE TABLE IF NOT EXISTS paper_assets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL UNIQUE,
    paper_id TEXT NOT NULL,
    content_type TEXT NOT NULL,
    data BLOB NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS paper_assets_paper ON paper_assets (paper_id);
