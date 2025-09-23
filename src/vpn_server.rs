use std::process::Stdio;
use std::{collections::HashSet, time::Duration};

use crate::utils;
use pnet::datalink;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tun_rs::DeviceBuilder;

pub struct VpnSession {
    process: Child,
    pub interface: datalink::NetworkInterface,
}

impl VpnSession {
    pub async fn new(server: &str, user: &str, password: &str) -> std::io::Result<Self> {
        let free_interface_str = utils::generate_free_interface_name("utun");

        let existing_interfaces: HashSet<String> =
            datalink::interfaces().into_iter().map(|i| i.name).collect();

        let mut process = Command::new("openconnect")
            .arg("--protocol=pulse")
            .arg("--user")
            .arg(user)
            .arg("--passwd-on-stdin")
            .arg("--interface")
            .arg(&free_interface_str)
            .arg(server)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = process.stdin.take() {
            stdin.write_all(password.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }

        if let Some(stdout) = process.stdout.take() {
            let mut reader = BufReader::new(stdout).lines();
            while let Some(line) = reader.next_line().await? {
                println!("openconnect stdout: {}", line);
                if line.contains("ESP session established") {
                    println!("VPN session established!");
                    break;
                }
            }
        }

        let new_interface =
            Self::wait_for_interface(&existing_interfaces, Duration::from_secs(10)).await?;

        Ok(Self {
            process,
            interface: new_interface,
        })
    }
    async fn wait_for_interface(
        existing_interfaces: &HashSet<String>,
        timeout: Duration,
    ) -> std::io::Result<datalink::NetworkInterface> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < timeout {
            if let Some(new_iface) = datalink::interfaces().into_iter().find(|i| {
                i.name.starts_with("utun") && i.is_up() && !existing_interfaces.contains(&i.name)
            }) {
                return Ok(new_iface);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
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
    }
}
