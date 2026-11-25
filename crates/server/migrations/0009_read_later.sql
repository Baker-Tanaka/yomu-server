-- 後で読む論文。版違いでも同じ論文として扱うので、paper_id は版なしの ID。
-- seq は積んだ順（大きいほど上）。authors は JSON の配列
CREATE TABLE IF NOT EXISTS read_later (
    paper_id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    authors TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    seq INTEGER NOT NULL
) WITHOUT ROWID;
