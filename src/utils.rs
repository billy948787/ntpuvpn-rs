use net_route::Handle;
use pnet::datalink::{self, NetworkInterface};

// may not work on all platforms
pub fn get_default_interface() -> Option<datalink::NetworkInterface> {
    let interfaces = datalink::interfaces();
    for interface in interfaces {
        if interface.is_up() && !interface.is_loopback() && !interface.ips.is_empty() {
            return Some(interface);
        }
    }
    None
}

pub fn check_free_interface_name(name: &str) -> bool {
    let interfaces = datalink::interfaces();
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
