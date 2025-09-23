use std::io::{self, Write};
use std::net::Ipv4Addr;

use net_route::{Handle, Route};
use ntpuvpn_rs::config::Config;
use ntpuvpn_rs::reroute_server::RerouteServer;
use ntpuvpn_rs::vpn_server::VpnSession;
use rpassword::read_password;
use serde::{Deserialize, Serialize};

fn main() {
    let config = prompt_config();
    // first get dafault interface
    if let Some(default_interface) = ntpuvpn_rs::utils::get_default_interface() {
        println!("Default interface: {}", default_interface.name);

        // Start VPN session
        let vpn_session = VpnSession::new(&config.server, &config.username, &config.password)
            .expect("Failed to start VPN session");

        let mut reroute_server = RerouteServer::new(
            ntpuvpn_rs::utils::generate_free_interface_name("utun").as_str(),
            default_interface,
            vpn_session.interface.clone(),
            config.vpn_network,
            config.vpn_mask,
        )
        .expect("Failed to create reroute server");

        reroute_server.run().expect("Failed to run reroute server");
    }
}

fn prompt_config() -> Config {
    println!("Enter VPN details:");

    print!("Server: ");
    io::stdout().flush().unwrap();
    let mut server = String::new();
    io::stdin().read_line(&mut server).unwrap();
    let server = server.trim().to_string();

    print!("Username: ");
    io::stdout().flush().unwrap();
    let mut username = String::new();
    io::stdin().read_line(&mut username).unwrap();
    let username = username.trim().to_string();
    print!("Password: ");
    io::stdout().flush().unwrap();

    let password = read_password().unwrap();

    let vpn_network = Ipv4Addr::new(10, 0, 0, 0);
    let vpn_mask = Ipv4Addr::new(255, 0, 0, 0);

    Config {
        server,
        username,
        password,
        vpn_network,
        vpn_mask,
    }
}
