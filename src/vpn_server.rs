use std::io;
use std::net::IpAddr;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use anyconnect::{AuthFormResult, Client, Command, CommandHandle, FormOptionKind};
use tokio::task::JoinHandle;
use tun::AbstractDevice;

use crate::utils;

pub struct VpnSession {
    command: CommandHandle,
    task: Option<JoinHandle<io::Result<()>>>,
    pub interface_index: u32,
}

impl VpnSession {
    pub async fn new(server: &str, user: &str, password: &str) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        if !std::path::Path::new("/dev/net/tun").exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "/dev/net/tun is unavailable; load the tun kernel module first",
            ));
        }

        let interface_name = utils::generate_free_interface_name(interface_prefix());
        let server = format!("https://{server}");
        let user = user.to_owned();
        let password = password.to_owned();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        let task = tokio::task::spawn_blocking(move || {
            let result = Self::connect(&server, &user, &password, &interface_name, ready_tx);
            if let Err(error) = &result {
                eprintln!("VPN connection failed: {error}");
            }
            result
        });

        let (command, interface_index) = ready_rx.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "VPN connection stopped before creating the TUN interface",
            )
        })??;

        println!("VPN session established on interface index {interface_index}");
        Ok(Self {
            command,
            task: Some(task),
            interface_index,
        })
    }

    fn connect(
        server: &str,
        user: &str,
        password: &str,
        interface_name: &str,
        ready: tokio::sync::oneshot::Sender<io::Result<(CommandHandle, u32)>>,
    ) -> io::Result<()> {
        let username = user.to_owned();
        let password = password.to_owned();
        let mut password_available = true;
        let mut client = Client::builder()
            .url(server)
            .protocol("nc")
            .authentication_handler(move |form| {
                let mut valid = true;
                for mut option in form.options().filter(|option| !option.is_ignored()) {
                    let name = option.name().unwrap_or_default().to_ascii_lowercase();
                    let value = match option.kind() {
                        FormOptionKind::Text
                            if name.starts_with("user") || name.starts_with("uname") =>
                        {
                            Some(username.as_str())
                        }
                        FormOptionKind::Password if password_available => {
                            password_available = false;
                            Some(password.as_str())
                        }
                        _ => None,
                    };
                    if let Some(value) = value
                        && option.set_value(value).is_err()
                    {
                        valid = false;
                    }
                }
                if valid {
                    AuthFormResult::Submit
                } else {
                    AuthFormResult::Error
                }
            })
            .build()
            .map_err(to_io_error)?;

        client.obtain_cookie().map_err(to_io_error)?;
        client.make_cstp_connection().map_err(to_io_error)?;
        let network = client.network_config().map_err(to_io_error)?;
        let address = parse_ip(network.address.as_deref(), "VPN IPv4 address")?;
        let netmask = parse_ip(network.netmask.as_deref(), "VPN IPv4 netmask")?;

        let mut tun_config = tun::Configuration::default();
        tun_config
            .tun_name(interface_name)
            .address(address)
            .destination(address)
            .netmask(netmask)
            .up();
        if let Ok(mtu) = u16::try_from(network.mtu)
            && mtu > 0
        {
            tun_config.mtu(mtu);
        }

        let device = tun::create(&tun_config)
            .map_err(|error| io::Error::other(format!("failed to create TUN device: {error}")))?;
        let interface_index = u32::try_from(device.tun_index().map_err(|error| {
            io::Error::other(format!("failed to get TUN interface index: {error}"))
        })?)
        .map_err(|_| io::Error::other("TUN interface returned a negative index"))?;

        // SAFETY: into_raw_fd transfers the device's only FD ownership here.
        let tun_fd = unsafe { OwnedFd::from_raw_fd(device.into_raw_fd()) };
        client.setup_tun_fd(tun_fd).map_err(to_io_error)?;
        let command = client.command_handle().map_err(to_io_error)?;
        ready.send(Ok((command, interface_index))).map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "VPN startup was cancelled",
            )
        })?;

        if let Err(error) = client.setup_dtls(60) {
            eprintln!("Warning: DTLS is unavailable, continuing over TLS: {error}");
        }
        client.run_mainloop(300, 10).map_err(to_io_error)
    }

    pub async fn wait(&mut self) -> io::Result<()> {
        let task = self
            .task
            .take()
            .ok_or_else(|| io::Error::other("VPN task has already been joined"))?;
        task.await
            .map_err(|error| io::Error::other(format!("VPN task failed: {error}")))?
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(task) = self.task.take() {
            let cancel_result = self.command.send(Command::Cancel).map_err(to_io_error);
            let _ = task.await;
            cancel_result?;
        }
        Ok(())
    }
}

fn to_io_error(error: anyconnect::Error) -> io::Error {
    io::Error::other(error)
}

fn parse_ip(value: Option<&str>, field: &str) -> io::Result<IpAddr> {
    value
        .ok_or_else(|| io::Error::other(format!("VPN server did not provide {field}")))?
        .parse()
        .map_err(|error| io::Error::other(format!("invalid {field}: {error}")))
}

#[cfg(target_os = "macos")]
fn interface_prefix() -> &'static str {
    "utun"
}

#[cfg(target_os = "linux")]
fn interface_prefix() -> &'static str {
    "tun"
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ntpuvpn-rs currently supports macOS and Linux only");

impl Drop for VpnSession {
    fn drop(&mut self) {
        let _ = self.command.send(Command::Cancel);
    }
}

#[cfg(test)]
mod tests {
    use super::interface_prefix;

    #[test]
    fn uses_platform_interface_prefix() {
        #[cfg(target_os = "macos")]
        assert_eq!(interface_prefix(), "utun");
        #[cfg(target_os = "linux")]
        assert_eq!(interface_prefix(), "tun");
    }
}
