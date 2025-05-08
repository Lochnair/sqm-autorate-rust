## Summary

**sqm‑autorate‑rust** is a Rust re‑implementation of the original sqm‑autorate tool, which dynamically adjusts CAKE SQM bandwidth settings by measuring traffic load and latency – ideal for variable DOCIS/cable or LTE/wireless links where capacity fluctuates. This port preserves the original’s adaptive control logic while leveraging Rust’s type safety, performance, and concurrency primitives to deliver a more robust, maintainable codebase.

## Features

* **Adaptive rate control**: continuously measures one‑way delay baselines and traffic load to tune both download and upload shaping rates in real­time .
* **Multiple measurement types**: prefers ICMP timestamps by default, but also supports ICMP echo requests as a fallback for networks where timestamps are blocked
* **Concurrent architecture**: separates packet sending, receiving, baselining, reflector reselection and rate‑controller loops into dedicated threads for maximum responsiveness .
* **Configurable via environment or UCI**: reads settings from environment variables or OpenWRT UCI, with sensible defaults and type‑safe parsing .
* **Minimal dependencies**: relies on Rust crates only (anyhow, socket2, neli, rustix), no external scripts .

## Installation

You can install **sqm‑autorate‑rust** in two ways:

### 1. Native Rust build

1. Ensure you have the Rust toolchain (1.70+), Cargo, and `libnl` headers installed on your Linux host.
2. Clone and build:

   ```bash
   git clone https://github.com/Lochnair/sqm-autorate-rust.git
   cd sqm-autorate-rust
   cargo build --release
   ```
3. The optimized binary is at `target/release/sqm-autorate-rust`.

### 2. OpenWrt SDK build

You can also build an OpenWrt package to install in your images with the SDK:

1. **Prepare the OpenWrt SDK**

   ```bash
   # Download and extract the matching SDK for your target platform
   tar xJf openwrt-sdk-*.tar.xz
   cd openwrt-sdk-*
   ```
2. **Add the ****************************`openwrt-feeds`**************************** repository**

   ```bash
   # Add the feed to feeds.conf.default
   echo 'src-link lochnair https://github.com/Lochnair/openwrt-feeds.git' >> feeds.conf.default
   ./scripts/feeds update lochnair
   ./scripts/feeds install sqm-autorate-rust
   ```
3. **Configure and build the package**

   ```bash
   make menuconfig        # navigate to Network → sqm-autorate-rust, enable it
   make package/sqm-autorate-rust/{clean,compile} V=s
   ```
4. **Install on your device**

   ```bash
   # Copy the generated .ipk from bin/packages/... to your router
   opkg install sqm-autorate-rust_*.ipk
   ```
