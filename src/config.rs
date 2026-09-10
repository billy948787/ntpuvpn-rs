use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub username: String,
    pub vpn_network: Ipv4Addr,
    pub vpn_mask: Ipv4Addr,
}

impl Config {
    pub fn vpn_prefix(&self) -> std::io::Result<u8> {
        let mask = u32::from(self.vpn_mask);
        let prefix = mask.leading_ones() as u8;
        let expected = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);

        if mask != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("VPN netmask {} is not contiguous", self.vpn_mask),
            ));
        }

        Ok(prefix)
    }

    pub fn normalized_vpn_network(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.vpn_network) & u32::from(self.vpn_mask))
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use std::net::Ipv4Addr;

    fn config(mask: Ipv4Addr) -> Config {
        Config {
            username: "user".into(),
            vpn_network: Ipv4Addr::new(10, 1, 2, 3),
            vpn_mask: mask,
        }
    }

    #[test]
    fn converts_netmask_to_prefix() {
        assert_eq!(config(Ipv4Addr::new(255, 0, 0, 0)).vpn_prefix().unwrap(), 8);
        assert_eq!(config(Ipv4Addr::UNSPECIFIED).vpn_prefix().unwrap(), 0);
        assert_eq!(config(Ipv4Addr::BROADCAST).vpn_prefix().unwrap(), 32);
    }

    #[test]
    fn rejects_non_contiguous_netmask() {
        assert!(config(Ipv4Addr::new(255, 0, 255, 0)).vpn_prefix().is_err());
    }

    #[test]
    fn normalizes_network_address() {
        assert_eq!(
            config(Ipv4Addr::new(255, 0, 0, 0)).normalized_vpn_network(),
            Ipv4Addr::new(10, 0, 0, 0)
        );
    }
}
