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

fn server_archive(generated: &mut String) {
    for key in [
        "CODE_SERVER_ARCHIVE",
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

    let archive = env::var("CODE_SERVER_ARCHIVE")
        .ok()
        .filter(|value| !value.is_empty());
    let Some(archive) = archive else {
        generated.push_str("pub static CODE_SERVER_ARCHIVE: &[u8] = &[];\n");
        generated.push_str("pub static CODE_SERVER_SHA256: &str = \"\";\n");
        generated.push_str("pub static CODE_SERVER_VERSION: &str = \"\";\n");
        generated.push_str("pub static CODE_SERVER_ROOT: &str = \"\";\n");
        return;
    };

    let checksum = env::var("CODE_SERVER_SHA256").expect("CODE_SERVER_SHA256 is required");
    assert!(
        checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "CODE_SERVER_SHA256 must be a SHA-256 hex digest"
    );
    let version = env::var("CODE_SERVER_VERSION").expect("CODE_SERVER_VERSION is required");
    let root = env::var("CODE_SERVER_ROOT").expect("CODE_SERVER_ROOT is required");
    assert!(
        !version.is_empty() && !root.is_empty(),
        "code-server build metadata is empty"
    );
    println!("cargo:rerun-if-changed={archive}");
    generated.push_str(&format!(
        "pub static CODE_SERVER_ARCHIVE: &[u8] = include_bytes!({archive:?});\n"
    ));
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
    server_archive(&mut generated);
    let output = Path::new(&env::var("OUT_DIR").expect("output directory")).join("ui_assets.rs");
    fs::write(output, generated).expect("write embedded asset index");
    println!("cargo:rerun-if-changed=ui/manifest.json");
    println!("cargo:rerun-if-changed=i18n");
    println!("cargo:rerun-if-changed=docs");
}
