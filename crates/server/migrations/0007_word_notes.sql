-- 単語帳: 辞書で引いた語の検索回数と、辞書を補う情報（カタカナの読み、辞書に無い語の訳）。
-- kana_source / meanings_source は 'llm'（ローカル LLM の推定）か 'user'（利用者が登録）。
-- meanings は JSON の文字列配列。ai_checked_at は LLM に問い合わせた時刻（同じ語を何度も問い合わせない）
CREATE TABLE IF NOT EXISTS word_notes (
    word TEXT PRIMARY KEY NOT NULL,
    kana TEXT,
    kana_source TEXT,
    meanings TEXT,
    meanings_source TEXT,
    lookup_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_looked_at INTEGER NOT NULL,
    ai_checked_at INTEGER
) WITHOUT ROWID;
-- 検索回数の少ない語から消すための索引
CREATE INDEX IF NOT EXISTS word_notes_rank ON word_notes (lookup_count, last_looked_at);
CREATE INDEX IF NOT EXISTS word_notes_recent ON word_notes (last_looked_at DESC);
