use std::{
    collections::HashSet,
    process::{Command, Stdio},
    time::Duration,
};

use pnet::datalink;

use crate::utils;

pub struct VpnSession {
    process: std::process::Child,
    pub interface: datalink::NetworkInterface,
}

impl VpnSession {
    pub fn new(server: &str, user: &str, password: &str) -> std::io::Result<Self> {
        let free_interface_str = utils::generate_free_interface_name("utun");

        let existing_interfaces: HashSet<String> =
            datalink::interfaces().into_iter().map(|i| i.name).collect();

        let mut process = Command::new("openconnect")
            .arg("--protocol=pulse")
            .arg("--user")
            .arg(user)
            .arg("--passwd-on-stdin")
            .arg(server)
            .arg("--interface")
            .arg(&free_interface_str)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        if let Some(mut stdin) = process.stdin.take() {
            use std::io::Write;
            stdin.write_all(password.as_bytes())?;
            stdin.write_all(b"\n")?;
        }

        // Wait for the interface to appear

        let new_interface =
            Self::wait_for_interface(&existing_interfaces, Duration::from_secs(10))?;

        Ok(Self {
            process,
            interface: new_interface,
        })
    }
    fn wait_for_interface(
        existing_interfaces: &HashSet<String>,
        timeout: Duration,
    ) -> std::io::Result<datalink::NetworkInterface> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Some(new_iface) = datalink::interfaces().into_iter().find(|i| {
                i.name.starts_with("utun") && i.is_up() && !existing_interfaces.contains(&i.name)
            }) {
                return Ok(new_iface);
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "New VPN interface (utun) did not appear within timeout",
        ))
    }
}

impl Drop for VpnSession {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
