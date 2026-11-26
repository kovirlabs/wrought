//! With `visual-editor`: names for the editor's embedded files that change
//! with their content (`editor-<fnv32>.mjs`, ...), since `/_engine/ui/`
//! serves files as immutable. The bundle itself is built from `editor/`
//! by `scripts/build-editor.sh` and committed, so building the engine
//! never needs Node.

use std::path::Path;

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dest = Path::new(&out).join("editor_assets.rs");
    if std::env::var_os("CARGO_FEATURE_VISUAL_EDITOR").is_none() {
        std::fs::write(&dest, "").expect("writing editor_assets.rs");
        return;
    }
    let mut code = String::new();
    for (konst, file, stem, ext) in [
        ("EDITOR_JS", "editor-dist/editor.mjs", "editor", "mjs"),
        ("EDITOR_CSS", "editor-dist/editor.css", "editor", "css"),
        ("FRAME_CSS", "editor-frame.css", "editor-frame", "css"),
        (
            "LICENSES",
            "editor-dist/editor-licenses.txt",
            "editor-licenses",
            "txt",
        ),
    ] {
        println!("cargo:rerun-if-changed={file}");
        let body = std::fs::read(file).unwrap_or_else(|e| {
            panic!(
                "{file}: {e}. The editor bundle is committed to the repository; \
                 rebuild it with scripts/build-editor.sh"
            )
        });
        code.push_str(&format!(
            "pub(crate) const {konst}: &str = \"{stem}-{:08x}.{ext}\";\n",
            fnv1a(&body)
        ));
    }
    std::fs::write(&dest, code).expect("writing editor_assets.rs");
}
