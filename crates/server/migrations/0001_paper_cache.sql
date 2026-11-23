CREATE TABLE IF NOT EXISTS paper_cache (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL,
    html        TEXT NOT NULL,
    source_url  TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL
);
