use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub username: String,
    pub vpn_network: Ipv4Addr,
    pub vpn_mask: Ipv4Addr,
}
