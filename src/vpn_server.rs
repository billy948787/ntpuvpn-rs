use std::process::Stdio;
use std::time::Duration;

use crate::utils;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

pub struct VpnSession {
    process: Child,
    pub interface_index: u32,
}

impl VpnSession {
    pub async fn new(server: &str, user: &str, password: &str) -> std::io::Result<Self> {
        let free_interface_str = utils::generate_free_interface_name("utun");

        let mut process = Command::new("openconnect")
            .arg("--protocol=nc")
            .arg(format!("--user={}", user))
            .arg("--passwd-on-stdin")
            .arg("--non-inter")
            .arg(format!("--interface={}", free_interface_str))
            .arg(server)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        if let Some(mut stdin) = process.stdin.take() {
            stdin.write_all(password.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }

        let stdout_task = process
            .stdout
            .take()
            .map(|stdout| Self::log_output(stdout, false));

        let stderr_task = process
            .stderr
            .take()
            .map(|stderr| Self::log_output(stderr, true));

        let new_interface = match Self::wait_for_interface(
            &free_interface_str,
            &mut process,
            Duration::from_secs(30),
        )
        .await
        {
            Ok(interface) => interface,
            Err(error) => {
                let _ = process.kill().await;
                let _ = process.wait().await;
                if let Some(task) = stdout_task {
                    let _ = task.await;
                }
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                return Err(error);
            }
        };

        println!("VPN session established on {}", new_interface.name);

        Ok(Self {
            process,
            interface_index: new_interface.index,
        })
    }

    fn log_output<R>(output: R, stderr: bool) -> tokio::task::JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let mut lines = BufReader::new(output).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stderr {
                    eprintln!("openconnect: {}", line);
                } else {
                    println!("openconnect: {}", line);
                }
            }
        })
    }

    async fn wait_for_interface(
        interface_name: &str,
        process: &mut Child,
        timeout: Duration,
    ) -> std::io::Result<NetworkInterface> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < timeout {
            if let Some(new_iface) = network_interface::NetworkInterface::show()
                .map_err(|e| {
                    std::io::Error::other(format!("Failed to get network interfaces: {}", e))
                })?
                .into_iter()
                .find(|i| i.name == interface_name)
            {
                return Ok(new_iface);
            }

            if let Some(status) = process.try_wait()? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    format!("openconnect exited before establishing the VPN ({status})"),
                ));
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("VPN interface {interface_name} did not appear within 30 seconds"),
        ))
    }
}

impl Drop for VpnSession {
    fn drop(&mut self) {
        let _ = self.process.start_kill();
    }
}
