#![warn(clippy::all, rust_2018_idioms)]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    eframe::run_native(
        "Benchy",
        eframe::NativeOptions::default(),
        Box::new(|context| Ok(Box::new(benchy_gui::App::new(context)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast as _;

    eframe::WebLogger::init(log::LevelFilter::Debug).ok();
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("window is unavailable")
            .document()
            .expect("document is unavailable");
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("the_canvas_id is missing")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id is not a canvas");

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|context| Ok(Box::new(benchy_gui::App::new(context)))),
            )
            .await;

        if let Some(loading) = document.get_element_by_id("loading_text") {
            loading.remove();
        }
        if let Err(error) = result {
            panic!("failed to start Benchy: {error:?}");
        }
    });
}
