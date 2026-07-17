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

    match target_os.as_str() {
        "linux" => {}
        "macos" => println!(
            "cargo::warning=traffic control is unavailable on macOS; requested rate changes will be calculated but not applied"
        ),
        _ => println!(
            "cargo::warning=target `{target}` has no native interface-statistics or traffic-control backend; synthetic interface load will be used and requested rate changes will not be applied"
        ),
    }

    println!("cargo::rerun-if-changed=build.rs");
}
