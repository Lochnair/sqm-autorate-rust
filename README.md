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

## Reflector files and inline UCI reflectors

Inline `config reflector` sections supplement the default
`/etc/sqm-autorate-rust/reflectors.csv`. To use only inline reflectors, explicitly
disable file loading:

```uci
config reflectors
    option files 'none'

config reflector
    option host 'global.irtt.lochnair.net'
    list protocols 'irtt'
```

Use `list files '/path/to/reflectors.csv'` instead of `option files 'none'` to
combine CSV files with inline reflectors. Omitting `files` keeps the default CSV;
UCI discards empty string options, so `option files ''` does not disable it.
Absent or blank `irtt_hmac` values use unauthenticated IRTT sessions.

## Logging

RAVEL uses plain-text tracing events on stdout with UTC timestamps, severity,
source file/line, and the message. The existing TOML/UCI `output.log_level`
values and default (`error`) are unchanged; filtering also applies to dependency
`log` records. There is no `RUST_LOG` override. The startup banner is an info event.

A dedicated tracing-appender OS thread writes the output. Its bounded queue holds
1024 lines and drops new messages when full, so a slow or closed consumer does
not apply I/O backpressure to network control or shaper restoration. Formatting
and enqueueing still run on the calling thread.

Logging is best effort, including at process exit. The worker guard is deliberately
retained in a static for the whole process lifetime and is never dropped. There
is no shutdown flush, join, or exit delay; queued messages, including final shutdown
messages, may be lost. This avoids tracing-appender 0.2.5's synchronous `println!`
in the guard's shutdown-timeout path. The retained guard keeps a queue sender
alive, preventing both worker disconnection and its final-flush error-reporting
path. Ordinary output errors are ignored by the upstream worker loop. Review
these implementation details before updating the pinned tracing-appender version.

Upstream's constructor still panics if its OS logging thread cannot be spawned;
there is no fallible constructor. This is a resource-exhaustion initialization
limitation, separate from output failures. Subscriber/bridge initialization errors
are returned normally. Logging does not provide crash durability or a hard
allocation/formatting latency bound.
