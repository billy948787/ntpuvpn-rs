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

After a successful initial connection, the client automatically reconnects if
the VPN connection ends (including an expired session). It obtains a fresh
authentication cookie using the credentials entered at startup. Failed retries
back off from 5 seconds to at most 60 seconds; a connection lasting at least a
minute resets this delay. OpenConnect's built-in transport reconnect remains
enabled as well, so reauthentication happens after that recovery ends.

The TUN interface stays alive across retries to keep its interface index stable.
Each new connection refreshes its assigned address, netmask and MTU, and restores
the configured VPN route if the OS removed it during the address update.
Ctrl+C stops both the active connection and pending retries. The initial login
still fails immediately on error; if credentials change while running, stop the
client and restart it with the new password. Server-required interactive MFA is
not supported by automatic reauthentication.

### Reconnection tests

`cargo test` covers retry backoff and cancellation without connecting to the VPN.
On Linux, the optional TUN/route lifecycle test must run in an isolated network
namespace so it does not modify the host's routing table:

```bash
NTPUVPN_TEST_HOST_NETNS="$(readlink /proc/self/ns/net)" \
  unshare --user --map-root-user --net \
  cargo test tun_and_route_survive_client_replacement -- --ignored
```

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
