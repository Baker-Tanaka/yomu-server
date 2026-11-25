//! paper-reader の右ペイン（egui）。wasm32-unknown-unknown 専用。

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(target_arch = "wasm32")]
mod local_llm;
#[cfg(target_arch = "wasm32")]
mod recent;
#[cfg(target_arch = "wasm32")]
mod search_input;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod theme;
#[cfg(target_arch = "wasm32")]
mod words_view;

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;

    eframe::WebLogger::init(log::LevelFilter::Info).ok();

    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("document が取得できません");
            return;
        };
        let Some(canvas) = document
            .get_element_by_id("egui_canvas")
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("#egui_canvas が見つかりません");
            return;
        };

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::PaperReaderApp::new(cc)))),
            )
            .await;
        if let Err(e) = result {
            log::error!("eframe の起動に失敗しました: {e:?}");
            dom::show_fatal_error(
                "解説パネルの起動に失敗しました。ブラウザが WebGL に対応しているか確認してください。",
            );
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("このクレートは wasm 専用です。`trunk serve` でビルドしてください。");
}
