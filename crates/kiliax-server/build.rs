use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let dist_dir = std::env::var("KILIAX_WEB_DIST_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("..").join("..").join("web").join("dist"));

    let generated = out_dir.join("embedded_web.rs");
    println!("cargo:rerun-if-env-changed=KILIAX_WEB_DIST_DIR");
    println!("cargo:rerun-if-changed={}", dist_dir.display());

    if !dist_dir.join("index.html").is_file() {
        write_disabled(&generated);
        return;
    }

    let embed_root = out_dir.join("web-dist");
    let _ = fs::remove_dir_all(&embed_root);
    fs::create_dir_all(&embed_root).expect("create embed root");

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    walk_files(&dist_dir, &dist_dir, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel, src) in &entries {
        println!("cargo:rerun-if-changed={}", src.display());
        let dst = embed_root.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::copy(src, &dst).expect("copy dist file");
    }

    let mut f = fs::File::create(&generated).expect("create embedded_web.rs");
    writeln!(
        f,
        "pub const ENABLED: bool = true;\n\
         \n\
         pub fn index_html() -> &'static [u8] {{\n\
           get(\"/index.html\").expect(\"index.html\")\n\
         }}\n\
         \n\
         pub fn get(path: &str) -> Option<&'static [u8]> {{\n\
           match path {{"
    )
    .unwrap();

    for (i, (rel, _)) in entries.iter().enumerate() {
        let key = format!("/{}", rel.replace('\\', "/"));
        let ident = format!("FILE_{i}");
        writeln!(f, "    \"{key}\" => Some({ident}),").unwrap();
    }

    writeln!(f, "    _ => None,\n  }}\n}}\n").unwrap();

    for (i, (rel, _)) in entries.iter().enumerate() {
        let rel = rel.replace('\\', "/");
        let ident = format!("FILE_{i}");
        let path = format!("{}/web-dist/{}", out_dir.display(), rel);
        writeln!(
            f,
            "static {ident}: &[u8] = include_bytes!({path:?});"
        )
        .unwrap();
    }
}

fn write_disabled(path: &Path) {
    let mut f = fs::File::create(path).expect("create embedded_web.rs (disabled)");
    f.write_all(
        b"pub const ENABLED: bool = false;\n\
          pub fn index_html() -> &'static [u8] { b\"\" }\n\
          pub fn get(_path: &str) -> Option<&'static [u8]> { None }\n",
    )
    .expect("write disabled embedded_web.rs");
}

fn walk_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            walk_files(root, &path, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        if path.file_name() == Some(OsStr::new(".DS_Store")) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        out.push((rel.to_string_lossy().to_string(), path));
    }
}
