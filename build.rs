use std::env;

fn main() {
    let uci_enabled = env::var_os("CARGO_FEATURE_UCI").is_some();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".into());

    if uci_enabled && target_os != "linux" {
        println!(
            "cargo::warning=feature `uci` is unsupported for target `{target}`; \
             UCI integration will not be compiled"
        );
    }

    println!("cargo::rerun-if-changed=build.rs");
}
