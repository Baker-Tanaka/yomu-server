//! 単語帳タブと、単語帳が上限に近いときの確認画面。

use eframe::egui::{self, Margin, RichText};
use paper_reader_shared::{WordNote, WordSort, WordSource};

use crate::app::notice;
use crate::state::{self, SharedRef, Tab, WordsStatus};
use crate::theme::{self, Palette};

enum WordAction {
    Toggle(String, bool),
    Edit(WordNote),
}

/// 絞り込みの欄の右に並び順の切り替えを置ける最小の幅
const SORT_BESIDE_MIN_WIDTH: f32 = 430.0;

/// 単語帳タブ。
pub fn words_tab(shared: &SharedRef, ui: &mut egui::Ui) {
    let p = theme::palette(ui.ctx());
    let ctx = ui.ctx().clone();
    if shared.borrow().words.stale {
        state::refresh_words(shared, &ctx);
    }

    // 絞り込みと並び順
    {
        let mut s = shared.borrow_mut();
        let w = &mut s.words;
        let mut changed = false;
        // 狭い画面では、並び順の切り替えが右にはみ出さないよう 2 行に分ける
        let narrow = ui.available_width() < SORT_BESIDE_MIN_WIDTH;
        let layout = if narrow {
            egui::Layout::top_down(egui::Align::Min)
        } else {
            egui::Layout::left_to_right(egui::Align::Center)
        };
        let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(size, layout, |ui| {
            let width = if narrow {
                ui.available_width()
            } else {
                ui.available_width() - 210.0
            };
            let edit = ui.add_sized(
                [width, 30.0],
                egui::TextEdit::singleline(&mut w.query)
                    .hint_text("単語を絞り込む（前方一致）")
                    .margin(Margin::symmetric(8, 6)),
            );
            changed |= edit.changed();
            changed |= theme::segmented(
                ui,
                &mut w.sort,
                &[
                    (WordSort::Count, "回数順"),
                    (WordSort::Recent, "最近"),
                    (WordSort::Alpha, "ABC"),
                ],
            );
        });
        if changed {
            w.stale = true;
        }
    }

    let status = shared.borrow().words.status.clone();
    let list = match status {
        WordsStatus::NotLoaded | WordsStatus::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("単語帳を読み込み中…").color(p.weak));
            });
            return;
        }
        WordsStatus::Error(e) => {
            notice(ui, &p, &format!("単語帳: {e}"));
            return;
        }
        WordsStatus::Loaded(list) => list,
    };

    ui.add_space(4.0);
    let store = list.store;
    ui.label(
        RichText::new(format!(
            "{} / {} 語を保存 · 上限を超えると、検索回数の少ない語から自動で削除します（自分で登録した語は残ります）",
            group(store.count),
            group(store.max)
        ))
        .size(12.0)
        .color(if store.near_full() { p.error } else { p.weak }),
    );
    ui.add_space(6.0);

    // 操作
    let (selected_n, busy, form_open) = {
        let s = shared.borrow();
        (s.words.selected.len(), s.words.busy, s.words.form_open)
    };
    ui.horizontal_wrapped(|ui| {
        if theme::subtle_button(
            ui,
            if form_open {
                "− 閉じる"
            } else {
                "＋ 単語を登録"
            },
        )
        .on_hover_text("辞書に無い語の訳や、読みを自分で登録します")
        .clicked()
        {
            let mut s = shared.borrow_mut();
            s.words.form_open = !form_open;
            s.words.message = None;
        }
        if !list.items.is_empty() && theme::subtle_button(ui, "表示中をすべて選ぶ").clicked()
        {
            let mut s = shared.borrow_mut();
            s.words
                .selected
                .extend(list.items.iter().map(|i| i.word.clone()));
        }
        if selected_n > 0 {
            if theme::subtle_button(ui, "選択を解除").clicked() {
                shared.borrow_mut().words.selected.clear();
            }
            ui.add_enabled_ui(!busy, |ui| {
                if theme::primary_button(ui, &format!("選んだ {selected_n} 語を削除"), !busy)
                    .clicked()
                {
                    state::delete_selected_words(shared, &ctx);
                }
            });
        }
    });

    egui::CollapsingHeader::new(
        RichText::new("検索回数の少ない語をまとめて削除")
            .size(13.0)
            .color(p.weak),
    )
    .id_salt("words_prune")
    .default_open(false)
    .show(ui, |ui| {
        let mut count = shared.borrow().words.prune_count;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("下位").size(13.5));
            ui.add(
                egui::DragValue::new(&mut count)
                    .range(1..=store.count.max(1))
                    .speed(10.0),
            );
            ui.label(RichText::new("語を").size(13.5));
            if theme::primary_button(ui, "削除", !busy && store.count > 0).clicked() {
                state::prune_words(shared, &ctx, count);
            }
        });
        ui.label(
            RichText::new("回数が同じなら、最後に引いたのが古い語から消します。自分で登録した語は消しません。")
                .size(12.0)
                .color(p.weak),
        );
        shared.borrow_mut().words.prune_count = count;
    });

    if form_open {
        ui.add_space(4.0);
        word_form(shared, ui, &p, busy);
    }
    if let Some((ok, msg)) = shared.borrow().words.message.clone() {
        ui.add_space(4.0);
        if ok {
            ui.label(RichText::new(format!("✓ {msg}")).size(13.0).color(p.ok));
        } else {
            notice(ui, &p, &msg);
        }
    }
    ui.add_space(6.0);

    if list.items.is_empty() {
        ui.label(
            RichText::new(if list.total == 0 && shared.borrow().words.query.trim().is_empty() {
                "まだ単語はありません。本文で単語を選ぶと、検索回数と読み・AI 訳がここに残ります。"
            } else {
                "条件に合う単語はありません。"
            })
            .color(p.weak),
        );
        return;
    }

    let selected = shared.borrow().words.selected.clone();
    let mut action = None;
    for item in &list.items {
        if let Some(a) = word_row(ui, &p, item, selected.contains(&item.word)) {
            action = Some(a);
        }
        ui.add_space(5.0);
    }
    match action {
        Some(WordAction::Toggle(word, on)) => {
            let mut s = shared.borrow_mut();
            if on {
                s.words.selected.insert(word);
            } else {
                s.words.selected.remove(&word);
            }
        }
        Some(WordAction::Edit(note)) => state::edit_word(shared, &note),
        None => {}
    }

    if (list.items.len() as i64) < list.total {
        ui.add_space(4.0);
        let loading = shared.borrow().words.loading_more;
        ui.horizontal(|ui| {
            if loading {
                ui.spinner();
            } else if theme::subtle_button(
                ui,
                &format!(
                    "さらに読み込む（残り {} 語）",
                    list.total - list.items.len() as i64
                ),
            )
            .clicked()
            {
                state::words_more(shared, &ctx);
            }
        });
    }
}

/// 単語の登録・編集の入力欄。
fn word_form(shared: &SharedRef, ui: &mut egui::Ui, p: &Palette, busy: bool) {
    let ctx = ui.ctx().clone();
    theme::card(ui, |ui| {
        let mut save = false;
        let mut cancel = false;
        {
            let mut s = shared.borrow_mut();
            let w = &mut s.words;
            ui.label(RichText::new("英単語").size(12.5).color(p.weak));
            ui.add(
                egui::TextEdit::singleline(&mut w.form_word)
                    .hint_text("例: modality")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new("読み（カタカナ・省略可）")
                    .size(12.5)
                    .color(p.weak),
            );
            ui.add(
                egui::TextEdit::singleline(&mut w.form_kana)
                    .hint_text("例: モダリティ")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new("訳（1 行に 1 つ、または「、」区切り・省略可）")
                    .size(12.5)
                    .color(p.weak),
            );
            ui.add(
                egui::TextEdit::multiline(&mut w.form_meanings)
                    .hint_text("例: 様式、様相")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new("同じ語がすでにあれば、入力した項目だけを置き換えます。自分で登録した語は自動では削除されません。")
                    .size(12.0)
                    .color(p.weak),
            );
            let can_save = !busy && !w.form_word.trim().is_empty();
            ui.horizontal(|ui| {
                save = theme::primary_button(ui, "登録", can_save).clicked();
                cancel = theme::subtle_button(ui, "キャンセル").clicked();
            });
        }
        if save {
            state::save_word(shared, &ctx);
        }
        if cancel {
            let mut s = shared.borrow_mut();
            s.words.form_open = false;
            s.words.form_word.clear();
            s.words.form_kana.clear();
            s.words.form_meanings.clear();
        }
    });
}

/// 単語帳の 1 行。
fn word_row(ui: &mut egui::Ui, p: &Palette, item: &WordNote, selected: bool) -> Option<WordAction> {
    let mut action = None;
    theme::card(ui, |ui| {
        let edit_or_toggle = egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                let mut on = selected;
                let toggled = ui
                    .checkbox(&mut on, "")
                    .changed()
                    .then(|| WordAction::Toggle(item.word.clone(), on));
                ui.label(
                    RichText::new(&item.word)
                        .size(16.5)
                        .strong()
                        .color(p.strong),
                );
                toggled
            },
            |ui| {
                let edit = ui
                    .add(egui::Button::new(RichText::new("編集").size(12.5)).frame(false))
                    .clicked()
                    .then(|| WordAction::Edit(item.clone()));
                ui.label(
                    RichText::new(format!("{} 回", item.lookup_count))
                        .size(12.0)
                        .color(p.weak),
                );
                edit
            },
        );
        action = edit_or_toggle.0.or(edit_or_toggle.1);
        // 読みは長くなるので、見出しの行とは分ける（同じ行だと右のボタンに重なる）
        if item.kana.is_some() || item.is_user() {
            ui.horizontal_wrapped(|ui| {
                if let Some(kana) = &item.kana {
                    ui.label(RichText::new(kana).size(13.0).color(p.weak));
                }
                if item.is_user() {
                    theme::chip(ui, "自分で登録", p.accent, p.accent_soft);
                }
            });
        }
        let (text, tag) = if !item.meanings.is_empty() {
            (
                item.meanings.join("；"),
                match item.meanings_source {
                    Some(WordSource::Llm) => Some("AI 訳"),
                    _ => None,
                },
            )
        } else if let Some(d) = &item.dict_summary {
            (d.replace(['『', '』'], ""), None)
        } else {
            (String::new(), None)
        };
        if !text.is_empty() {
            ui.horizontal(|ui| {
                if let Some(tag) = tag {
                    theme::chip(ui, tag, p.sentence, p.sentence_soft)
                        .on_hover_text("辞書に無い語のため、ローカル LLM が推定した訳です");
                }
                ui.add(egui::Label::new(RichText::new(text).size(13.5).color(p.text)).truncate());
            });
        }
    });
    action
}

/// 単語帳が上限に近いときの確認画面。
pub fn store_warning_modal(shared: &SharedRef, ctx: &egui::Context) {
    let Some(store) = shared.borrow().store_warning else {
        return;
    };
    let p = theme::palette(ctx);
    // 上限の 1 割（少なくとも 1 語）を消す提案
    let suggest = (store.max / 10).max(1);
    let resp = egui::Modal::new(egui::Id::unique("word_store_warning")).show(ctx, |ui| {
        ui.set_max_width(360.0);
        ui.label(
            RichText::new("単語帳がいっぱいに近づいています")
                .size(16.0)
                .strong()
                .color(p.strong),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "{} / {} 語を保存しています。上限を超えると、検索回数の少ない語から自動で削除されます（自分で登録した語は残ります）。",
                group(store.count),
                group(store.max)
            ))
            .size(13.5)
            .color(p.text),
        );
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, &format!("回数の少ない {} 語を削除", group(suggest)), true)
                .clicked()
            {
                state::prune_words(shared, ctx, suggest);
                state::dismiss_store_warning(shared);
            }
            if theme::subtle_button(ui, "単語帳で選んで削除").clicked() {
                state::dismiss_store_warning(shared);
                let mut s = shared.borrow_mut();
                s.tab = Tab::Words;
                s.words.sort = WordSort::Count;
                s.words.stale = true;
            }
            if theme::subtle_button(ui, "あとで").clicked() {
                state::dismiss_store_warning(shared);
            }
        });
    });
    if resp.should_close() {
        state::dismiss_store_warning(shared);
    }
}

fn group(n: i64) -> String {
    crate::app::group_digits(n.max(0) as u64)
}
