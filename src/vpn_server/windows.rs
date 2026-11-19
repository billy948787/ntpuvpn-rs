//! Windows uses a native OpenConnect installation. Its vpnc script reports
//! tunnel settings; we configure only that adapter and the requested VPN route.
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

use crate::reroute_server::RerouteServer;

const SCRIPT: &str = include_str!("windows-script.js");
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

pub struct VpnSession {
    cancel: watch::Sender<bool>,
    task: Option<JoinHandle<io::Result<()>>>,
    pub interface_index: u32,
}

impl VpnSession {
    pub async fn new(
        server: &str,
        user: &str,
        password: &str,
        vpn_network: Ipv4Addr,
        vpn_prefix: u8,
    ) -> io::Result<Self> {
        if vpn_prefix > 32 || password.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid IPv4 VPN prefix or password contains a newline",
            ));
        }
        let worker = Worker {
            executable: find_openconnect()?,
            server: format!("https://{server}"),
            username: user.to_owned(),
            password: password.to_owned(),
            network: vpn_network,
            prefix: vpn_prefix,
        };
        let (cancel, receiver) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(async move { worker.run(receiver, ready_tx).await });
        let mut session = Self {
            cancel,
            task: Some(task),
            interface_index: 0,
        };
        session.interface_index = tokio::select! {
            result = ready_rx => result.map_err(|_| {
                io::Error::other("OpenConnect stopped before configuring the VPN interface")
            })??,
            signal = tokio::signal::ctrl_c() => {
                session.shutdown().await?;
                signal?;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "VPN startup was cancelled"));
            },
        };
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
        self.cancel.send_replace(true);
        if self.task.is_some() {
            self.wait().await?;
        }
        Ok(())
    }
}

impl Drop for VpnSession {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}

struct Worker {
    executable: PathBuf,
    server: String,
    username: String,
    password: String,
    network: Ipv4Addr,
    prefix: u8,
}

impl Worker {
    async fn run(
        self,
        mut cancel: watch::Receiver<bool>,
        ready: oneshot::Sender<io::Result<u32>>,
    ) -> io::Result<()> {
        let mut ready = Some(ready);
        let mut failures = 0_u32;
        while !*cancel.borrow() {
            let mut connected_at = None;
            let result = self
                .attempt(&mut cancel, &mut ready, &mut connected_at)
                .await;
            if *cancel.borrow() {
                return Ok(());
            }
            if let Some(sender) = ready.take() {
                let _ = sender.send(Err(result
                    .err()
                    .unwrap_or_else(|| io::Error::other("OpenConnect exited during startup"))));
                return Ok(());
            }
            if connected_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(60)) {
                failures = 0;
            }
            let delay = retry_delay(failures);
            failures = failures.saturating_add(1);
            if let Err(error) = result {
                eprintln!(
                    "VPN disconnected: {error}; reconnecting in {}s",
                    delay.as_secs()
                );
            } else {
                eprintln!("VPN connection ended; reconnecting in {}s", delay.as_secs());
            }
            tokio::select! {
                biased;
                _ = cancel.changed() => return Ok(()),
                _ = tokio::time::sleep(delay) => {},
            }
        }
        Ok(())
    }

    fn command(&self, script: &Path, state: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args([
                "--protocol=nc",
                "--non-inter",
                "--passwd-on-stdin",
                "--disable-ipv6",
                "--reconnect-timeout=300",
            ])
            .arg(format!("--user={}", self.username))
            // A unique name prevents borrowing or changing an existing adapter.
            .arg(format!(
                "--interface={}",
                crate::utils::generate_free_interface_name(&format!(
                    "ntpuvpn-{}-",
                    std::process::id()
                ))
            ))
            .arg("--script")
            .arg(script)
            .arg(&self.server)
            .env("NTPUVPN_TUNNEL_STATE", state)
            .env_remove("NTPUVPN_PASSWORD")
            .env_remove("NTPUVPN_USERNAME")
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            // Ctrl+C is handled by the parent. A targeted Ctrl+Break lets the
            // child run its disconnect hooks without signalling other apps.
            .creation_flags(CREATE_NEW_PROCESS_GROUP);
        command
    }

    async fn attempt(
        &self,
        cancel: &mut watch::Receiver<bool>,
        ready: &mut Option<oneshot::Sender<io::Result<u32>>>,
        connected_at: &mut Option<Instant>,
    ) -> io::Result<()> {
        let temp = tempfile::Builder::new().prefix("ntpuvpn-").tempdir()?;
        let script = temp.path().join("vpnc.js");
        let state = temp.path().join("tunnel.txt");
        std::fs::write(&script, SCRIPT)?;
        let mut child = self.command(&script, &state).spawn().map_err(|error| {
            io::Error::new(error.kind(), format!("Failed to launch OpenConnect: {error}. Install native OpenConnect and Wintun; see readme.md"))
        })?;
        let mut route = None;
        let result = tokio::select! {
            biased;
            _ = cancelled(cancel) => Ok(()),
            result = self.monitor(&mut child, &state, ready, connected_at, &mut route) => result,
        };
        // Remove our route before stopping OpenConnect, while the interface
        // still exists. Wintun removes the temporary adapter on process exit.
        let cleanup = match &mut route {
            Some(route) => route.shutdown().await,
            None => Ok(()),
        };
        let stopped = stop_child(&mut child).await;
        result.and(cleanup).and(stopped)
    }

    async fn monitor(
        &self,
        child: &mut Child,
        state: &Path,
        ready: &mut Option<oneshot::Sender<io::Result<u32>>>,
        connected_at: &mut Option<Instant>,
        route: &mut Option<RerouteServer>,
    ) -> io::Result<()> {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("OpenConnect stdin is unavailable"))?;
        // The password is sent only over stdin, never in arguments or a file.
        input.write_all(self.password.as_bytes()).await?;
        input.write_all(b"\n").await?;
        drop(input);
        let started = Instant::now();
        let mut last = String::new();
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                status = child.wait() => {
                    let status = status?;
                    return if status.success() { Ok(()) } else {
                        Err(io::Error::other(format!("OpenConnect exited with {status}")))
                    };
                },
                _ = poll.tick() => {
                    if connected_at.is_none() && started.elapsed() > STARTUP_TIMEOUT {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "OpenConnect did not configure a tunnel within 120 seconds"));
                    }
                    let contents = match std::fs::read_to_string(state) {
                        Ok(contents) => contents,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error),
                    };
                    if contents == last { continue; }
                    let settings = TunnelSettings::parse(&contents)?;
                    if let Some(route) = route {
                        route.shutdown().await?;
                    }
                    configure_tunnel(&settings).await?;
                    *route = Some(RerouteServer::for_interface(settings.index, self.network, self.prefix).await?);
                    last = contents;
                    if connected_at.is_none() {
                        *connected_at = Some(Instant::now());
                        if let Some(sender) = ready.take() {
                            sender.send(Ok(settings.index)).map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "VPN startup was cancelled"))?;
                        } else {
                            println!("VPN reconnected on interface index {}", settings.index);
                        }
                    }
                },
            }
        }
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    if !*cancel.borrow() {
        let _ = cancel.changed().await;
    }
}

async fn stop_child(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Some(pid) = child.id() {
        // SAFETY: pid identifies the process group created for this child.
        unsafe {
            GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
        }
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(result) => {
            result?;
        }
        Err(_) => child.kill().await?,
    }
    Ok(())
}

fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((5_u64 << failures.min(4)).min(60))
}

fn find_openconnect() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("NTPUVPN_OPENCONNECT") {
        if path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NTPUVPN_OPENCONNECT cannot be empty",
            ));
        }
        return Ok(path.into());
    }
    let beside_exe = std::env::current_exe()?.with_file_name("openconnect.exe");
    if beside_exe.is_file() {
        return Ok(beside_exe);
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(directory) = std::env::var_os(variable) {
            let path = PathBuf::from(directory).join("OpenConnect/openconnect.exe");
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Ok("openconnect.exe".into())
}

#[derive(Debug, PartialEq)]
struct TunnelSettings {
    index: u32,
    address: Ipv4Addr,
    netmask: Ipv4Addr,
    mtu: u16,
}

impl TunnelSettings {
    fn parse(contents: &str) -> io::Result<Self> {
        fn invalid() -> io::Error {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid OpenConnect tunnel settings",
            )
        }
        let fields: Vec<_> = contents.lines().collect();
        if fields.len() != 5 || fields[4].parse::<u64>().is_err() {
            return Err(invalid());
        }
        let settings = Self {
            index: fields[0].parse().map_err(|_| invalid())?,
            address: fields[1].parse().map_err(|_| invalid())?,
            netmask: fields[2].parse().map_err(|_| invalid())?,
            mtu: fields[3].parse().map_err(|_| invalid())?,
        };
        let mask = u32::from(settings.netmask);
        if settings.index == 0
            || settings.mtu < 576
            || settings.address.is_unspecified()
            || mask != u32::MAX.checked_shl(32 - mask.leading_ones()).unwrap_or(0)
        {
            return Err(invalid());
        }
        Ok(settings)
    }
}

async fn configure_tunnel(settings: &TunnelSettings) -> io::Result<()> {
    let index = format!("name={}", settings.index);
    netsh(&[
        "interface",
        "ipv4",
        "set",
        "address",
        &index,
        "source=static",
        &format!("address={}", settings.address),
        &format!("mask={}", settings.netmask),
        "gateway=none",
        "store=active",
    ])
    .await?;
    netsh(&[
        "interface",
        "ipv4",
        "set",
        "subinterface",
        &settings.index.to_string(),
        &format!("mtu={}", settings.mtu),
        "store=active",
    ])
    .await
}

async fn netsh(arguments: &[&str]) -> io::Result<()> {
    let system = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
    let status = Command::new(PathBuf::from(system).join("System32/netsh.exe"))
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .await?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Failed to configure VPN adapter ({status}); run ntpuvpn-rs from an Administrator terminal"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tunnel_settings() {
        let settings = TunnelSettings::parse("42\n10.1.2.3\n255.255.255.255\n1400\n123").unwrap();
        assert_eq!(settings.index, 42);
        assert_eq!(settings.mtu, 1400);
        for invalid in [
            "0\n10.1.2.3\n255.0.0.0\n1400",
            "42\n::1\n255.0.0.0\n1400",
            "42\n10.1.2.3\n255.0.255.0\n1400",
            "42\n10.1.2.3\n255.0.0.0\n0",
            "42\n10.1.2.3\n255.0.0.0\n65536",
            "42 & calc\n10.1.2.3\n255.0.0.0\n1400",
        ] {
            assert!(
                TunnelSettings::parse(&format!("{invalid}\n123")).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn password_is_not_in_process_arguments_or_environment() {
        let worker = Worker {
            executable: "openconnect.exe".into(),
            server: "https://example.test".into(),
            username: "student".into(),
            password: "SECRET".into(),
            network: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        };
        let command = worker.command(Path::new("C:/path with spaces/vpnc.js"), Path::new("state"));
        let command = command.as_std();
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--passwd-on-stdin")));
        assert!(args.contains(&std::ffi::OsStr::new("C:/path with spaces/vpnc.js")));
        assert!(!format!("{command:?}").contains("SECRET"));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "NTPUVPN_PASSWORD" && value.is_none())
        );
    }

    #[test]
    fn script_reports_settings_without_changing_network() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("vpnc.js");
        let state = temp.path().join("state");
        std::fs::write(&script, SCRIPT).unwrap();
        for reason in ["pre-init", "connect", "reconnect", "disconnect"] {
            let status = std::process::Command::new("cscript.exe")
                .args(["//nologo", "//e:JScript"])
                .arg(&script)
                .env("reason", reason)
                .env("TUNIDX", "42")
                .env("INTERNAL_IP4_ADDRESS", "10.1.2.3")
                .env("INTERNAL_IP4_NETMASK", "255.255.255.255")
                .env("INTERNAL_IP4_MTU", "1400")
                .env("NTPUVPN_TUNNEL_STATE", &state)
                .status()
                .unwrap();
            assert!(status.success());
            if reason == "connect" || reason == "reconnect" {
                assert!(TunnelSettings::parse(&std::fs::read_to_string(&state).unwrap()).is_ok());
            }
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_pending_retry() {
        let (sender, mut receiver) = watch::channel(false);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), cancelled(&mut receiver))
                .await
                .is_err()
        );
        let task = tokio::spawn(async move {
            cancelled(&mut receiver).await;
            Ok(())
        });
        let mut session = VpnSession {
            cancel: sender,
            task: Some(task),
            interface_index: 1,
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), session.wait())
                .await
                .is_err()
        );
        assert!(session.task.is_some());
        tokio::time::timeout(Duration::from_secs(1), session.shutdown())
            .await
            .unwrap()
            .unwrap();
        session.shutdown().await.unwrap();
    }

    fn test_worker(executable: PathBuf) -> Worker {
        Worker {
            executable,
            server: "https://example.test".into(),
            username: "student".into(),
            password: "test-password".into(),
            network: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        }
    }

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "vpn_server::windows::tests::child_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("NTPUVPN_TEST_CHILD", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .kill_on_drop(true);
        command
    }

    #[test]
    #[ignore = "subprocess fixture for process lifecycle tests"]
    fn child_process_fixture() {
        match std::env::var("NTPUVPN_TEST_CHILD").as_deref() {
            Ok("fail") => {
                std::thread::sleep(Duration::from_millis(100));
                std::process::exit(7);
            }
            Ok("wait") => std::thread::sleep(Duration::from_secs(60)),
            _ => {}
        }
    }

    #[tokio::test]
    async fn failed_process_does_not_wait_for_startup_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let mut child = fixture_command("fail").spawn().unwrap();
        let worker = test_worker("unused.exe".into());
        let (sender, _receiver) = oneshot::channel();
        let mut ready = Some(sender);
        let mut connected_at = None;
        let mut route = None;
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            worker.monitor(
                &mut child,
                &temp.path().join("state"),
                &mut ready,
                &mut connected_at,
                &mut route,
            ),
        )
        .await
        .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("OpenConnect exited")
        );
        assert!(ready.is_some());
        assert!(connected_at.is_none());
        assert!(route.is_none());
    }

    #[tokio::test]
    async fn shutdown_reaps_running_child() {
        let mut child = fixture_command("wait").spawn().unwrap();
        assert!(child.try_wait().unwrap().is_none());
        tokio::time::timeout(Duration::from_secs(7), stop_child(&mut child))
            .await
            .unwrap()
            .unwrap();
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn initial_launch_failure_is_not_retried() {
        let temp = tempfile::tempdir().unwrap();
        let worker = test_worker(temp.path().join("missing-openconnect.exe"));
        let (_cancel, receiver) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(worker.run(receiver, ready_tx));
        let error = tokio::time::timeout(Duration::from_secs(2), ready_rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("Failed to launch OpenConnect"));
        task.await.unwrap().unwrap();
    }

    #[test]
    fn retry_backoff_is_capped() {
        let delays: Vec<_> = (0..7).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [5, 10, 20, 40, 60, 60, 60]);
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(60));
    }
}
