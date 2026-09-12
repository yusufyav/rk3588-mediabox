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
    leptos::mount::mount_to_body(app::App);
}

/// Turn a Rust panic into something readable in the browser console instead of
/// the default `unreachable executed`.
fn console_error_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
    }));
}
