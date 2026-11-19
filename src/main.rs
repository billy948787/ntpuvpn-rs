use std::fs;
use std::io::{self, IsTerminal, Write};

use net_route::Handle;
use ntpuvpn_rs::config::{Config, EnvSettings};
#[cfg(unix)]
use ntpuvpn_rs::reroute_server::RerouteServer;
use ntpuvpn_rs::vpn_server::VpnSession;
use rpassword::read_password;

#[cfg(windows)]
mod windows_elevation;

#[tokio::main]
async fn main() -> io::Result<()> {
    #[cfg(windows)]
    if windows_elevation::prepare().await? {
        return Ok(());
    }

    let env = EnvSettings::load()?;
    let config = prompt_config(&env)?;

    let password = match env.password {
        Some(password) => password,
        None => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "No password available; set NTPUVPN_PASSWORD or run interactively",
                ));
            }
            print!("Password: ");
            io::stdout().flush()?;
            read_password()?
        }
    };

    #[cfg(windows)]
    windows_elevation::require_elevated()?;

    let vpn_prefix = config.vpn_prefix()?;
    let handle = Handle::new()?;

    let Some(default_route) = handle.default_route().await? else {
        return Err(io::Error::new(
            io::ErrorKind::NetworkUnreachable,
            "No default route was found",
        ));
    };
    println!("Default route: {:?}", default_route);

    // Start VPN session
    let mut vpn_session = match VpnSession::new(
        "ntpu.twaren.net",
        &config.username,
        &password,
        config.normalized_vpn_network(),
        vpn_prefix,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            eprintln!("Failed to start VPN session: {error}");
            return Err(error);
        }
    };

    #[cfg(unix)]
    let mut reroute_server = match RerouteServer::new(
        vpn_session.interface_index,
        config.normalized_vpn_network(),
        vpn_prefix,
        default_route,
    )
    .await
    {
        Ok(server) => server,
        Err(error) => {
            let _ = vpn_session.shutdown().await;
            eprintln!("Failed to configure VPN routes: {error}");
            return Err(error);
        }
    };

    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            if let Err(error) = signal {
                eprintln!("Failed to listen for Ctrl+C: {error}");
            } else {
                println!("Received Ctrl+C, exiting...");
            }
        },
        status = vpn_session.wait() => match status {
            Ok(()) => eprintln!("VPN connection ended"),
            Err(error) => eprintln!("VPN connection ended with an error: {error}"),
        },
    }

    #[cfg(unix)]
    if let Err(error) = reroute_server.shutdown().await {
        eprintln!("Failed to remove VPN route: {error}");
    }
    if let Err(error) = vpn_session.shutdown().await {
        eprintln!("Failed to stop VPN connection: {error}");
    }
    Ok(())
}

fn prompt_config(env: &EnvSettings) -> io::Result<Config> {
    #[cfg(windows)]
    let config_dir = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(dirs::config_dir);
    #[cfg(not(windows))]
    let config_dir = dirs::config_dir();
    let config_path = config_dir.map(|dir| dir.join("ntpuvpn-rs/config.json"));
    let mut stored = match &config_path {
        Some(path) => match fs::read_to_string(path) {
            Ok(data) => serde_json::from_str::<Config>(&data)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid config.json"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Config::default(),
            Err(error) => return Err(error),
        },
        None => Config::default(),
    };
    let mut config = env.apply_config(stored.clone())?;
    if env.username.is_some() {
        // Env overrides are temporary; no config file is created or changed.
        return Ok(config);
    }

    if !io::stdin().is_terminal() {
        if !config.username.trim().is_empty() {
            return Ok(config);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "No username available; set NTPUVPN_USERNAME or run interactively",
        ));
    }
    if config.username.trim().is_empty() {
        print!("Username: ");
    } else {
        print!("Username [{}]: ", config.username);
    }
    io::stdout().flush()?;
    let mut username = String::new();
    io::stdin().read_line(&mut username)?;
    let username = username.trim();
    if !username.is_empty() {
        config.username = username.to_owned();
        stored.username = username.to_owned();
    }
    if config.username.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Username cannot be empty",
        ));
    }

    if username.is_empty() {
        return Ok(config);
    }
    let config_path =
        config_path.ok_or_else(|| io::Error::other("No config directory available"))?;
    fs::create_dir_all(config_path.parent().unwrap())?;
    let config_json = serde_json::to_string_pretty(&stored).map_err(io::Error::other)?;
    fs::write(&config_path, config_json)?;
    println!("Config saved to: {}", config_path.display());
    Ok(config)
}
