-- 付箋を行に付ける。char_offset は、ブロックの中の何文字目（UTF-16 の位置）の行かを表す。
-- 行の折り返しは画面の幅で変わるので、行の番号ではなく文字の位置で持つ。以前の付箋は 0（ブロックの 1 行目）
ALTER TABLE paper_bookmarks ADD COLUMN char_offset INTEGER NOT NULL DEFAULT 0;
