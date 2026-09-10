# ntpuvpn-rs

An NTPU VPN client for Linux and macOS. The executable embeds OpenConnect's
Juniper Network Connect (`nc`) implementation through the `anyconnect` Rust
crate, so users do not need to install the `openconnect` command.

Administrator privileges are required to create the TUN interface and update
routes.

## Run

On Linux, make sure the TUN device is available:

```bash
test -c /dev/net/tun
sudo ./ntpuvpn-rs
```

On macOS:

```bash
sudo ./ntpuvpn-rs
```

The program routes the configured VPN network (by default `10.0.0.0/8`) through
the VPN interface and leaves the normal default route in place. On Linux, the
desktop keyring may not be available under `sudo`; in that case the password is
requested each time instead of aborting the VPN connection.

## Build

OpenConnect 9.20 is compiled from the source bundled by `anyconnect-sys`. The
`openconnect` package is not required, but its native build dependencies are.

Debian / Ubuntu:

```bash
sudo apt install build-essential clang libdbus-1-dev libssl-dev libxml2-dev pkg-config zlib1g-dev
cargo build --release
```

Fedora:

```bash
sudo dnf install clang dbus-devel gcc libxml2-devel make openssl-devel pkgconf-pkg-config zlib-devel
cargo build --release
```

Arch Linux:

```bash
sudo pacman -S base-devel clang dbus libxml2 openssl pkgconf zlib
cargo build --release
```

macOS:

```bash
brew install openssl@3 pkgconf
cargo build --release
```

The generated binary statically includes OpenConnect under LGPL-2.1-only.
Distributors must comply with its source and relinking requirements.
