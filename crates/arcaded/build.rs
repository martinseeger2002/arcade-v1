//! Builds the table of the desktop app's window files (crates/arcade-desktop/dist), which arcaded
//! also serves at /ui/ for a browser on this computer (crate::webui). One copy of the window: the
//! app and the browser show the same files.
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            let name = rel.to_string_lossy().replace('\\', "/");
            if !name.starts_with('.') {
                out.push((name, p.canonicalize().unwrap_or(p.clone())));
            }
        }
    }
}

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../arcade-desktop/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = Vec::new();
    walk(&dist, &dist, &mut files);
    for (_, p) in &files {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    let mut src = String::from("pub(crate) static FILES: &[(&str, &[u8])] = &[\n");
    for (name, p) in &files {
        src.push_str(&format!("    ({name:?}, include_bytes!({:?})),\n", p.display().to_string()));
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("webui_files.rs");
    std::fs::write(out, src).unwrap();
}
