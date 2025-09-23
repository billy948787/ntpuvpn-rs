use futures::executor::block_on;
use net_route::{Handle, Route};
use pnet::{datalink, packet::ipv4::Ipv4Packet};
use std::{net::Ipv4Addr, process::Command};
use tun_rs::{DeviceBuilder, SyncDevice};

pub struct RerouteServer {
    interface_name: String,
    device: SyncDevice,
    default_interface: datalink::NetworkInterface,
    vpn_interface: datalink::NetworkInterface,
    vpn_network: Ipv4Addr,
    vpn_mask: Ipv4Addr,
    orig_default_route: Option<Route>,
    new_default_route: Route,
    route_handle: Handle,
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

        let handle = Handle::new()?;

        let mut default_route = None;
        let new_route =
            Route::new("0.0.0.0".parse().unwrap(), 0).with_ifindex(device.if_index().unwrap());

        block_on(async {
            if let Ok(orig_route) = handle.default_route().await {
                default_route = orig_route.clone();
            }

            handle.add(&new_route).await.unwrap();
        });

        Ok(Self {
            device,
            default_interface,
            vpn_interface,
            vpn_network,
            vpn_mask,
            interface_name: interface_name.to_string(),
            orig_default_route: default_route,
            route_handle: handle,
            new_default_route: new_route,
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
        block_on(async {
            // delete the new route

            if let Err(e) = self.route_handle.delete(&self.new_default_route).await {
                eprintln!("Failed to delete new default route: {}", e);
            }

            if let Some(orig_route) = &self.orig_default_route {
                if let Err(e) = self.route_handle.add(orig_route).await {
                    eprintln!("Failed to restore original default route: {}", e);
                }
            }
        });
    }
}
