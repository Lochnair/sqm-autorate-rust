## Netlink-bindings

Type-safe Rust bindings for encoding/decoding Netlink messages generated from
YAML [specifications].

[specifications]: https://docs.kernel.org/userspace-api/netlink/specs.html

## Overview

Netlink is a collection of APIs exposed by the kernel, unified by a similar
style of encoding data. The general list of Netlink families can be found in
[the kernel
documentation](https://docs.kernel.org/networking/netlink_spec/index.html), or
at least those families, that were possible to condense to a machine readable
description. This list very likely includes all the families that you would
want to interact with.

The goal of this project is to provide an easy-to-use type-safe interface,
while also being reasonably fast and supporting all properties of all
_sensible_ Netlink families.

## Features

- Streamlined type-safe interface.
- All the properties described in yaml specifications are available in Rust.
- Decoded Netlink messages can be Debug-printed, with enum values and flags
annotated.
- There is [a tool](#working-off-of-existing-tools) to simplify working off of
existing tools by decoding Netlink messages of other programs.
- Unified interface for both generic and classic flavors of netlink. Family
resolution is automatic.
- Many quirks of netlink-legacy and netlink-raw protocols are supported,
including: binary structures, nested types, multi-attribute arrays, indexed
arrays, sub messages.
- Zero unsafe code in encoder/decoder logic.

## Installation

```toml
[dependencies]
netlink-bindings = { version = "0.3", features = [ "wireguard" ] }
netlink-socket2 = { version = "0.3", features = [ ] }
```

## Making requests

A typical Netlink family, say wireguard, support multiple operations, for
example, "get-device" and "set-device". Each operation may also be of "do" or
"dump" kind.

For example, to get info about a device you would use a "dump" kind of
"get-device" request. That's usually what it means, although different
subsystems may imply different things. A typical request looks like this:

```rust
use netlink_bindings::wireguard;
use netlink_socket2::NetlinkSocket;

let mut sock = NetlinkSocket::new();

let ifname = "wg0";

// All available requests are conveniently accessible using `family::Request`
let mut request = wireguard::Request::new()
    .op_get_device_dump();

// Add contents to the request
request.encode()
    .push_ifname_bytes(ifname.as_bytes());

let mut iter = sock.request(&request).unwrap();
while let Some(res) = iter.recv() {
    // Each request may return an error (literal error code), in some cases
    // with some additional info from the kernel, e.g. lacking a permission,
    // if you missing CAP_NET_ADMIN capability for querying wireguard info.
    let attrs = res.unwrap();

    // A simple approach to get a specific property from an attribute set is
    // following. Note that it's not guaranteed that the property was supplied,
    // nor that it can be parsed correctly. If either occurs, the error will
    // include error context, i.e. name of the attribute and it's parent set.
    let listen_port = attrs.get_listen_port().unwrap();
    println!("Interface {ifname:?} is listening on {listen_port}");

    // Print out all the attributes using the debug formatter.
    println!("{:#?}", attrs);
}
```

Your LSP should be able to nicely suggest appropriate methods both for encoding
and decoding as you type.

## More complicated requests

Let's say you have a network interface and want to assign an ip address to it.
This is the domain of "rt-addr" family. It was one of the first ones created,
inheriting some now-discouraged quirks, like a fixed-header - a struct that
always present and may also carry some relevant data in some cases or may be
just ignored (zeroed-out) for other requests.

The relevant operation is "newaddr" with "do" kind. You may also notice
".set_change()". This specifies an additional request flags. Similar to
fixed-header, theses flags may invoke some additional behavior in certain
operations, or do nothing in others.

```rust,should_panic
use std::net::IpAddr;
use netlink_bindings::rt_addr;
use netlink_socket2::NetlinkSocket;

let mut sock = NetlinkSocket::new();

let addr: IpAddr = "10.0.0.1".parse().unwrap();

// Create fixed-header for the request
let header = rt_addr::Ifaddrmsg {
    ifa_index: 1234, // Acquired via "get-addr" request
    ifa_family: libc::AF_INET as u8, // aka ipv4
    ifa_prefixlen: 32, // stands for "/32" in "10.0.0.1/32"
    ..Default::default()
};

let mut request = rt_addr::Request::new()
    .set_change() // Don't fail if address already assigned
    .op_newaddr_do(&header);

request.encode()
    .push_local(addr);

sock.request(&request).unwrap()
    .recv_ack().unwrap();
```

See full code in the [example](./netlink-socket/examples/wireguard-setup.rs).

## Async sockets

Async functionality is available using the same interface, you just need to
enable it, and to add `.await` keyword in all places where async IO is expected.

```toml
[dependencies]
netlink-socket2 = { ... , features = [ "tokio" ] } # or "smol"
```

An earlier example, but using async, would look like this:

```rust,compile_fail
use netlink_bindings::wireguard;
use netlink_socket2::NetlinkSocket;

let mut sock = NetlinkSocket::new();

let mut request = wireguard::Request::new()
    .op_get_device_dump();

request.encode()
    .push_ifname_bytes(b"wg0");

let mut iter = sock.request(&request).await.unwrap();
while let Some(res) = iter.recv().await {
    println!("{:#?}", res);
}
```

## Other examples

- [wireguard-setup](./netlink-socket/examples/wireguard-setup.rs) - Create and
configure wireguard interface.
- [conntrack](./netlink-socket/examples/conntrack.rs) - Dump tracked network
connections, similar to `conntrack -L`.
- [extack](./netlink-socket/examples/extack.rs) - Showcase handing of extended
ack attributes in error reporting.
- [nftables](./netlink-socket/examples/nftables.rs) - Create nftables rules.
- [nftables-api](./netlink-socket/examples/nftables-api.rs) - A high-level
wrapper for creating nftables rules.
- [nl80211](./netlink-socket/examples/nl80211.rs) - Basic interactions with nl80211.
- [nl80211-raw](./netlink-socket/examples/nl80211-raw.rs) - Same as nl80211,
but manually encoding/decoding netlink messages.
- [tc-prio](./netlink-socket/examples/tc-prio.rs) - Add, show, and delete
traffic control queueing discipline.
- [tcp-rtt](./netlink-socket/examples/tcp-rtt.rs) - Dump socket information,
including RTT of a TCP socket.
- [multicast-simple](./netlink-socket/examples/multicast-simple.rs) - Listen
for multicast notifications emitted when a network device is created, changed,
or deleted.
- [multicast-generic](./netlink-socket/examples/multicast-generic.rs) - Listen
for all multicast notifications on a generic netlink subsystem.
- [multicast-raw](./netlink-socket/examples/multicast-rtnetlink.rs) - Listen
for multicast notifications on legacy rtnetlink subsystem.

## Attribute encoding

Under the hood, calling `.encode()` is just a convenience to pass the lead to
the correct encoding struct. The ecoder's job is to actually write the
attributes directly into provided buffer. All types relevant for encoding are
prefixed with `Push`. For example, directly encoding a "do" request of
"set-device" operation looks like the following:

```rust
use netlink_bindings::wireguard::{OpSetDeviceDo, WgdeviceFlags};

let mut vec = Vec::new();

// Do set-device (request)
OpSetDeviceDo::encode_request(&mut vec)
    .push_ifname(c"wg0") // &CStr
    // .push_ifname_bytes("wg0".as_bytes()) // &[u8]
    .push_flags(WgdeviceFlags::ReplacePeers as u32) // Remove existing peers
    .array_peers()
        .entry_nested()
        .push_public_key(&[/* ... */]) // &[u8]
        .push_endpoint("127.0.0.1:12345".parse().unwrap()) // SocketAddr
        .array_allowedips()
            .entry_nested()
            .push_family(libc::AF_INET as u16) // aka ipv4
            .push_ipaddr("0.0.0.0".parse().unwrap()) // IpAddr
            .push_cidr_mask(0) // stands for "/0" in "0.0.0.0/0"
            .end_nested()
            // More allowed ips...
        .end_array() // Explicitly closing isn't necessary
        .end_nested()
        // More peers...
    .end_array();
```

Additionally, check out [all available
methods](./netlink-bindings/src/wireguard/wireguard.md), along with the in-line
documentation.

## Attribute decoding

Similarly, under the hood, receiving a reply yield an attribute decoder. The
decoder itself is just a slice, therefore it can be cheaply cloned, copying
it's frame. The low-level interface is based on iterators, with nice-to-use
wrapper on top.

```rust,should_panic
use netlink_bindings::traits::NetlinkRequest;
use netlink_bindings::wireguard::OpGetDeviceDump;

let buf = vec![/* ... */];

// Dump get-device (reply)
let attrs = OpGetDeviceDump::decode_reply(&buf);

println!("Ifname: {:?}", attrs.get_ifname().unwrap()); // &CStr
for peer in attrs.get_peers().unwrap() {
    println!("Endpoint: {}", peer.get_endpoint().unwrap()); // SockAddr

    for addr in peer.get_allowedips().unwrap() {
        let ip = addr.get_ipaddr().unwrap(); // IpAddr
        let mask = addr.get_cidr_mask().unwrap(); // u8
        println!("Allowed ip: {ip}/{mask}");
    }
}
```

See full code in the [example](./netlink-bindings/examples/wireguard.rs). And
as previously, check out [all available
methods](./netlink-bindings/src/wireguard/wireguard.md), along with the in-line
documentation.

## Low-level decoding

A low-level decoding interface is exposed as an iterator, that yields enum
variants, containing either a target type, e.g. SockAddr, or another iterator,
in case of a nested attribute set. But as you can see, using it directly
quickly turns very ugly.

```rust
use netlink_bindings::traits::NetlinkRequest;
use netlink_bindings::wireguard::{OpGetDeviceDump, Wgdevice, Wgpeer};

let buf = vec![/* ... */];

for attr in OpGetDeviceDump::decode_reply(&buf) {
    match attr.unwrap() {
        Wgdevice::Ifname(n) => println!("Ifname: {n:?}"),
        Wgdevice::Peers(iter) => {
            for entry in iter {
                for attr in entry.unwrap() {
                    match attr.unwrap() {
                        Wgpeer::Endpoint(e) => println!("Endpoint: {e:?}"),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}
```

## Alternatives

- [Rust-netlink](https://github.com/rust-netlink).
- [Neli](https://github.com/jbaublitz/neli).

Both don't use codegen to generate bindings, hence many Netlink families are
not supported.

Another difference is that they represent netlink messages as lists of rust
enums, while this project works with the binary representation directly, with a
separate interfaces for encoding and decoding: a builder pattern-like interface
for encoding, and an iterator interface for decoding (internally).

## Support status

All [upstream specifications](https://www.kernel.org/doc/html/latest/netlink/specs/) are supported as of Linux 6.19.

- ✅ supported, has tests:
[conntrack](./netlink-socket/examples/conntrack.rs),
[inet-diag](./netlink-socket/examples/tcp-rtt.rs),
[nftables](./netlink-socket/examples/nftables.rs),
[nl80211](./netlink-socket/examples/nl80211.rs),
[nlctrl](./netlink-bindings/src/nlctrl/nlctrl.md),
[rt-addr](./netlink-socket/examples/wireguard-setup.rs),
[rt-link](./netlink-socket/examples/wireguard-setup.rs),
[tc](./netlink-socket/examples/tc-prio.rs),
[wireguard](./netlink-socket/examples/wireguard-setup.rs).
- ✔️ compiles, testing needed: binder, dev-energymodel, devlink, dpll, ethtool,
fou, handshake, lockd, mptcp_pm, netdev, net-shaper, nfsd, ovpn, ovs_datapath,
ovs_flow, ovs_vport, psp, rt-neigh, rt-route, rt-rule tcp_metrics, team,
unix-diag.

## Working off of existing tools

If there's an existing tool using Netlink, you can use `reverse-lookup` binary
from this project to decipher it's Netlink communications, and work off of
that. Let's say you want to inspect what `wg` command does.

```sh
$ strace -o ./output_file --decode-fd=socket -e %network --{write,read}=$(seq -s, 0 100) -- wg
$ cargo run --bin reverse-lookup --features=wireguard,nlctrl,rt-link -- ./output_file
Decoding request in family ROUTE flags=[REQUEST,ACK,DUMP,REPLACE,EXCL] Raw { protonum: 0, request_type: 0 }
...
Decoding reply in genl family wireguard flags=[MULTI] Generic("wireguard")
Wgdevice {
    ListenPort: 0,
    Fwmark: 0,
    Ifindex: 10,
    Ifname: "wg0",
}
...
```

This way you can study the structure of the messages the kernel expects/replies
with. In order to translate it into code, you simply need to convert attributes
from CamelCase to snake_case, and add occasional .get_\*() or .push_\*()
prefix.

You can quickly build and install the tool using:

```sh
$ cargo install --git https://github.com/one-d-wide/netlink-bindings reverse-lookup --features all-subsystems
$ ~/.cargo/bin/reverse-lookup --help
```

## Contribute

See [./CONTRIBUTING.md](./CONTRIBUTING.md) for information on how to work with
this repo, its structure, codegen stuff, etc.

If your want to contribute, you can, for example:

- Just use netlink-bindings. If you encounter a shortcoming of the current API, report it.
- Write straightforward higher-level abstractions on top of netlink-bindings.
- Add testing: collect netlink messages and check that they are parsed
correctly. See wireguard [tests](./netlink-bindings/src/wireguard/tests.rs) as
an example. Additional [examples](./netlink-socket/examples) are also very
welcome.
- Implement yet unsupported netlink functionality.
- Improve compilation time, reduce the size of generated bindings.
- Experiment with a better Rust interface (for encoding/decoding and the sockets).
- Sponsor the project (contact the author for options).
