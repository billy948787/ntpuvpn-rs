use futures::executor::block_on;
use libarp;
use net_route::{Handle, Route};
use pnet::{
    datalink::{self, Channel, Config},
    packet::{
        Packet,
        ethernet::{EtherTypes, MutableEthernetPacket},
        ipv4::{Ipv4, Ipv4Packet, MutableIpv4Packet},
    },
    util::MacAddr,
};
use std::{net::Ipv4Addr, process::Command};
use tokio::runtime::Runtime;
use tun_rs::{AsyncDevice, DeviceBuilder, SyncDevice};

pub struct RerouteServer {
    interface_name: String,
    device: AsyncDevice,
    default_interface: datalink::NetworkInterface,
    default_tx: Box<dyn datalink::DataLinkSender>,
    vpn_interface: datalink::NetworkInterface,
    vpn_tx: Box<dyn datalink::DataLinkSender>,
    vpn_network: Ipv4Addr,
    vpn_mask: Ipv4Addr,
    orig_default_route: Option<Route>,
    new_default_route: Route,
    route_handle: Handle,
    arp_client: libarp::client::ArpClient,
}

impl RerouteServer {
    pub async fn new(
        interface_name: &str,
        default_interface: datalink::NetworkInterface,
        original_route: Option<Route>,
        vpn_interface: datalink::NetworkInterface,
        vpn_network: Ipv4Addr,
        vpn_mask: Ipv4Addr,
    ) -> std::io::Result<Self> {
        let virtual_ip = Ipv4Addr::new(192, 0, 2, 1);
        let device = DeviceBuilder::new()
            .name(interface_name)
            .ipv4(virtual_ip, "255.255.255.0", None)
            .mtu(1500)
            .build_async()?;

        println!("new device: {:?}", device.name());

        let handle = Handle::new()?;

        if let Ok(Some(current_route_with_vpn)) = handle.default_route().await {
            println!(
                "Removing existing default route: {:?}",
                current_route_with_vpn
            );
            handle.delete(&current_route_with_vpn).await?;
        }

        let new_route = Route::new("0.0.0.0".parse().unwrap(), 0)
            .with_ifindex(device.if_index().unwrap())
            .with_gateway(virtual_ip.into());

        println!("Adding new default route: {:?}", new_route);

        handle.add(&new_route).await?;

        let arp_client =
            libarp::client::ArpClient::new_with_iface_name(&default_interface.name).unwrap();

        let default_datalink = datalink::channel(&default_interface, Default::default())?;

        let vpn_datalink = datalink::channel(&vpn_interface, Default::default())?;
        if let (Channel::Ethernet(tx, _), Channel::Ethernet(vpn_tx, _)) =
            (default_datalink, vpn_datalink)
        {
            Ok(Self {
                interface_name: interface_name.to_string(),
                device,
                default_interface,
                default_tx: tx,
                vpn_interface,
                vpn_tx,
                vpn_network,
                vpn_mask,
                orig_default_route: original_route,
                new_default_route: new_route,
                route_handle: handle,
                arp_client,
            })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to create datalink channel for default interface",
            ))
        }
    }

    pub async fn run(&mut self) -> std::io::Result<()> {
        let mut buf = [0u8; 65535];
        loop {
            let nbytes = self.device.recv(&mut buf).await?;

            if nbytes == 0 {
                continue;
            }

            self.reroute_packet(&buf[..nbytes]).await?;
        }
    }

    async fn reroute_packet(&mut self, packet: &[u8]) -> std::io::Result<()> {
        if packet.len() < 20 {
            return Ok(());
        }
        // check the destination IP and reroute accordingly
        if let Some(ipv4_packet) = Ipv4Packet::new(packet) {
            let dest_ip = ipv4_packet.get_destination();
            if dest_ip & self.vpn_mask == self.vpn_network {
                // Reroute to VPN interface
                // println!(
                //     "Rerouting packet to VPN interface {}: {:?}",
                //     self.vpn_interface, dest_ip
                // );

                let source_mac = self.vpn_interface.mac.unwrap();
                let dest_mac = self.arp_client.ip_to_mac(dest_ip, None).await?;

                Self::forward_to_interface(
                    &mut self.vpn_tx,
                    packet,
                    source_mac,
                    MacAddr {
                        0: dest_mac.0,
                        1: dest_mac.1,
                        2: dest_mac.2,
                        3: dest_mac.3,
                        4: dest_mac.4,
                        5: dest_mac.5,
                    },
                )
                .await?;
            } else {
                // Reroute to default interface
                // println!(
                //     "Rerouting packet to default interface {}: {:?}",
                //     self.default_interface, dest_ip
                // );

                let source_mac = self.default_interface.mac.unwrap();
                let dest_mac = self.arp_client.ip_to_mac(dest_ip, None).await?;

                Self::forward_to_interface(
                    &mut self.default_tx,
                    packet,
                    source_mac,
                    MacAddr {
                        0: dest_mac.0,
                        1: dest_mac.1,
                        2: dest_mac.2,
                        3: dest_mac.3,
                        4: dest_mac.4,
                        5: dest_mac.5,
                    },
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn forward_to_interface(
        tx: &mut Box<dyn datalink::DataLinkSender>,
        _packet: &[u8],
        source_mac: MacAddr,
        dest_mac: MacAddr,
    ) -> std::io::Result<()> {
        let mut ethernet_buffer = [0u8; 1500]; // 假設 Ethernet MTU
        let mut ethernet_packet = MutableEthernetPacket::new(&mut ethernet_buffer).unwrap();
        ethernet_packet.set_source(source_mac);
        ethernet_packet.set_destination(dest_mac);
        ethernet_packet.set_ethertype(EtherTypes::Ipv4);
        ethernet_packet.set_payload(_packet);

        let ethernet_data = ethernet_packet.packet();

        match tx.send_to(ethernet_data, None) {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to send packet",
            )),
        }
    }
}
impl Drop for RerouteServer {
    fn drop(&mut self) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
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
