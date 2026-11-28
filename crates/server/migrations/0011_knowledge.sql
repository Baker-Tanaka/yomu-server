-- 知識（RAG）。解説から LLM が整理した用語と要点。embedding は f32 リトルエンディアンの列で、
-- embed_model はそれを作った埋め込みモデル（EMBED_MODEL を変えたら起動時に作り直す）
CREATE TABLE IF NOT EXISTS knowledge (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    term TEXT NOT NULL,
    summary TEXT NOT NULL,
    source_text TEXT NOT NULL DEFAULT '',
    paper_id TEXT,
    created_at INTEGER NOT NULL,
    embedding BLOB,
    embed_model TEXT NOT NULL DEFAULT ''
);
