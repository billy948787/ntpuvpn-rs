use network_interface::NetworkInterfaceConfig;

pub fn check_free_interface_name(name: &str) -> bool {
    let interfaces = network_interface::NetworkInterface::show()
        .map_err(|e| {
            eprintln!("Failed to get network interfaces: {}", e);
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to get network interfaces",
            )
        })
        .unwrap_or_else(|_| vec![]);
    for interface in interfaces {
        if interface.name == name {
            return false;
        }
    }
    true
}

pub fn generate_free_interface_name(base: &str) -> String {
    let mut index = 0;
    loop {
        let candidate = format!("{}{}", base, index);
        if check_free_interface_name(&candidate) {
            return candidate;
        }
        index += 1;
    }
}
