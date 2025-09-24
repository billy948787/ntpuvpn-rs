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
            println!("Storing password in keyring.");
            io::stdout().flush().unwrap();
            let password = read_password().unwrap();
            keyring_entry
                .set_password(&password)
                .expect("Failed to store password in keyring");
            password
        }
    };
    // first get dafault interface
    if let Some(default_interface) = ntpuvpn_rs::utils::get_default_interface() {
        println!("Default interface: {}", default_interface.name);

        let handle = Handle::new().expect("Failed to create route handle");
        let default_route = handle
            .default_route()
            .await
            .expect("Failed to get default route");
        println!("Default route: {:?}", default_route);

        // Start VPN session
        let _vpn_session = VpnSession::new("ntpu.twaren.net", &config.username, &password)
            .await
            .expect("Failed to start VPN session");

        let mut reroute_server = RerouteServer::new(default_interface, default_route)
            .await
            .expect("Failed to create reroute server");

        reroute_server
            .run()
            .await
            .expect("Failed to run reroute server");
    }
}

fn prompt_config() -> Config {
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

    Config {
        username,

        vpn_network,
        vpn_mask,
    }
}
