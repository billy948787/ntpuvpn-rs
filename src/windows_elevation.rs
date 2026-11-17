use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_PIPE_BUSY, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, OpenProcessToken, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const ELEVATED_PIPE_ARGUMENT: &str = "--elevated-pipe";
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ENVIRONMENT_SIZE: usize = 1024 * 1024;
const ENVIRONMENT_NAMES: [&str; 6] = [
    "NTPUVPN_USERNAME",
    "NTPUVPN_PASSWORD",
    "NTPUVPN_VPN_NETWORK",
    "NTPUVPN_VPN_MASK",
    "NTPUVPN_OPENCONNECT",
    "APPDATA",
];

// Preserve Windows paths and passwords as UTF-16, including non-Unicode paths.
// Values travel only over a local pipe to the process launched by ShellExecute.
type Environment = Vec<Option<Vec<u16>>>;

/// Returns true when a replacement process has taken over in a new console.
pub async fn prepare() -> io::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|argument| argument == ELEVATED_PIPE_ARGUMENT)
    {
        if arguments.len() != 2 || !is_elevated()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid elevated startup",
            ));
        }
        let environment = tokio::time::timeout(HANDOFF_TIMEOUT, receive_environment(&arguments[1]))
            .await
            .map_err(|_| handoff_timeout())??;
        for (name, value) in ENVIRONMENT_NAMES.iter().zip(environment) {
            // SAFETY: setting process environment variables is thread-safe on
            // Windows. This module is compiled only for Windows.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, OsString::from_wide(&value)),
                    None => std::env::remove_var(name),
                }
            }
        }
        return Ok(false);
    }

    if is_elevated()? || !io::stdin().is_terminal() {
        // Scheduled/headless runs report a permission error after validating
        // credentials, without opening an interactive UAC dialog.
        return Ok(false);
    }

    let environment: Environment = ENVIRONMENT_NAMES
        .iter()
        .map(|name| std::env::var_os(name).map(|value| value.encode_wide().collect()))
        .collect();
    let payload = serde_json::to_vec(&environment).map_err(io::Error::other)?;
    if payload.len() > MAX_ENVIRONMENT_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VPN environment is too large",
        ));
    }

    let identifier = tempfile::Builder::new().prefix("ntpuvpn-uac-").tempdir()?;
    let pipe_name = format!(
        r"\\.\pipe\{}-{}",
        identifier.path().file_name().unwrap().to_string_lossy(),
        std::process::id()
    );
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .access_outbound(true)
        .reject_remote_clients(true)
        .create(&pipe_name)?;
    let child = launch_elevated(&pipe_name)?;
    // SAFETY: the ShellExecute process handle remains owned by child.
    let child_pid = unsafe { GetProcessId(child.as_raw_handle()) };
    if child_pid == 0 {
        return Err(io::Error::last_os_error());
    }

    tokio::time::timeout(HANDOFF_TIMEOUT, async {
        loop {
            tokio::select! {
                result = server.connect() => result?,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    // SAFETY: child owns a live process handle; zero does not block.
                    if unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
                        return Err(io::Error::other("Elevated VPN process exited during startup"));
                    }
                    continue;
                },
            }
            let mut client_pid = 0;
            // SAFETY: server owns this connected named pipe and the output is valid.
            if unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &mut client_pid) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if client_pid == child_pid {
                break;
            }
            // Never disclose the environment to another local process.
            server.disconnect()?;
        }
        send_environment(&mut server, &payload).await
    })
    .await
    .map_err(|_| handoff_timeout())??;
    println!("Continue in the new Administrator window. Press Ctrl+C there to disconnect.");
    Ok(true)
}

pub fn require_elevated() -> io::Result<()> {
    if is_elevated()? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Administrator privileges are required to create the VPN adapter. Run interactively to approve UAC, or run the scheduled task as Administrator",
    ))
}

fn is_elevated() -> io::Result<bool> {
    let mut token = std::ptr::null_mut();
    // SAFETY: current process is valid and token points to a writable handle slot.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a new handle owned by this function.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0;
    // SAFETY: the handle, output buffer, buffer size and returned length are valid.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn launch_elevated(pipe_name: &str) -> io::Result<OwnedHandle> {
    let executable = wide(std::env::current_exe()?.as_os_str());
    let directory = wide(std::env::current_dir()?.as_os_str());
    // The generated pipe name contains no spaces or quotes. Credentials never
    // appear in these arguments, and lpDirectory preserves .env lookup.
    let parameters = wide(OsStr::new(&format!("{ELEVATED_PIPE_ARGUMENT} {pipe_name}")));
    let verb = wide(OsStr::new("runas"));
    // SAFETY: initialize COM on this thread before calling the Windows shell.
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 {
        return Err(io::Error::other(format!(
            "Failed to initialize UAC launcher: {initialized:#x}"
        )));
    }
    // SAFETY: all-zero is a valid initial state for this C structure.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = executable.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.lpDirectory = directory.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: structure and its UTF-16 strings stay live throughout the call.
    let launched = unsafe { ShellExecuteExW(&mut info) };
    let error = (launched == 0).then(io::Error::last_os_error);
    // SAFETY: balance the successful CoInitializeEx call on the same thread.
    unsafe {
        CoUninitialize();
    }
    if let Some(error) = error {
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "UAC was cancelled; VPN was not started",
            )
        } else {
            error
        });
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other(
            "UAC did not return an elevated process handle",
        ));
    }
    // SAFETY: SEE_MASK_NOCLOSEPROCESS transfers ownership of this handle to us.
    Ok(unsafe { OwnedHandle::from_raw_handle(info.hProcess) })
}

async fn send_environment(server: &mut NamedPipeServer, payload: &[u8]) -> io::Result<()> {
    server.write_u32_le(payload.len() as u32).await?;
    server.write_all(payload).await?;
    if server.read_u8().await? != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Elevated VPN process did not acknowledge startup",
        ));
    }
    Ok(())
}

async fn receive_environment(pipe_name: &OsStr) -> io::Result<Environment> {
    let mut client: NamedPipeClient = loop {
        match ClientOptions::new().read(true).write(true).open(pipe_name) {
            Ok(client) => break client,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let length = client.read_u32_le().await? as usize;
    if length > MAX_ENVIRONMENT_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VPN environment is too large",
        ));
    }
    let mut payload = vec![0; length];
    client.read_exact(&mut payload).await?;
    // Avoid including JSON parser diagnostics, which can contain secret values.
    let environment: Environment = serde_json::from_slice(&payload).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid VPN environment handoff",
        )
    })?;
    if environment.len() != ENVIRONMENT_NAMES.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Incomplete VPN environment handoff",
        ));
    }
    client.write_u8(1).await?;
    Ok(environment)
}

fn handoff_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "Elevated VPN process did not complete startup within 15 seconds",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn elevation_pipe_preserves_environment_without_shell_quoting() {
        let temp = tempfile::tempdir().unwrap();
        let name = format!(
            r"\\.\pipe\ntpuvpn-uac-test-{}",
            temp.path().file_name().unwrap().to_string_lossy()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .unwrap();
        let expected: Environment = [
            Some("s123456789"),
            Some("密碼 $ '#\\\""),
            None,
            Some("255.0.0.0"),
            Some("C:\\path with spaces\\openconnect.exe"),
            None,
        ]
        .into_iter()
        .map(|value| value.map(|value| OsStr::new(value).encode_wide().collect()))
        .collect();
        let payload = serde_json::to_vec(&expected).unwrap();
        let receiver = tokio::spawn(async move { receive_environment(OsStr::new(&name)).await });
        server.connect().await.unwrap();
        send_environment(&mut server, &payload).await.unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), expected);
    }

    #[test]
    fn can_query_current_process_elevation() {
        is_elevated().unwrap();
    }
}
