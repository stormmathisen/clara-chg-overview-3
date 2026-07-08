mod app;
mod controls;
mod strip_chart;
mod util;
mod ws_client;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let Some(window) = web_sys::window() else {
            log::error!("No window object available");
            return;
        };
        let Some(document) = window.document() else {
            log::error!("No document object available");
            return;
        };

        let Some(canvas) = document
            .get_element_by_id("the_canvas_id")
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("Canvas element 'the_canvas_id' not found");
            if let Some(body) = document.body() {
                body.set_inner_html(
                    "<h2 style='color:red;padding:2em'>Error: Canvas element not found. \
                     Check index.html has &lt;canvas id=\"the_canvas_id\"&gt;</h2>",
                );
            }
            return;
        };

        if let Err(e) = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(app::ChargeOverviewApp::new(cc)))),
            )
            .await
        {
            log::error!("Failed to start eframe: {e:?}");
        }
    });

    Ok(())
}
