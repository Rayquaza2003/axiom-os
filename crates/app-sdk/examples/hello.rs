//! Minimal native app: searches and logs the top titles.
//! Build: `cargo build --release --example hello`, then point manifest `cmd` at the binary.

fn main() {
    match axiom_app::search("revenue") {
        Ok(hits) => {
            for h in hits.as_array().into_iter().flatten() {
                axiom_app::log(&format!("hit: {}", h["title"].as_str().unwrap_or("?")));
            }
        }
        Err(e) => axiom_app::log(&format!("search refused: {e}")),
    }
}
