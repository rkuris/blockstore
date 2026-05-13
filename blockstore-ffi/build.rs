use std::env;
use std::fs;
use std::path::{Path, PathBuf};

extern crate cbindgen;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set");

    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=build.rs");
    let src_dir = PathBuf::from(&crate_dir).join("src");
    rerun_if_rust_changed(&src_dir);

    let config = cbindgen::Config::from_file("cbindgen.toml").expect("cbindgen.toml is present");

    let bindings = cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("unable to generate bindings");

    bindings.write_to_file("src/blockstore.h");
}

fn rerun_if_rust_changed(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rerun_if_rust_changed(&path);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
