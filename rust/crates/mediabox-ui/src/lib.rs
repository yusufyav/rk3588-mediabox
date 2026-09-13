//! MediaBox — the appliance's product interface.

pub mod api;
pub mod app;
pub mod components;
pub mod focus;
pub mod model;
pub mod screens;

use wasm_bindgen::prelude::*;

/// The media core's identifier for the appliance's own library.
pub const LIBRARY_ADDON_ID: &str = "mediabox.library";

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook();
    if let Some(document) = web_sys::window().and_then(|window| window.document())
        && let Some(boot) = document.get_element_by_id("boot")
    {
        boot.remove();
    }
    mark_television();
    leptos::mount::mount_to_body(app::App);
}

/// Say whether this page is the television's own kiosk.
///
/// It cannot be worked out from the browser. The kiosk reports `hover: hover`
/// and `pointer: fine` exactly as a desktop does — measured on the appliance —
/// so a media query that tried to tell them apart would put a television's
/// behaviour on somebody's laptop, or a laptop's on the television. The kiosk
/// is therefore asked to say so: it loads the page with `?tv=1`, and only then
/// does the interface stop scrolling and start moving itself.
///
/// Everything that follows from this flag is in the stylesheet under `body.tv`
/// and in `focus::reveal`. Without it the page is an ordinary scrolling page,
/// which is what a phone and a laptop need.
fn mark_television() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let asked = window
        .location()
        .search()
        .map(|search| search.contains("tv=1"))
        .unwrap_or(false);
    if !asked {
        return;
    }
    if let Some(body) = window.document().and_then(|document| document.body()) {
        let _ = body.class_list().add_1("tv");
    }
}

/// Turn a Rust panic into something readable in the browser console instead of
/// the default `unreachable executed`.
fn console_error_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
    }));
}
