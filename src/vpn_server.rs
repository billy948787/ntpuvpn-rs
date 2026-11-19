#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::VpnSession;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::VpnSession;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("ntpuvpn-rs supports Windows, macOS and Linux");
