-- 論文の付箋（どこまで読んだか）。版違いでも同じ論文として扱うので、paper_id は版なしの ID。
-- block は本文のブロック要素の通し番号、snippet はその書き出し（番号がずれたときに探し直す）
CREATE TABLE IF NOT EXISTS paper_bookmarks (
    paper_id TEXT PRIMARY KEY NOT NULL,
    block INTEGER NOT NULL,
    snippet TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
