use std::{env, fs, path::Path};

fn content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("md") => "text/markdown",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Embeds the pinned release metadata only. The archive itself is downloaded
/// and verified on the node after installation, keeping the plugin small.
fn server_release(generated: &mut String) {
    for key in [
        "CODE_SERVER_URL",
        "CODE_SERVER_SIZE",
        "CODE_SERVER_SHA256",
        "CODE_SERVER_VERSION",
        "CODE_SERVER_ROOT",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    println!("cargo:rerun-if-changed=runtime/launcher.mjs");
    generated.push_str(&format!(
        "pub static LAUNCHER: &str = include_str!({:?});\n",
        Path::new(&env::var("CARGO_MANIFEST_DIR").expect("manifest directory"))
            .join("runtime/launcher.mjs")
            .to_string_lossy()
    ));
    let read = |key: &str| env::var(key).ok().filter(|value| !value.is_empty());
    let url = read("CODE_SERVER_URL").unwrap_or_default();
    let checksum = read("CODE_SERVER_SHA256").unwrap_or_default();
    let version = read("CODE_SERVER_VERSION").unwrap_or_default();
    let root = read("CODE_SERVER_ROOT").unwrap_or_default();
    let size: u64 = read("CODE_SERVER_SIZE")
        .map(|value| value.parse().expect("CODE_SERVER_SIZE must be a number"))
        .unwrap_or(0);
    if !url.is_empty() {
        assert!(url.starts_with("https://"), "CODE_SERVER_URL must be https");
        assert!(
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "CODE_SERVER_SHA256 must be a SHA-256 hex digest"
        );
        assert!(
            !version.is_empty() && !root.is_empty(),
            "code-server build metadata is empty"
        );
    }
    generated.push_str(&format!("pub static CODE_SERVER_URL: &str = {url:?};\n"));
    generated.push_str(&format!("pub static CODE_SERVER_SIZE: u64 = {size};\n"));
    generated.push_str(&format!(
        "pub static CODE_SERVER_SHA256: &str = {checksum:?};\n"
    ));
    generated.push_str(&format!(
        "pub static CODE_SERVER_VERSION: &str = {version:?};\n"
    ));
    generated.push_str(&format!("pub static CODE_SERVER_ROOT: &str = {root:?};\n"));
}

fn main() {
    let root = env::var("CARGO_MANIFEST_DIR").expect("manifest directory");
    let mut entries = Vec::new();
    for (directory, prefix, extensions) in [
        ("ui/dist", "ui", &["js", "css"][..]),
        ("ui", "ui", &["png"][..]),
        ("ui/dist/i18n", "i18n", &["json"][..]),
        ("docs", "docs", &["md"][..]),
    ] {
        println!("cargo:rerun-if-changed={directory}");
        let Ok(files) = fs::read_dir(Path::new(&root).join(directory)) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let extension = Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if extensions.contains(&extension) {
                entries.push((
                    format!("{prefix}/{name}"),
                    file.path().to_string_lossy().into_owned(),
                ));
            }
        }
    }
    entries.sort();
    let mut generated = String::from("pub static UI_ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for (name, path) in &entries {
        generated.push_str(&format!(
            "    ({name:?}, {:?}, include_bytes!({path:?})),\n",
            content_type(name)
        ));
    }
    generated.push_str("];\n");
    server_release(&mut generated);
    let output = Path::new(&env::var("OUT_DIR").expect("output directory")).join("ui_assets.rs");
    fs::write(output, generated).expect("write embedded asset index");
    println!("cargo:rerun-if-changed=ui/manifest.json");
    println!("cargo:rerun-if-changed=i18n");
    println!("cargo:rerun-if-changed=docs");
}
