use std::env;
use std::path::PathBuf;

extern crate cbindgen;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = PathBuf::from(&env::var("OUT_DIR").unwrap());

    let config = cbindgen::Config::from_file("cbindgen.toml").unwrap_or_default();

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(out_dir.join("blockstore.h"));
}
