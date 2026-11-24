-- 英文の解説で検出した文法タグ（DetectedTag の JSON 配列）。mode=word や旧データは NULL
ALTER TABLE explain_cache ADD COLUMN tags TEXT;

-- 履歴の英文で検出した文法タグ id の JSON 配列（出現回数の集計に使う）
ALTER TABLE history ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
