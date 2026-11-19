use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyconnect::{AuthFormResult, Client, Command, CommandHandle, FormOptionKind};
use tokio::task::JoinHandle;
use tun::AbstractDevice;

use crate::utils;

pub struct VpnSession {
    control: Arc<SessionControl>,
    task: Option<JoinHandle<io::Result<()>>>,
    pub interface_index: u32,
}

#[derive(Default)]
struct SessionControl {
    state: Mutex<ControlState>,
    wake: Condvar,
}

#[derive(Default)]
struct ControlState {
    stopped: bool,
    command: Option<CommandHandle>,
}

impl SessionControl {
    fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }

    fn activate(&self, command: CommandHandle) -> io::Result<ActiveConnection<'_>> {
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "VPN stopped"));
        }
        state.command = Some(command);
        Ok(ActiveConnection(self))
    }

    fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        if let Some(command) = &state.command {
            // An already-closed connection is also a successful cancellation.
            let _ = command.send(Command::Cancel);
        }
        self.wake.notify_all();
    }

    fn wait_to_retry(&self, delay: Duration) -> bool {
        let (state, _) = self
            .wake
            .wait_timeout_while(self.state.lock().unwrap(), delay, |state| !state.stopped)
            .unwrap();
        !state.stopped
    }
}

// Remove the command pipe before its Client is dropped, so cancellation never
// writes to a stale connection while a replacement is being authenticated.
struct ActiveConnection<'a>(&'a SessionControl);

impl Drop for ActiveConnection<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().command = None;
    }
}

struct ConnectionWorker {
    server: String,
    username: String,
    password: String,
    interface_name: String,
    device: Option<tun::Device>,
    vpn_route: net_route::Route,
}

struct ConnectionAttempt {
    result: io::Result<()>,
    established: bool,
    connected_for: Duration,
}

fn run_reconnecting(
    control: &SessionControl,
    mut attempt: impl FnMut() -> ConnectionAttempt,
    mut wait_to_retry: impl FnMut(Duration) -> bool,
) -> io::Result<()> {
    let mut failures: u32 = 0;
    while !control.stopped() {
        let ConnectionAttempt {
            result,
            established,
            connected_for,
        } = attempt();
        if control.stopped() {
            return Ok(());
        }
        // Fail fast on the initial login; only reconnect a session that
        // has previously authenticated and configured its TUN device.
        if !established {
            return result;
        }
        if connected_for >= Duration::from_secs(60) {
            failures = 0;
        }
        let delay = retry_delay(failures);
        failures = failures.saturating_add(1);
        match result {
            Ok(()) => eprintln!("VPN connection ended; reconnecting in {}s", delay.as_secs()),
            Err(error) => eprintln!(
                "VPN disconnected: {error}; reconnecting in {}s",
                delay.as_secs()
            ),
        }
        if !wait_to_retry(delay) {
            return Ok(());
        }
    }
    Ok(())
}

impl ConnectionWorker {
    fn run(
        &mut self,
        control: &SessionControl,
        ready: &mut Option<tokio::sync::oneshot::Sender<io::Result<u32>>>,
    ) -> io::Result<()> {
        run_reconnecting(
            control,
            || {
                let mut connected_at = None;
                let result = self.connect(control, ready, &mut connected_at);
                ConnectionAttempt {
                    result,
                    established: ready.is_none(),
                    connected_for: connected_at.map_or(Duration::ZERO, |at| at.elapsed()),
                }
            },
            |delay| control.wait_to_retry(delay),
        )
    }

    fn connect(
        &mut self,
        control: &SessionControl,
        ready: &mut Option<tokio::sync::oneshot::Sender<io::Result<u32>>>,
        connected_at: &mut Option<Instant>,
    ) -> io::Result<()> {
        let username = self.username.clone();
        let password = self.password.clone();
        let mut password_available = true;
        let mut client = Client::builder()
            .url(&self.server)
            .protocol("nc")
            .authentication_handler(move |form| {
                if let Some(error) = form.error().filter(|error| !error.trim().is_empty()) {
                    eprintln!("VPN authentication error: {error}");
                }
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
                        FormOptionKind::Password => {
                            // Do not repeatedly submit a failed password or
                            // fill a second-factor challenge with the password.
                            valid = false;
                            None
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

        // Install cancellation before authentication, not just the main loop.
        // The guard must be dropped before client on all return paths.
        let _active = control.activate(client.command_handle().map_err(to_io_error)?)?;
        client.obtain_cookie().map_err(to_io_error)?;
        client.make_cstp_connection().map_err(to_io_error)?;
        let network = client.network_config().map_err(to_io_error)?;
        let tun_config = tunnel_config(&self.interface_name, &network)?;

        if let Some(device) = &mut self.device {
            // Reuse the same interface/index and refresh the server-assigned
            // IP/netmask/MTU, which may change after reauthentication.
            device.configure(&tun_config).map_err(tun_error)?;
            let index = u32::try_from(device.tun_index().map_err(tun_error)?)
                .map_err(|_| io::Error::other("TUN interface returned a negative index"))?;
            // Linux may flush interface routes when the assigned IP changes.
            // Restore only our configured VPN route, never the default route.
            let route = self.vpn_route.clone().with_ifindex(index);
            tokio::runtime::Handle::current().block_on(ensure_vpn_route(&route))?;
        } else {
            self.device = Some(tun::create(&tun_config).map_err(tun_error)?);
        }
        let device = self.device.as_ref().unwrap();
        let interface_index = u32::try_from(device.tun_index().map_err(tun_error)?)
            .map_err(|_| io::Error::other("TUN interface returned a negative index"))?;

        // SAFETY: device owns this live FD throughout this call. The client
        // duplicates it, so dropping a connection cannot destroy the TUN.
        let fd = unsafe { BorrowedFd::borrow_raw(device.as_raw_fd()) };
        client.setup_tun_fd_borrowed(fd).map_err(to_io_error)?;
        if let Some(sender) = ready.take() {
            sender.send(Ok(interface_index)).map_err(|_| {
                io::Error::new(io::ErrorKind::Interrupted, "VPN startup was cancelled")
            })?;
        } else {
            println!("VPN reconnected on {}", self.interface_name);
        }

        if let Err(error) = client.setup_dtls(60) {
            eprintln!("Warning: DTLS is unavailable, continuing over TLS: {error}");
        }
        *connected_at = Some(Instant::now());
        client.run_mainloop(300, 10).map_err(to_io_error)
    }
}

impl VpnSession {
    pub async fn new(
        server: &str,
        user: &str,
        password: &str,
        vpn_network: Ipv4Addr,
        vpn_prefix: u8,
    ) -> io::Result<Self> {
        if vpn_prefix > 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid IPv4 VPN prefix",
            ));
        }
        #[cfg(target_os = "linux")]
        if !std::path::Path::new("/dev/net/tun").exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "/dev/net/tun is unavailable; load the tun kernel module first",
            ));
        }

        let mut worker = ConnectionWorker {
            server: format!("https://{server}"),
            username: user.to_owned(),
            password: password.to_owned(),
            interface_name: utils::generate_free_interface_name(interface_prefix()),
            device: None,
            vpn_route: net_route::Route::new(vpn_network.into(), vpn_prefix),
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let control = Arc::new(SessionControl::default());
        let worker_control = control.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut ready = Some(ready_tx);
            let result = worker.run(&worker_control, &mut ready);
            if let Some(sender) = ready {
                let error = result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::Interrupted, "VPN startup was cancelled")
                });
                let _ = sender.send(Err(error));
                return Ok(());
            }
            result
        });
        // Own the control before awaiting startup so cancelling new() also
        // cancels a worker that is still authenticating.
        let mut session = Self {
            control,
            task: Some(task),
            interface_index: 0,
        };
        session.interface_index = ready_rx.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "VPN connection stopped before creating the TUN interface",
            )
        })??;

        println!(
            "VPN session established on interface index {}",
            session.interface_index
        );
        Ok(session)
    }

    pub async fn wait(&mut self) -> io::Result<()> {
        let task = self
            .task
            .as_mut()
            .ok_or_else(|| io::Error::other("VPN task has already been joined"))?;
        let result = task.await;
        self.task = None;
        result.map_err(|error| io::Error::other(format!("VPN task failed: {error}")))?
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.control.cancel();
        if self.task.is_some() {
            self.wait().await?;
        }
        Ok(())
    }
}

fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((5_u64 << failures.min(4)).min(60))
}

async fn ensure_vpn_route(route: &net_route::Route) -> io::Result<()> {
    let handle = net_route::Handle::new()?;
    if !handle.list().await?.contains(route) {
        handle.add(route).await?;
    }
    Ok(())
}

fn tun_error(error: tun::Error) -> io::Error {
    io::Error::other(format!("failed to configure TUN device: {error}"))
}

fn tunnel_config(
    name: &str,
    network: &anyconnect::NetworkConfig,
) -> io::Result<tun::Configuration> {
    let address = parse_ip(network.address.as_deref(), "VPN IPv4 address")?;
    let netmask = parse_ip(network.netmask.as_deref(), "VPN IPv4 netmask")?;
    let mut config = tun::Configuration::default();
    config
        .tun_name(name)
        .address(address)
        .destination(address)
        .netmask(netmask)
        .up();
    if let Ok(mtu) = u16::try_from(network.mtu)
        && mtu > 0
    {
        config.mtu(mtu);
    }
    Ok(config)
}

fn to_io_error(error: anyconnect::Error) -> io::Error {
    io::Error::other(error)
}

fn parse_ip(value: Option<&str>, field: &str) -> io::Result<Ipv4Addr> {
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
        self.control.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_platform_interface_prefix() {
        #[cfg(target_os = "macos")]
        assert_eq!(interface_prefix(), "utun");
        #[cfg(target_os = "linux")]
        assert_eq!(interface_prefix(), "tun");
    }

    #[test]
    fn retry_backoff_is_capped() {
        let delays: Vec<_> = (0..7).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [5, 10, 20, 40, 60, 60, 60]);
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn initial_login_failure_is_not_retried() {
        let control = SessionControl::default();
        let result = run_reconnecting(
            &control,
            || ConnectionAttempt {
                result: Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad login")),
                established: false,
                connected_for: Duration::ZERO,
            },
            |_| panic!("initial authentication must fail without retrying"),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn reconnects_after_both_clean_exit_and_error() {
        let control = SessionControl::default();
        let mut attempts = 0;
        let mut delays = Vec::new();
        run_reconnecting(
            &control,
            || {
                attempts += 1;
                ConnectionAttempt {
                    result: if attempts == 1 {
                        Ok(())
                    } else {
                        Err(io::Error::other("connection lost"))
                    },
                    established: true,
                    connected_for: Duration::ZERO,
                }
            },
            |delay| {
                delays.push(delay.as_secs());
                delays.len() < 6
            },
        )
        .unwrap();
        assert_eq!(attempts, 6);
        assert_eq!(delays, [5, 10, 20, 40, 60, 60]);
    }

    #[test]
    fn stable_connection_resets_retry_backoff() {
        let control = SessionControl::default();
        let mut attempts = 0;
        let mut delays = Vec::new();
        run_reconnecting(
            &control,
            || {
                attempts += 1;
                ConnectionAttempt {
                    result: Err(io::Error::other("connection lost")),
                    established: true,
                    connected_for: if attempts == 4 {
                        Duration::from_secs(60)
                    } else {
                        Duration::ZERO
                    },
                }
            },
            |delay| {
                delays.push(delay.as_secs());
                delays.len() < 5
            },
        )
        .unwrap();
        assert_eq!(delays, [5, 10, 20, 5, 10]);
    }

    #[test]
    fn cancellation_does_not_start_another_connection() {
        let control = SessionControl::default();
        run_reconnecting(
            &control,
            || {
                control.cancel();
                ConnectionAttempt {
                    result: Err(io::Error::other("cancelled")),
                    established: true,
                    connected_for: Duration::ZERO,
                }
            },
            |_| panic!("must not reconnect after cancellation"),
        )
        .unwrap();
        run_reconnecting(
            &control,
            || panic!("must not connect an already-stopped session"),
            |_| panic!("must not wait an already-stopped session"),
        )
        .unwrap();
    }

    #[test]
    fn cancellation_wakes_retry_wait() {
        let control = Arc::new(SessionControl::default());
        let worker_control = control.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(worker_control.wait_to_retry(Duration::from_secs(60)))
                .unwrap();
        });
        started_rx.recv().unwrap();
        control.cancel();
        assert!(!done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn active_command_is_cleared_before_client_drop() {
        let control = SessionControl::default();
        let mut client = Client::builder().protocol("nc").build().unwrap();
        let active = control.activate(client.command_handle().unwrap()).unwrap();
        assert!(control.state.lock().unwrap().command.is_some());
        drop(active);
        assert!(control.state.lock().unwrap().command.is_none());
        control.cancel();
        assert!(control.activate(client.command_handle().unwrap()).is_err());
    }

    #[tokio::test]
    async fn cancelling_wait_keeps_worker_available_for_shutdown() {
        let control = Arc::new(SessionControl::default());
        let worker_control = control.clone();
        let task = tokio::task::spawn_blocking(move || {
            assert!(!worker_control.wait_to_retry(Duration::from_secs(60)));
            Ok(())
        });
        let mut session = VpnSession {
            control,
            task: Some(task),
            interface_index: 1,
        };
        // Like the losing wait() branch of main's Ctrl+C tokio::select!.
        assert!(
            tokio::time::timeout(Duration::from_millis(10), session.wait())
                .await
                .is_err()
        );
        assert!(session.task.is_some());
        tokio::time::timeout(Duration::from_secs(2), session.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(session.task.is_none());
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_session_cancels_retrying_worker() {
        let control = Arc::new(SessionControl::default());
        let worker_control = control.clone();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            assert!(!worker_control.wait_to_retry(Duration::from_secs(60)));
            finished_tx.send(()).unwrap();
            Ok(())
        });
        let session = VpnSession {
            control,
            task: Some(task),
            interface_index: 1,
        };
        drop(session);
        tokio::time::timeout(Duration::from_secs(2), finished_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn validates_tunnel_configuration_before_reconfiguring() {
        let mut network = anyconnect::NetworkConfig {
            address: Some("10.1.2.3".into()),
            netmask: Some("255.255.255.255".into()),
            mtu: 1400,
            ..Default::default()
        };
        assert!(tunnel_config("tun0", &network).is_ok());
        network.address = Some("10.2.3.4".into());
        network.mtu = 1300;
        assert!(tunnel_config("tun0", &network).is_ok());
        network.address = Some("::1".into());
        assert!(tunnel_config("tun0", &network).is_err());
        network.address = None;
        assert!(tunnel_config("tun0", &network).is_err());
        network.address = Some("10.1.2.3".into());
        network.netmask = Some("invalid".into());
        assert!(tunnel_config("tun0", &network).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires CAP_NET_ADMIN in an isolated network namespace"]
    async fn tun_and_route_survive_client_replacement() {
        // Never mutate the host's routing table when running this test.
        assert_ne!(
            std::fs::read_link("/proc/self/ns/net")
                .unwrap()
                .to_string_lossy(),
            std::env::var("NTPUVPN_TEST_HOST_NETNS")
                .expect("provide the host netns ID before running unshare (see readme.md)"),
            "run this test in a separate network namespace"
        );
        let mut network = anyconnect::NetworkConfig {
            address: Some("10.20.0.1".into()),
            netmask: Some("255.255.255.255".into()),
            mtu: 1400,
            ..Default::default()
        };
        let mut device = tun::create(&tunnel_config("tun-test0", &network).unwrap()).unwrap();
        let index = u32::try_from(device.tun_index().unwrap()).unwrap();
        let handle = net_route::Handle::new().unwrap();
        let route = net_route::Route::new("10.0.0.0".parse().unwrap(), 8).with_ifindex(index);
        handle.add(&route).await.unwrap();
        let routes = handle.list().await.unwrap();
        assert!(
            routes.contains(&route),
            "route missing after add: {routes:?}"
        );

        for (address, mtu) in [("10.20.0.1", 1400), ("10.30.0.2", 1300)] {
            network.address = Some(address.into());
            network.mtu = mtu;
            device
                .configure(&tunnel_config("tun-test0", &network).unwrap())
                .unwrap();
            ensure_vpn_route(&route).await.unwrap();
            let mut client = Client::builder().protocol("nc").build().unwrap();
            // SAFETY: device owns this FD until after the client is dropped.
            let fd = unsafe { BorrowedFd::borrow_raw(device.as_raw_fd()) };
            client.setup_tun_fd_borrowed(fd).unwrap();
            drop(client);

            assert_eq!(u32::try_from(device.tun_index().unwrap()).unwrap(), index);
            assert_eq!(device.address().unwrap().to_string(), address);
            assert_eq!(device.mtu().unwrap(), u16::try_from(mtu).unwrap());
            let routes = handle.list().await.unwrap();
            assert!(
                routes.contains(&route),
                "route missing for {address}: {routes:?}"
            );
        }
        handle.delete(&route).await.unwrap();
    }
}
