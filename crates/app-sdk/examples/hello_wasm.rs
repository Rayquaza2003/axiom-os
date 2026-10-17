//! The same app as `hello`, for the wasm runtime.
//! Build: `cargo build --release --example hello_wasm --target wasm32-unknown-unknown`, manifest `"wasm": "hello_wasm.wasm"`.

#[unsafe(no_mangle)]
pub extern "C" fn run() {
    match axiom_app::search("revenue") {
        Ok(hits) => {
            for h in hits.as_array().into_iter().flatten() {
                axiom_app::log(&format!("hit: {}", h["title"].as_str().unwrap_or("?")));
            }
        }
        Err(e) => axiom_app::log(&format!("search refused: {e}")),
    }
}
