use std::{
    io::Write,
    os::unix::{fs::PermissionsExt, process::CommandExt},
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
        let free_interface_str = utils::generate_free_interface_name("tun");

        const VPNC_SCRIPT_CONTENT: &str = "#!/bin/sh\nexit 0\n";

        let mut vpnc_script = tempfile::NamedTempFile::new()?;
        vpnc_script.write_all(VPNC_SCRIPT_CONTENT.as_bytes())?;

        let mut perms = vpnc_script.as_file().metadata()?.permissions();
        perms.set_mode(0o755);
        vpnc_script.as_file().set_permissions(perms)?;

        let vpnc_script_path = "vpnc_script.sh";

        vpnc_script.into_temp_path().persist(vpnc_script_path)?;

        let mut process = Command::new("openconnect")
            .arg("--protocol=pulse")
            .arg("--user")
            .arg(user)
            .arg("--passwd-on-stdin")
            .arg(server)
            .arg(format!("--interface={}", free_interface_str))
            .arg(format!("--script"))
            .arg(vpnc_script_path)
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

        Ok(Self {
            process,
            interface: Self::wait_for_interface(&free_interface_str, Duration::from_secs(10))?,
        })
    }
    fn wait_for_interface(
        interface_name: &str,
        timeout: Duration,
    ) -> std::io::Result<datalink::NetworkInterface> {
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            if let Some(iface) = datalink::interfaces()
                .into_iter()
                .find(|i| i.name == interface_name)
            {
                return Ok(iface);
            }
            if start.elapsed() > timeout {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Interface not found within timeout",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Interface not found within timeout",
        ))
    }
}

impl Drop for VpnSession {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
