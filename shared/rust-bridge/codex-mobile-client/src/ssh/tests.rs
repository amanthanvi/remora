use super::probes::format_process_logs;
use super::*;

fn rsa_policy_test_key() -> russh::keys::PrivateKey {
    use russh::keys::ssh_key::Mpint;
    use russh::keys::ssh_key::private::{RsaKeypair, RsaPrivateKey};
    use russh::keys::ssh_key::public::RsaPublicKey;

    // Textbook RSA components: a deterministic parser fixture, never a signing key.
    let integer = |value: u16| Mpint::from_positive_bytes(&value.to_be_bytes());
    let public = RsaPublicKey::new(integer(17), integer(3233)).unwrap();
    let private = RsaPrivateKey::new(integer(2753), integer(38), integer(61), integer(53)).unwrap();
    RsaKeypair::new(public, private).unwrap().into()
}

#[tokio::test]
async fn rsa_private_key_auth_is_rejected_before_host_verification() {
    use russh::keys::ssh_key::LineEnding;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let server = test_server::TestSshServer::start(test_server::test_host_key()).await;
    let host_checks = Arc::new(AtomicUsize::new(0));
    for line_ending in [LineEnding::LF, LineEnding::CRLF] {
        let credentials = SshCredentials {
            host: server.host.clone(),
            port: server.port,
            username: "rsa-policy-test".into(),
            auth: SshAuth::PrivateKey {
                key_pem: rsa_policy_test_key()
                    .to_openssh(line_ending)
                    .unwrap()
                    .to_string(),
                passphrase: None,
            },
            unlock_macos_keychain: false,
        };
        let host_checks = Arc::clone(&host_checks);
        let result = SshClient::connect(
            credentials,
            Box::new(move |_| {
                host_checks.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { true })
            }),
        )
        .await;
        let Err(SshError::AuthFailed(message)) = result else {
            panic!("RSA private key must fail preflight authentication policy");
        };
        assert_eq!(
            message,
            "RSA private keys are not supported; use an Ed25519 or ECDSA key instead"
        );
    }
    assert_eq!(host_checks.load(Ordering::SeqCst), 0);
    assert_eq!(server.auth_attempts(), 0);
}

#[tokio::test]
async fn ed25519_private_key_auth_still_succeeds() {
    use russh::keys::ssh_key::{LineEnding, private::Ed25519Keypair};

    let key: russh::keys::PrivateKey = Ed25519Keypair::from_seed(&[42; 32]).into();
    let server = test_server::TestSshServer::start(test_server::test_host_key()).await;
    let expected_fingerprint = server.fingerprint.clone();
    let client = SshClient::connect(
        SshCredentials {
            host: server.host.clone(),
            port: server.port,
            username: "ed25519-policy-test".into(),
            auth: SshAuth::PrivateKey {
                key_pem: key.to_openssh(LineEnding::LF).unwrap().to_string(),
                passphrase: None,
            },
            unlock_macos_keychain: false,
        },
        Box::new(move |fingerprint| {
            let matches = fingerprint == expected_fingerprint;
            Box::pin(async move { matches })
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("Ed25519 authentication must remain supported"));
    assert!(client.is_connected());
    assert!(server.auth_attempts() > 0);
    client.disconnect().await;
}

#[tokio::test]
async fn rsa_only_host_is_rejected_before_authentication() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let server = test_server::TestSshServer::start(rsa_policy_test_key()).await;
    let host_checks = Arc::new(AtomicUsize::new(0));
    let callback_checks = Arc::clone(&host_checks);
    let result = SshClient::connect(
        SshCredentials {
            host: server.host.clone(),
            port: server.port,
            username: "rsa-host-policy-test".into(),
            auth: SshAuth::Password("unused-test-password".into()),
            unlock_macos_keychain: false,
        },
        Box::new(move |_| {
            callback_checks.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { true })
        }),
    )
    .await;
    assert!(
        matches!(result, Err(SshError::ConnectionFailed(message)) if message.starts_with("No common Key algorithm")),
        "RSA-only hosts must fail key negotiation before verification or signing"
    );
    assert_eq!(host_checks.load(Ordering::SeqCst), 0);
    assert_eq!(server.auth_attempts(), 0);
}

#[test]
fn test_normalize_host_simple() {
    assert_eq!(normalize_host("example.com"), "example.com");
}

#[test]
fn test_normalize_host_trimming() {
    assert_eq!(normalize_host("  example.com  "), "example.com");
}

#[test]
fn test_normalize_host_ipv6_brackets() {
    assert_eq!(normalize_host("[::1]"), "::1");
}

#[test]
fn test_normalize_host_percent_encoding() {
    assert_eq!(normalize_host("fe80::1%25eth0"), "fe80::1%eth0");
}

#[test]
fn test_normalize_host_zone_id_removal() {
    // Non-IPv6 host with a zone id should have it stripped.
    assert_eq!(normalize_host("192.168.1.1%eth0"), "192.168.1.1");
}

#[test]
fn test_normalize_host_key_is_case_insensitive() {
    assert_eq!(normalize_host_key("  EXAMPLE.COM  "), "example.com");
}

#[test]
fn test_shell_quote_simple() {
    // Detailed contract lives in shell_quoting; this guards the re-export wiring.
    assert_eq!(shell_quote("hello"), "'hello'");
}

#[test]
fn test_server_launch_command_for_codex() {
    let command = server_launch_command(
        &RemoteCodexBinary::Codex("/usr/local/bin/codex".into()),
        "ws://127.0.0.1:8390",
        RemoteShell::Posix,
    );
    assert_eq!(
        command,
        "'/usr/local/bin/codex' --enable goals app-server --listen 'ws://127.0.0.1:8390'"
    );
}

#[test]
fn test_windows_start_process_spec_for_cmd_shim() {
    let (file_path, argument_list) = windows_start_process_spec(
        &RemoteCodexBinary::Codex(r#"C:\Users\me\AppData\Roaming\npm\codex.cmd"#.into()),
        "ws://127.0.0.1:8390",
    );
    assert_eq!(file_path, "$env:ComSpec");
    assert_eq!(
        argument_list,
        r#"@('/d', '/c', '""C:\Users\me\AppData\Roaming\npm\codex.cmd" --enable goals app-server --listen ws://127.0.0.1:8390"')"#
    );
}

#[test]
fn test_windows_start_process_spec_for_exe() {
    let (file_path, argument_list) = windows_start_process_spec(
        &RemoteCodexBinary::Codex(r#"C:\Program Files\Codex\codex.exe"#.into()),
        "ws://127.0.0.1:8390",
    );
    assert_eq!(file_path, r#"'C:\Program Files\Codex\codex.exe'"#);
    assert_eq!(
        argument_list,
        "@('--enable', 'goals', 'app-server', '--listen', 'ws://127.0.0.1:8390')"
    );
}

#[test]
fn test_format_process_logs_includes_stderr() {
    assert_eq!(
        format_process_logs("stdout line", "stderr line"),
        "stdout:\nstdout line\n\nstderr:\nstderr line"
    );
    assert_eq!(
        format_process_logs("", "stderr line"),
        "stderr:\nstderr line"
    );
}

#[test]
fn test_shell_quote_with_single_quote() {
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
}

#[test]
fn test_shell_quote_path() {
    assert_eq!(
        shell_quote("/home/user/my file.txt"),
        "'/home/user/my file.txt'"
    );
}

#[test]
fn test_build_posix_exec_command_uses_non_login_sh() {
    assert_eq!(
        build_posix_exec_command("echo 'hi' && printf '%s' \"$HOME\""),
        "/usr/bin/env sh -c 'echo '\\''hi'\\'' && printf '\\''%s'\\'' \"$HOME\"'"
    );
}

#[test]
fn test_exec_result_default() {
    let r = ExecResult {
        exit_code: 0,
        stdout: "hello\n".into(),
        stderr: String::new(),
    };
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout.trim(), "hello");
}

#[test]
fn test_ssh_error_display() {
    let e = SshError::ConnectionFailed("refused".into());
    assert_eq!(e.to_string(), "connection failed: refused");

    let e = SshError::HostKeyVerification {
        host: "host.example".into(),
        port: 22,
        fingerprint: "SHA256:abc".into(),
        pinned: None,
    };
    assert!(e.to_string().contains("SHA256:abc"));
    assert!(e.to_string().starts_with("unknown-host:host.example:22"));

    let e = SshError::HostKeyVerification {
        host: "host.example".into(),
        port: 22,
        fingerprint: "SHA256:new".into(),
        pinned: Some("SHA256:old".into()),
    };
    assert!(
        e.to_string()
            .starts_with("host-key-changed:host.example:22")
    );

    let e = SshError::HostKeyStoreUnavailable {
        host: "host.example".into(),
        port: 22,
        message: "keychain locked".into(),
    };
    assert!(
        e.to_string()
            .starts_with("host-key-store-unavailable:host.example:22")
    );
    assert!(e.to_string().contains("keychain locked"));

    let e = SshError::ExecFailed {
        exit_code: 127,
        stderr: "not found".into(),
    };
    assert!(e.to_string().contains("127"));
    assert!(e.to_string().contains("not found"));

    assert_eq!(SshError::Timeout.to_string(), "timeout");
    assert_eq!(SshError::Disconnected.to_string(), "disconnected");
}

#[test]
fn test_ssh_credentials_construction() {
    let creds = SshCredentials {
        host: "example.com".into(),
        port: 22,
        username: "user".into(),
        auth: SshAuth::Password("pass".into()),
        unlock_macos_keychain: false,
    };
    assert_eq!(creds.port, 22);
    assert_eq!(creds.username, "user");

    let creds_key = SshCredentials {
        host: "example.com".into(),
        port: 2222,
        username: "deploy".into(),
        auth: SshAuth::PrivateKey {
            key_pem: "-----BEGIN OPENSSH PRIVATE KEY-----\n...\n-----END OPENSSH PRIVATE KEY-----"
                .into(),
            passphrase: None,
        },
        unlock_macos_keychain: false,
    };
    assert_eq!(creds_key.port, 2222);
}

#[test]
fn test_bootstrap_result_clone() {
    let r = SshBootstrapResult {
        server_port: 8390,
        tunnel_local_port: 12345,
        server_version: Some("1.0.0".into()),
        pid: Some(42),
        codex_path: "/usr/local/bin/codex".into(),
        shell: RemoteShell::Posix,
        transport: SshBootstrapTransport::WebSocketTunnel,
    };
    let r2 = r.clone();
    assert_eq!(r2.server_port, 8390);
    assert_eq!(r2.tunnel_local_port, 12345);
    assert_eq!(r2.server_version.as_deref(), Some("1.0.0"));
    assert_eq!(r2.pid, Some(42));
}

#[test]
fn test_profile_init_sources_common_files() {
    // Verify the profile init string references the expected shell config files.
    assert!(PROFILE_INIT.contains(".profile"));
    assert!(PROFILE_INIT.contains(".bash_profile"));
    assert!(PROFILE_INIT.contains(".bashrc"));
    assert!(PROFILE_INIT.contains(".zshenv"));
    assert!(PROFILE_INIT.contains(".zprofile"));
    assert!(PROFILE_INIT.contains(".zshrc"));
    assert!(!PROFILE_INIT.contains("-ic 'printf %s \"$PATH\"'"));
}

#[test]
fn test_profile_init_adds_common_node_manager_bins() {
    assert!(PROFILE_INIT.contains("$NVM_BIN"));
    assert!(PROFILE_INIT.contains("ASDF_DATA_DIR"));
    assert!(PROFILE_INIT.contains("/opt/homebrew/opt/node/bin"));
    assert!(PROFILE_INIT.contains("/opt/homebrew/bin"));
    assert!(PROFILE_INIT.contains("/usr/local/opt/node/bin"));
    assert!(PROFILE_INIT.contains("/usr/local/bin"));
    assert!(PROFILE_INIT.contains("$HOME/.volta/bin"));
    assert!(PROFILE_INIT.contains("$HOME/.bun/bin"));
    assert!(PROFILE_INIT.contains("NVM_DIR"));
    assert!(PROFILE_INIT.contains(".nvm"));
    assert!(PROFILE_INIT.contains(".fnm/node-versions"));
    assert!(PROFILE_INIT.contains(".asdf/shims"));
    assert!(PROFILE_INIT.contains(".local/share/mise/shims"));
    assert!(PROFILE_INIT.contains("export PATH"));
}

#[test]
fn test_posix_resolver_preserves_path_precedence_without_package_manager_probes() {
    let script = resolve_codex_binary_script_posix();
    assert!(script.contains("packages/standalone/current/codex"));
    assert!(script.contains("${BUN_INSTALL:-$HOME/.bun}/bin/codex"));
    assert!(script.contains("PNPM_HOME"));
    assert!(script.contains("NVM_BIN"));
    assert!(script.contains("$HOME/.volta/bin/codex"));
    assert!(script.contains("$HOME/.local/bin/codex"));
    assert!(script.contains("Codex.app/Contents/Resources/codex"));
    assert!(script.contains("/opt/homebrew/bin/codex"));
    assert!(script.contains("/usr/local/bin/codex"));
    assert!(script.contains("/usr/bin/codex"));
    assert!(
        script.find("_remora_consider_path_candidates codex codex")
            < script.find("packages/standalone/current/codex")
    );
    assert!(script.contains("_remora_first_path"));
    assert!(!script.contains("npm config get prefix"));
    assert!(!script.contains("pnpm bin -g"));
    assert!(!script.contains("bun pm bin -g"));
    assert!(!script.contains("--version"));
    assert!(!script.contains("codex-app-server"));
}

#[test]
fn test_powershell_resolver_preserves_command_precedence_without_probes() {
    let script = resolve_codex_binary_script_powershell();
    assert!(script.contains("Get-Command codex"));
    assert!(script.contains("packages\\standalone\\current\\codex.exe"));
    assert!(script.contains("AppData\\Roaming\\npm\\codex.cmd"));
    assert!(!script.contains("Get-Command codex -All"));
    assert!(!script.contains("$bestVersion"));
    assert!(!script.contains("CompareTo"));
    assert!(!script.contains("--version"));
    assert!(!script.contains("npm "));
}

#[test]
fn test_default_remote_port() {
    assert_eq!(DEFAULT_REMOTE_PORT, 8390);
}

#[test]
fn test_port_candidates_range() {
    let ports: Vec<u16> = (0..PORT_CANDIDATES)
        .map(|i| DEFAULT_REMOTE_PORT + i)
        .collect();
    assert_eq!(ports.len(), 21);
    assert_eq!(*ports.first().unwrap(), 8390);
    assert_eq!(*ports.last().unwrap(), 8410);
}
