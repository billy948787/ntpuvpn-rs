use std::fs;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn run(dir: &TempDir, values: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ntpuvpn-rs"))
        .env_clear()
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .envs(values.iter().copied())
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn assert_error(output: Output, expected: &str) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected), "unexpected error: {stderr}");
    assert!(output.stdout.is_empty(), "startup must not prompt");
}

#[test]
fn missing_username_fails_without_prompting_or_writing_config() {
    let dir = tempfile::tempdir().unwrap();
    assert_error(run(&dir, &[]), "No username available");
    assert!(!dir.path().join("config").exists());
}

#[test]
fn malformed_dotenv_does_not_print_secrets() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".env"),
        "NTPUVPN_PASSWORD='DO_NOT_LOG_THIS\n",
    )
    .unwrap();
    let output = run(&dir, &[]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("DO_NOT_LOG_THIS"));
    assert_error(output, "Invalid .env syntax");
}

#[test]
fn empty_process_password_overrides_dotenv_without_leaking_it() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".env"),
        "NTPUVPN_USERNAME=student\nNTPUVPN_PASSWORD='DO_NOT_LOG_THIS'\n",
    )
    .unwrap();
    let output = run(&dir, &[("NTPUVPN_PASSWORD", "")]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("DO_NOT_LOG_THIS"));
    assert_error(output, "Invalid or empty NTPUVPN_PASSWORD");
}

#[test]
fn invalid_netmask_fails_before_network_access() {
    let dir = tempfile::tempdir().unwrap();
    assert_error(
        run(
            &dir,
            &[
                ("NTPUVPN_USERNAME", "student"),
                ("NTPUVPN_PASSWORD", "secret"),
                ("NTPUVPN_VPN_MASK", "255.0.255.0"),
            ],
        ),
        "not contiguous",
    );
    assert!(!dir.path().join("config").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an isolated network namespace with no default route"]
fn dotenv_credentials_skip_prompts_and_do_not_change_saved_config() {
    // Confirm isolation before launching a CLI that could otherwise connect.
    assert_ne!(
        fs::read_link("/proc/self/ns/net")
            .unwrap()
            .to_string_lossy(),
        std::env::var("NTPUVPN_TEST_HOST_NETNS").expect("provide host netns ID before unshare")
    );
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config/ntpuvpn-rs");
    fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.json");
    let saved = r#"{"username":"saved-user","vpn_network":"10.0.0.0","vpn_mask":"255.0.255.0"}"#;
    fs::write(&config_path, saved).unwrap();
    fs::write(
        dir.path().join(".env"),
        "NTPUVPN_USERNAME=student\nNTPUVPN_PASSWORD='secret $ with # spaces'\nNTPUVPN_VPN_MASK=invalid\n",
    )
    .unwrap();
    // Process env repairs the invalid file mask; .env credentials override
    // saved settings and interactive prompts. Stop before actual VPN login.
    assert_error(
        run(&dir, &[("NTPUVPN_VPN_MASK", "255.0.0.0")]),
        "No default route was found",
    );
    assert_eq!(fs::read_to_string(&config_path).unwrap(), saved);
    fs::remove_file(&config_path).unwrap();
    assert_error(
        run(&dir, &[("NTPUVPN_VPN_MASK", "255.0.0.0")]),
        "No default route was found",
    );
    assert!(!config_path.exists());
}
