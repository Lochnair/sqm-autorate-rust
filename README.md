# unRAVEL

unRAVEL adjusts CAKE traffic shaping rates to keep latency under control on connections with fluctuating bandwidth. RAVEL stands for "Rate Adaptation Via Event-driven Logic", with a lowercase "un" in front. OpenWrt is the main target, though it can also run on other Linux systems with CAKE configured.

If your internet connection changes speed throughout the day, finding a good bandwidth setting for CAKE can be a bit of a pain. Set it too high and latency goes up when the connection slows down. Set it too low and you're giving up bandwidth when there's more available. unRAVEL watches traffic load and latency measurements from remote hosts (reflectors), then adjusts upload and download rates as conditions change.

This project started as a Rust port of [sqm-autorate](https://github.com/sqm-autorate/sqm-autorate), the original Lua project. The runtime separates measurement from rate control, with ICMP and IRTT measurement engines and a control law based on sqm-autorate. You don't need the Lua program installed, and you'll need to configure this version separately if you're moving over from it.

## Before you start

You'll need a Linux router with CAKE already configured on the interfaces you want to control. On OpenWrt, you can set this up through SQM. unRAVEL changes the bandwidth of an existing root CAKE qdisc, so your SQM setup still handles things like overhead compensation and traffic redirection to the download IFB.

Check the interfaces before starting (replace these names with yours):

```sh
tc qdisc show dev wan
tc qdisc show dev ifb4wan
```

Both should show CAKE at the root if you're controlling both directions. Run unRAVEL as root, since it needs access to raw ICMP sockets and traffic control. Stop any other autorate program controlling the same qdiscs.

## Installation

### OpenWrt package

Download the package for your OpenWrt release and package architecture from the [openwrt-feeds build artifacts](https://github.com/Lochnair/openwrt-feeds/actions/workflows/build.yml). Extract the artifact and copy the `unravel` package to `/tmp` on your router.

On a router using opkg, install the `.ipk` package:

```sh
opkg update
opkg install /tmp/unravel_*.ipk
```

On a router using [APK](https://openwrt.org/docs/guide-user/additional-software/apk), install the `.apk` package instead:

```sh
apk update
apk add --allow-untrusted /tmp/unravel*.apk
```

The build artifacts contain unsigned packages, hence `--allow-untrusted` for APK. The package includes the binary, UCI configuration and init script. Edit the configuration before starting the service.

### Building with the OpenWrt SDK

Use an [SDK](https://openwrt.org/docs/guide-developer/toolchain/using_the_sdk) matching your OpenWrt release and target, with Rust package support available. The package builds use the [OpenWrt Rust SDK images](https://github.com/Lochnair/openwrt-sdk-rust).

From the extracted SDK directory, add the [package feed](https://github.com/Lochnair/openwrt-feeds):

```sh
[ -f feeds.conf ] || cp feeds.conf.default feeds.conf
echo 'src-git lochnair https://github.com/Lochnair/openwrt-feeds.git' >> feeds.conf
./scripts/feeds update -a
./scripts/feeds install unravel
make menuconfig
```

Select `unravel` under Utilities, save the configuration, then build:

```sh
make package/unravel/compile V=s
```

The resulting package is under `bin/packages/`. Copy it to your router and install it as shown above.

### Building with Cargo

For other Linux systems, use a current stable Rust toolchain. The default build includes TOML, IRTT and a bundled copy of UCI. On Debian or Ubuntu, install the build dependencies first:

```sh
sudo apt install build-essential cmake libclang-dev pkg-config
```

The clone URL below requires the planned GitHub repository rename to `Lochnair/unravel`:

```sh
git clone https://github.com/Lochnair/unravel.git
cd unravel
cargo build --release --locked
sudo install -m 755 target/release/unravel /usr/sbin/unravel
sudo mkdir -p /etc/unravel
sudo install -m 644 reflectors.csv /etc/unravel/reflectors.csv
```

If you only want TOML configuration and don't need UCI, build with `cargo build --release --locked --no-default-features --features toml,irtt` to skip the UCI build dependencies.

## Configuration

On OpenWrt, edit `/etc/config/unravel`. If you're moving from `sqm-autorate-rust`, check the [migration notes](MIGRATION.md) first.

For builds with UCI and TOML support, TOML settings override UCI and environment variables override both. The TOML file is `/etc/unravel/config.toml`. You only need to use one source, so on OpenWrt you can stick with UCI.

### UCI

Set your interface names and bandwidths in `/etc/config/unravel`:

```uci
config network
    option download_interface 'ifb4wan'
    option upload_interface 'wan'
    option download_base_kbits '50000'
    option upload_base_kbits '10000'
    option download_min_percent '20'
    option upload_min_percent '20'

config output
    option log_level 'info'
```

The bandwidth values are in kbit/s, so this example uses 50 Mbit/s download and 10 Mbit/s upload. Set the base rates to what you'd normally configure in SQM. These are reference rates for the controller, and it can increase the shapers above them when load and delay allow it.

The minimum is a percentage of the base rate, 20% by default. With these values the shapers can go down to 10 Mbit/s download and 2 Mbit/s upload. Choose minima that leave you with a usable connection when capacity drops.

### TOML

For a manual install, you can put the equivalent settings in `/etc/unravel/config.toml`:

```toml
[network]
download_interface = "ifb4wan"
upload_interface = "wan"
download_base_kbits = 50000
upload_base_kbits = 10000
download_min_percent = 20
upload_min_percent = 20

[output]
log_level = "info"
```

### Controlling one direction

If you only want to control upload, set `option download_interface 'none'` in UCI, or `download_interface = "none"` in TOML. You can then omit `download_base_kbits` and `download_min_percent`. The same applies to `upload_interface` if you only want download shaping. At least one direction must be enabled.

### Environment variables

Use `UNRAVEL__SECTION__OPTION`, with two underscores between each part. With the service stopped, you can try it without changing the shapers:

```sh
env UNRAVEL__ADVANCED_SETTINGS__DRY_RUN=true \
    UNRAVEL__OUTPUT__LOG_LEVEL=info /usr/sbin/unravel
```

Dry run still needs the configured interfaces and CAKE qdiscs to exist. It also sends probes and writes diagnostics.

## Running it

After configuring the OpenWrt package, start the service and enable it at boot:

```sh
/etc/init.d/unravel start
/etc/init.d/unravel enable
```

To try it in the foreground, stop the service first, then run the binary:

```sh
/etc/init.d/unravel stop
/usr/sbin/unravel
```

For a manual install on another Linux system, run `sudo /usr/sbin/unravel`. A manual binary install doesn't include a service definition.

The program initially uses conservative shaping rates while collecting measurements. Then try a download or upload and watch the rates with `tc -s qdisc show dev wan` (and your download interface).

Ctrl+C or SIGTERM stops the program and attempts to restore the enabled shapers to your configured base rates. When running through the OpenWrt service, use `/etc/init.d/unravel stop` to stop it.

## Reflectors and measurement protocols

By default, the program uses ICMP timestamps and selects five reflectors, or fewer if there aren't enough candidates. Bootstrap hosts get priority at startup. When there are more candidates than needed, it periodically tries other hosts from the combined bootstrap and configured inventory, temporarily probing extra hosts during selection.

If timestamps are blocked on your network, select `icmp-echo`. Echo uses round-trip measurements, so it can't separate upload and download delay the way timestamps or IRTT can. You can also select `irtt` to use IRTT servers. Set the protocol in UCI:

```uci
config advanced_settings
    option measurement_type 'irtt'
```

Or in TOML:

```toml
[advanced_settings]
measurement_type = "irtt"
```

The choices are `icmp-timestamps` (default), `icmp-echo` and `irtt`. There's no automatic switch to echo if timestamps don't work. ICMP measurements use IPv4; IRTT supports IPv4 and IPv6, and its client is built into the default binary.

You can add your own reflectors with `[[reflectors.entries]]` in TOML or repeated `config reflector` sections in UCI:

```uci
config reflector
    option host 'global.irtt.lochnair.net'
    list protocols 'irtt'
```

This adds to the CSV inventory. To skip file loading and use the built-in bootstrap hosts plus any inline entries, use `[reflectors]` with `files = []` in TOML, or a `config reflectors` section with `option files 'none'` in UCI. UCI drops empty string options, so `option files ''` won't disable file loading.

To load a different CSV file in UCI, use `list files '/path/to/reflectors.csv'` under `config reflectors`. For IRTT entries, absent or blank `irtt_hmac` values use unauthenticated sessions.

## Logs and statistics

The default log level is `error`; the examples use `info` so you can see more of what's happening during setup. Logs go to stdout.

CSV statistics are enabled by default and are independent of network metrics export:

- `/tmp/unravel.csv` contains traffic load, delay above baseline and shaping rates.
- `/tmp/unravel-speedhist.csv` contains the speed history snapshots.

These files are recreated on startup. You can change their paths with `output.stats_file` and `output.speed_hist_file`, or disable both with `output.suppress_statistics = true`.

To send metrics to a collector accepting InfluxDB line protocol, add this to your TOML configuration (change the host to yours):

```toml
[observability]
enabled = true
protocol = "udp"
host = "192.168.1.100"
port = 8089
```

TCP is also supported. Rate metrics and events are exported by default when enabled; add `export_ping_metrics = true` or `export_baseline_metrics = true` if you need those too.

## License

[Mozilla Public License 2.0](LICENSE), as used by the original sqm-autorate project.
