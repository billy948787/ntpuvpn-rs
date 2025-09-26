use std::fs;
use std::io::{self, Write};
use std::net::Ipv4Addr;

use net_route::Handle;
use ntpuvpn_rs::config::Config;
use ntpuvpn_rs::reroute_server::RerouteServer;
use ntpuvpn_rs::vpn_server::VpnSession;
use rpassword::read_password;
#[tokio::main]
async fn main() {
    let config = prompt_config();

    let service = "ntpuvpn-rs";

    let keyring_entry =
        keyring::Entry::new(service, &config.username).expect("Failed to create keyring entry");
    let password = match keyring_entry.get_password() {
        Ok(pwd) => {
            println!("Retrieved password from keyring.");
            pwd
        }
        Err(_) => {
            io::stdout().flush().unwrap();
            let password = read_password().unwrap();

            password
        }
    };

    let handle = Handle::new().expect("Failed to create netlink handle");

    if let Some(default_route) = handle
        .default_route()
        .await
        .expect("Failed to get default route")
    {
        println!("Default route: {:?}", default_route);

        // Start VPN session
        let _vpn_session = VpnSession::new("ntpu.twaren.net", &config.username, &password)
            .await
            .expect("Failed to start VPN session");

        // if the vpn session is established, set keyring password
        keyring_entry
            .set_password(&password)
            .expect("Failed to set password in keyring");

        let mut reroute_server = RerouteServer::new(
            default_route
                .ifindex
                .expect("Default route has no interface index"),
            _vpn_session.interface_index,
            Some(default_route),
        )
        .await
        .expect("Failed to create reroute server");

        reroute_server
            .run()
            .await
            .expect("Failed to run reroute server");
    }
}

fn prompt_config() -> Config {
    let config_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("ntpuvpn-rs");
    let config_path = config_dir.join("config.json");

    if config_path.exists() {
        let config_data =
            fs::read_to_string(&config_path).expect("Failed to read existing config file");
        let config: Config =
            serde_json::from_str(&config_data).expect("Failed to parse existing config file");
        return config;
    }
    println!("Enter VPN details:");

    print!("Username: ");
    io::stdout().flush().unwrap();
    let mut username = String::new();
    io::stdin().read_line(&mut username).unwrap();
    let username = username.trim().to_string();
    print!("Password: ");
    io::stdout().flush().unwrap();

    let vpn_network = Ipv4Addr::new(10, 0, 0, 0);
    let vpn_mask = Ipv4Addr::new(255, 0, 0, 0);

    let config = Config {
        username,

        vpn_network,
        vpn_mask,
    };

    fs::create_dir_all(&config_dir).expect("Failed to create config directory");
    let config_path = config_dir.join("config.json");
    let config_json = serde_json::to_string_pretty(&config).expect("Failed to serialize config");
    fs::write(&config_path, config_json).expect("Failed to write config file");
    println!("Config saved to: {}", config_path.display());

    config
}
