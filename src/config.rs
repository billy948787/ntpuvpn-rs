use std::collections::HashMap;
use std::io;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub username: String,
    pub vpn_network: Ipv4Addr,
    pub vpn_mask: Ipv4Addr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            username: String::new(),
            vpn_network: Ipv4Addr::new(10, 0, 0, 0),
            vpn_mask: Ipv4Addr::new(255, 0, 0, 0),
        }
    }
}

// Deliberately no Debug/Serialize implementation: passwords must not appear in
// diagnostics or be written to config.json.
#[derive(Default)]
pub struct EnvSettings {
    pub username: Option<String>,
    pub password: Option<String>,
    vpn_network: Option<Ipv4Addr>,
    vpn_mask: Option<Ipv4Addr>,
}

impl EnvSettings {
    pub fn load() -> io::Result<Self> {
        // Only read .env in the working directory, never search parent folders
        // or mutate the process environment (unsafe in multithreaded Rust).
        let file_values = match std::fs::File::open(".env") {
            Ok(file) => parse_env_file(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(_) => return Err(io::Error::other("Failed to read .env")),
        };
        Self::from_sources(&file_values, |name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(invalid_env(name)),
        })
    }

    fn from_sources(
        file_values: &HashMap<String, String>,
        mut process_value: impl FnMut(&str) -> io::Result<Option<String>>,
    ) -> io::Result<Self> {
        let mut value = |name: &str| -> io::Result<Option<String>> {
            let value = process_value(name)?.or_else(|| file_values.get(name).cloned());
            if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
                return Err(invalid_env(name));
            }
            Ok(value)
        };
        let username = value("NTPUVPN_USERNAME")?.map(|value| value.trim().to_owned());
        let password = value("NTPUVPN_PASSWORD")?;
        let vpn_network = value("NTPUVPN_VPN_NETWORK")?
            .map(|value| {
                value
                    .trim()
                    .parse()
                    .map_err(|_| invalid_env("NTPUVPN_VPN_NETWORK"))
            })
            .transpose()?;
        let vpn_mask = value("NTPUVPN_VPN_MASK")?
            .map(|value| {
                value
                    .trim()
                    .parse()
                    .map_err(|_| invalid_env("NTPUVPN_VPN_MASK"))
            })
            .transpose()?;
        Ok(Self {
            username,
            password,
            vpn_network,
            vpn_mask,
        })
    }

    pub fn apply_config(&self, mut config: Config) -> io::Result<Config> {
        if let Some(username) = &self.username {
            config.username = username.clone();
        }
        if let Some(network) = self.vpn_network {
            config.vpn_network = network;
        }
        if let Some(mask) = self.vpn_mask {
            config.vpn_mask = mask;
        }
        config.vpn_prefix()?;
        Ok(config)
    }
}

fn invalid_env(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Invalid or empty {name}"),
    )
}

fn parse_env_file(reader: impl io::Read) -> io::Result<HashMap<String, String>> {
    dotenvy::from_read_iter(reader)
        .map(|entry| {
            // dotenv parser errors can contain the whole line, including a
            // password. Never forward that error or its source to diagnostics.
            entry.map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid .env syntax"))
        })
        .collect()
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
    use super::*;
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

    fn settings(values: &[(&str, &str)]) -> io::Result<EnvSettings> {
        let file = values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        EnvSettings::from_sources(&file, |_| Ok(None))
    }

    #[test]
    fn environment_overrides_dotenv_and_saved_config() {
        let file = HashMap::from([
            ("NTPUVPN_USERNAME".into(), "file-user".into()),
            ("NTPUVPN_PASSWORD".into(), "file-password".into()),
        ]);
        let env = EnvSettings::from_sources(&file, |name| {
            Ok(match name {
                "NTPUVPN_USERNAME" => Some("env-user".into()),
                "NTPUVPN_PASSWORD" => Some("env-password".into()),
                _ => None,
            })
        })
        .unwrap();
        let stored = Config {
            username: "stored-user".into(),
            ..Default::default()
        };
        assert_eq!(env.apply_config(stored).unwrap().username, "env-user");
        assert_eq!(env.password.as_deref(), Some("env-password"));
    }

    #[test]
    fn unset_environment_keeps_saved_settings() {
        let stored = Config {
            username: "stored-user".into(),
            vpn_network: Ipv4Addr::new(192, 168, 1, 0),
            vpn_mask: Ipv4Addr::new(255, 255, 255, 0),
        };
        let env = settings(&[]).unwrap();
        let resolved = env.apply_config(stored).unwrap();
        assert_eq!(resolved.username, "stored-user");
        assert_eq!(resolved.vpn_prefix().unwrap(), 24);
        assert_eq!(resolved.vpn_network, Ipv4Addr::new(192, 168, 1, 0));
        assert!(env.password.is_none());
    }

    #[test]
    fn dotenv_credentials_work_without_a_saved_config() {
        let env = settings(&[
            ("NTPUVPN_USERNAME", "student"),
            ("NTPUVPN_PASSWORD", "secret"),
        ])
        .unwrap();
        let resolved = env.apply_config(Config::default()).unwrap();
        assert_eq!(resolved.username, "student");
        assert_eq!(resolved.vpn_prefix().unwrap(), 8);
        assert_eq!(env.password.as_deref(), Some("secret"));
    }

    #[test]
    fn overrides_vpn_network_and_mask() {
        let env = settings(&[
            ("NTPUVPN_VPN_NETWORK", "192.168.10.42"),
            ("NTPUVPN_VPN_MASK", "255.255.255.0"),
        ])
        .unwrap();
        let resolved = env.apply_config(Config::default()).unwrap();
        assert_eq!(resolved.vpn_prefix().unwrap(), 24);
        assert_eq!(
            resolved.normalized_vpn_network(),
            Ipv4Addr::new(192, 168, 10, 0)
        );
    }

    #[test]
    fn rejects_empty_and_invalid_values() {
        for name in [
            "NTPUVPN_USERNAME",
            "NTPUVPN_PASSWORD",
            "NTPUVPN_VPN_NETWORK",
            "NTPUVPN_VPN_MASK",
        ] {
            assert!(settings(&[(name, "")]).is_err());
            assert!(settings(&[(name, "   ")]).is_err());
        }
        assert!(settings(&[("NTPUVPN_VPN_NETWORK", "::1")]).is_err());
        assert!(settings(&[("NTPUVPN_VPN_MASK", "not-a-mask")]).is_err());
        let env = settings(&[("NTPUVPN_VPN_MASK", "255.0.255.0")]).unwrap();
        assert!(env.apply_config(Config::default()).is_err());
    }

    #[test]
    fn password_whitespace_is_preserved() {
        let env = settings(&[
            ("NTPUVPN_USERNAME", " student "),
            ("NTPUVPN_PASSWORD", " secret "),
        ])
        .unwrap();
        assert_eq!(env.username.as_deref(), Some("student"));
        assert_eq!(env.password.as_deref(), Some(" secret "));
    }

    #[test]
    fn dotenv_supports_quotes_and_does_not_expose_parse_errors() {
        let file =
            parse_env_file(b"NTPUVPN_PASSWORD='secret $ with # spaces'\n".as_slice()).unwrap();
        assert_eq!(file["NTPUVPN_PASSWORD"], "secret $ with # spaces");
        let error = parse_env_file(b"NTPUVPN_PASSWORD='DO_NOT_LOG_THIS\n".as_slice()).unwrap_err();
        assert_eq!(error.to_string(), "Invalid .env syntax");
        assert!(!format!("{error:?}").contains("DO_NOT_LOG_THIS"));
    }

    #[test]
    fn empty_process_value_is_not_replaced_with_dotenv_password() {
        let file = HashMap::from([("NTPUVPN_PASSWORD".into(), "secret".into())]);
        assert!(EnvSettings::from_sources(&file, |_| Ok(Some(String::new()))).is_err());
    }
}
