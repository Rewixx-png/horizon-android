//! Bakes the prepared Bedrock control sprites into the binary.
//!
//! `tools/android_bedrock_controls.py` writes `.local/android/bedrock-controls.zip`
//! from the pinned bedrock-samples revision. Embedding it means the touch HUD
//! always has the real sprites, even on a device where the extracted resources
//! are missing. Without the zip (a clean checkout) the module falls back to an
//! empty archive, and the GUI keeps the vanilla sheets only.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".local/android/bedrock-controls.zip");
    println!("cargo:rerun-if-changed={}", root.display());
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("bedrock-controls.zip");
    let bytes = std::fs::read(&root).unwrap_or_default();
    std::fs::write(&out, bytes).expect("writing the baked control pack");
}
