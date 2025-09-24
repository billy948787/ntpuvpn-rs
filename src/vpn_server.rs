use std::process::Stdio;
use std::{collections::HashSet, time::Duration};

use crate::utils;
use pnet::datalink;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

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
            .arg(format!("--user={}", user))
            .arg("--passwd-on-stdin")
            .arg(format!("--interface={}", free_interface_str))
            .arg(server)
            .arg("--non-inter")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = process.stdin.take() {
            stdin.write_all(password.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(stdout) = process.stdout.take() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    println!("openconnect stdout: {}", line);
                    if line.contains("ESP session established") {
                        let _ = tx.send("SESSION_ESTABLISHED".to_string());
                        break;
                    }
                }
            });
        }

        if let Some(stderr) = process.stderr.take() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    eprintln!("openconnect stderr: {}", line);
                }
            });
        }

        let mut session_ok = false;
        let start = tokio::time::Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Some(msg) = rx.recv().await {
                if msg == "SESSION_ESTABLISHED" {
                    println!("VPN session established!");
                    session_ok = true;
                    break;
                } else {
                }
            } else {
                break;
            }
        }

        if !session_ok {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout waiting for ESP session established",
            ));
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
