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

### Environment variables / .env

Credentials can be provided without prompts using either environment variables
or a `.env` file in the current working directory:

```bash
cp .env.example .env
chmod 600 .env
# Edit .env with your username and password, then run from this directory:
sudo ./ntpuvpn-rs
```

| Variable | Purpose |
| --- | --- |
| `NTPUVPN_USERNAME` | VPN username |
| `NTPUVPN_PASSWORD` | VPN password (also used for automatic reconnection) |
| `NTPUVPN_VPN_NETWORK` | IPv4 VPN network, defaults to `10.0.0.0` |
| `NTPUVPN_VPN_MASK` | IPv4 netmask, defaults to `255.0.0.0` |

Priority: process environment → `.env` → saved config/keyring → interactive
prompt. Unset values fall back individually. Empty or invalid values are errors,
not a request to fall back. With username and password supplied, no terminal is
required; missing credentials in a noninteractive run produce an error instead
of waiting for input.

Env overrides are not saved to `config.json`. An env-provided password bypasses
keyring reads and writes. `.env` is ignored by Git; never commit credentials.
Use single quotes in `.env` for passwords containing `$`, `#` or spaces. Parsing
errors do not print the failing line or password. Only the current directory's
`.env` is read; parent directories are not searched.

`sudo` normally removes environment variables. To pass exported credentials on
Linux without putting the password in command-line arguments or shell history:

```bash
export NTPUVPN_USERNAME='your-student-id'
read -rs -p 'VPN password: ' NTPUVPN_PASSWORD
export NTPUVPN_PASSWORD
sudo --preserve-env=NTPUVPN_USERNAME,NTPUVPN_PASSWORD ./ntpuvpn-rs
unset NTPUVPN_PASSWORD
```

The above input example uses Bash. `.env` stores a plaintext password, and process
environment values can be inspected by privileged processes; protect them
accordingly. For an interactive login without stored plaintext, leave
`NTPUVPN_PASSWORD` unset and continue using the keyring/password prompt.

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
On Linux, the optional TUN/route lifecycle and noninteractive credential tests
must run in an isolated network namespace with no default route, so they do not
modify the host's routing table or contact the VPN server:

```bash
NTPUVPN_TEST_HOST_NETNS="$(readlink /proc/self/ns/net)" \
  unshare --user --map-root-user --net \
  cargo test -- --ignored
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
