use std::{net::Ipv4Addr, process::Command};

use pnet::{datalink, packet::ipv4::Ipv4Packet};
use tun_rs::{DeviceBuilder, SyncDevice};

pub struct RerouteServer {
    interface_name: String,
    device: SyncDevice,
    default_interface: datalink::NetworkInterface,
    vpn_interface: datalink::NetworkInterface,
    vpn_network: Ipv4Addr,
    vpn_mask: Ipv4Addr,
}

impl RerouteServer {
    pub fn new(
        interface_name: &str,
        default_interface: datalink::NetworkInterface,
        vpn_interface: datalink::NetworkInterface,
        vpn_network: Ipv4Addr,
        vpn_mask: Ipv4Addr,
    ) -> std::io::Result<Self> {
        let device = DeviceBuilder::new()
            .name(interface_name)
            .mtu(1500)
            .build_sync()?;

        Command::new("ip")
            .arg("route")
            .arg("add")
            .arg("default")
            .arg("dev")
            .arg(interface_name)
            .arg("metric")
            .arg("0")
            .output()?;

        Ok(Self {
            device,
            default_interface,
            vpn_interface,
            vpn_network,
            vpn_mask,
            interface_name: interface_name.to_string(),
        })
    }

    pub fn run(&mut self) -> std::io::Result<()> {
        let mut buf = [0u8; 65535];
        loop {
            let nbytes = self.device.recv(&mut buf)?;

            if nbytes == 0 {
                continue;
            }

            self.reroute_packet(&buf[..nbytes])?;
        }
    }

    fn reroute_packet(&self, packet: &[u8]) -> std::io::Result<()> {
        if packet.len() < 20 {
            return Ok(());
        }
        // check the destination IP and reroute accordingly
        if let Some(ipv4_packet) = Ipv4Packet::new(packet) {
            let dest_ip = ipv4_packet.get_destination();
            if dest_ip & self.vpn_mask == self.vpn_network {
                // Reroute to VPN interface
                println!(
                    "Rerouting packet to VPN interface {}: {:?}",
                    self.vpn_interface, dest_ip
                );

                self.forward_to_interface(&self.vpn_interface, packet)?;
            } else {
                // Reroute to default interface
                println!(
                    "Rerouting packet to default interface {}: {:?}",
                    self.default_interface, dest_ip
                );

                self.forward_to_interface(&self.default_interface, packet)?;
            }
        }

        Ok(())
    }

    fn forward_to_interface(
        &self,
        _interface: &datalink::NetworkInterface,
        _packet: &[u8],
    ) -> std::io::Result<()> {
        if let datalink::Channel::Ethernet(mut tx, _) =
            datalink::channel(&_interface, Default::default())?
        {
            tx.send_to(_packet, None);
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to create datalink channel",
            ));
        }

        Ok(())
    }
}

impl Drop for RerouteServer {
    fn drop(&mut self) {
        if let Err(e) = Command::new("ip")
            .arg("route")
            .arg("del")
            .arg("default")
            .arg("dev")
            .arg(&self.interface_name)
            .output()
        {
            eprintln!("Failed to remove route: {}", e);
        }
    }
}
