use std::net::IpAddr;

use net_route::{Handle, Route};
use pnet::datalink;
use tokio::runtime::Runtime;

pub struct RerouteServer {
    orig_default_route: Option<Route>,
    vpn_route: Route,
    new_default_route: Route,
    handle: Handle,
}

impl RerouteServer {
    pub async fn new(
        default_interface: datalink::NetworkInterface,
        vpn_interface: datalink::NetworkInterface,
        original_route: Option<Route>,
    ) -> std::io::Result<Self> {
        let handle = Handle::new()?;

        if let Ok(Some(current_route_with_vpn)) = handle.default_route().await {
            println!(
                "Removing existing default route: {:?}",
                current_route_with_vpn
            );
            handle.delete(&current_route_with_vpn).await?;
        }

        let vpn_route = Route::new(IpAddr::V4("10.0.0.0".parse().unwrap()), 8)
            // .with_ifindex(vpn_interface.index);
            .with_ifindex(vpn_interface.index);

        let default_route = Route::new(IpAddr::V4("0.0.0.0".parse().unwrap()), 0)
            .with_ifindex(default_interface.index)
            .with_gateway(original_route.clone().unwrap().gateway.unwrap());

        Self::add_ignore_exists(&handle, &default_route).await?;
        Self::add_ignore_exists(&handle, &vpn_route).await?;

        Ok(RerouteServer {
            orig_default_route: original_route,
            vpn_route,
            new_default_route: default_route,
            handle,
        })
    }
    async fn add_ignore_exists(handle: &Handle, route: &Route) -> std::io::Result<()> {
        match handle.add(route).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                println!("Route already exists, skipping add: {:?}", route);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
    pub async fn run(&mut self) -> std::io::Result<()> {
        loop {}
    }
}
impl Drop for RerouteServer {
    fn drop(&mut self) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            self.handle.delete(&self.vpn_route).await.ok();
            self.handle.delete(&self.new_default_route).await.ok();

            if let Some(orig_route) = &self.orig_default_route {
                self.handle.add(orig_route).await.ok();
            }
        });
    }
}
