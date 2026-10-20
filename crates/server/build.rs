//! The web GUI is embedded from crates/gui/dist, which `trunk build` writes.
//! Make sure it exists, so debug builds read it from disk whenever it's
//! built (rust-embed skips a missing folder for good), and rebuild when it
//! changes, so release builds embed the latest one.

fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gui/dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        println!("cargo::warning=can't create {}: {e}", dist.display());
    }
    println!("cargo::rerun-if-changed={}", dist.display());
}
