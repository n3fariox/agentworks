use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_MODEL_VIEWER");
    if env::var_os("CARGO_FEATURE_MODEL_VIEWER").is_none() {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../web");
    let assets = root.join("model-viewer-assets");
    println!("cargo:rerun-if-changed={}", assets.display());
    let mut files = Vec::new();
    fn collect(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("Run npm ci --prefix web && npm run build:model-viewer before building with --features model-viewer") {
            let path = entry.unwrap().path();
            if path.is_dir() { collect(&path, files); } else { files.push(path); }
        }
    }
    collect(&assets, &mut files);
    files.sort();
    let mut source = String::from("match name.as_str() {\n");
    for file in files {
        let name = file
            .strip_prefix(&assets)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let mime = match file.extension().and_then(|s| s.to_str()).unwrap_or("") {
            "js" => "text/javascript; charset=utf-8",
            "wasm" => "application/wasm",
            "jpg" => "image/jpeg",
            _ => "text/plain; charset=utf-8",
        };
        source.push_str(&format!(
            "{name:?} => Some(({mime:?}, include_bytes!({:?}).as_slice())),\n",
            file.canonicalize().unwrap()
        ));
    }
    source.push_str("_ => None,\n}");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("model_viewer_assets.rs"),
        source,
    )
    .unwrap();
}
