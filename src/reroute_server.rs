use std::net::{IpAddr, Ipv4Addr};

use net_route::{Handle, Route};

pub struct RerouteServer {
    vpn_route: Route,
    vpn_route_added: bool,
}

impl RerouteServer {
    pub async fn new(
        vpn_interface_index: u32,
        vpn_network: Ipv4Addr,
        vpn_prefix: u8,
        original_route: Route,
    ) -> std::io::Result<Self> {
        let handle = Handle::new()?;

        // OpenConnect may install a full-tunnel default route. Restore the route
        // captured before connecting so only the configured VPN network is tunneled.
        if let Some(current_route) = handle.default_route().await?
            && current_route != original_route
        {
            println!("Removing VPN default route: {:?}", current_route);
            handle.delete(&current_route).await?;
        }
        Self::add_ignore_exists(&handle, &original_route).await?;

        Self::for_interface(vpn_interface_index, vpn_network, vpn_prefix).await
    }

    pub async fn for_interface(
        vpn_interface_index: u32,
        vpn_network: Ipv4Addr,
        vpn_prefix: u8,
    ) -> std::io::Result<Self> {
        let handle = Handle::new()?;
        let vpn_route =
            Route::new(IpAddr::V4(vpn_network), vpn_prefix).with_ifindex(vpn_interface_index);
        let vpn_route_added = Self::add_ignore_exists(&handle, &vpn_route).await?;

        Ok(Self {
            vpn_route,
            vpn_route_added,
        })
    }

    async fn add_ignore_exists(handle: &Handle, route: &Route) -> std::io::Result<bool> {
        if handle
            .list()
            .await?
            .iter()
            .any(|existing| same_route(existing, route))
        {
            println!("Route already exists, skipping add: {:?}", route);
            return Ok(false);
        }

        handle.add(route).await?;
        Ok(true)
    }

    pub async fn shutdown(&mut self) -> std::io::Result<()> {
        if !self.vpn_route_added {
            return Ok(());
        }

        let handle = Handle::new()?;
        match handle.delete(&self.vpn_route).await {
            Ok(()) => {
                self.vpn_route_added = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.vpn_route_added = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn same_route(existing: &Route, requested: &Route) -> bool {
    #[cfg(not(windows))]
    {
        existing == requested
    }

    // Windows fills in LUID, metric and an unspecified on-link gateway when
    // listing routes. Those defaults must not make an existing route invisible.
    #[cfg(windows)]
    {
        let gateway = |gateway: Option<IpAddr>| gateway.filter(|ip| !ip.is_unspecified());
        existing.destination == requested.destination
            && existing.prefix == requested.prefix
            && existing.ifindex == requested.ifindex
            && gateway(existing.gateway) == gateway(requested.gateway)
            && requested
                .metric
                .is_none_or(|metric| existing.metric == Some(metric))
            && requested
                .luid
                .is_none_or(|luid| existing.luid == Some(luid))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn compares_windows_routes_with_system_defaults() {
        let requested = Route::new("10.0.0.0".parse().unwrap(), 8).with_ifindex(42);
        let mut existing = requested
            .clone()
            .with_gateway(Ipv4Addr::UNSPECIFIED.into())
            .with_metric(0)
            .with_luid(123);
        assert!(same_route(&existing, &requested));
        existing.ifindex = Some(43);
        assert!(!same_route(&existing, &requested));
        existing.ifindex = Some(42);
        existing.gateway = Some("192.168.1.1".parse().unwrap());
        assert!(!same_route(&existing, &requested));
    }
}

impl Drop for RerouteServer {
    fn drop(&mut self) {
        if !self.vpn_route_added {
            return;
        }

        let vpn_route = self.vpn_route.clone();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                if let Ok(handle) = Handle::new() {
                    let _ = handle.delete(&vpn_route).await;
                }
            });
        })
        .join()
        .ok();
    }
}
