"""Run the live terminal gate against disposable OpenSSH, without personal credentials."""

import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]


def main():
    directory = tempfile.TemporaryDirectory(prefix="remora-ssh-")
    cidfile = Path(directory.name) / "container-id"
    try:
        subprocess.run([
            "docker", "run", "--detach", "--rm", "--cidfile", str(cidfile),
            "--publish", "127.0.0.1::22", "--env", "DEBIAN_FRONTEND=noninteractive",
            "debian:trixie", "sleep", "infinity",
        ], check=True, stdout=subprocess.DEVNULL, timeout=120)
        container = cidfile.read_text().strip()
        subprocess.run([
            "docker", "exec", container, "sh", "-ec",
            "apt-get update -qq && apt-get install -y -qq --no-install-recommends openssh-server "
            "&& useradd --create-home --shell /bin/sh remora-test && mkdir -p /run/sshd",
        ], check=True, timeout=240)
        password = secrets.token_urlsafe(32)
        subprocess.run([
            "docker", "exec", "-i", container, "chpasswd",
        ], input=f"remora-test:{password}\n", text=True, check=True, timeout=30)
        subprocess.run([
            "docker", "exec", "--detach", container, "/usr/sbin/sshd", "-D", "-e",
        ], check=True, timeout=30)
        ports = json.loads(subprocess.check_output([
            "docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container,
        ], text=True, timeout=30))
        binding, = ports["22/tcp"]
        if binding["HostIp"] != "127.0.0.1":
            raise RuntimeError("SSH fixture must be loopback-only")
        port = int(binding["HostPort"])
        deadline = time.monotonic() + 30
        while True:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1) as connection:
                    if connection.recv(256).startswith(b"SSH-2.0-"):
                        break
            except OSError:
                pass
            if time.monotonic() >= deadline:
                raise TimeoutError("Disposable OpenSSH did not become ready")
            time.sleep(0.2)
        environment = os.environ.copy()
        environment["REMORA_TERMINAL_LIVE_SSH"] = f"remora-test:{password}@127.0.0.1:{port}"
        result = subprocess.run([
            "cargo", "test", "--locked", "--manifest-path", "shared/rust-bridge/Cargo.toml",
            "-p", "codex-mobile-client", "--lib",
            "terminal::ssh::tests::live_remote_ssh_terminal_round_trips_shell_io",
            "--", "--exact", "--ignored", "--nocapture",
        ], cwd=ROOT, env=environment, capture_output=True, text=True, timeout=600)
        print(result.stdout, end="")
        print(result.stderr, end="")
        result.check_returncode()
        if "test result: ok. 1 passed;" not in result.stdout:
            raise RuntimeError("Live SSH gate did not execute exactly one passing test")
    finally:
        try:
            if cidfile.exists():
                subprocess.run([
                    "docker", "rm", "--force", cidfile.read_text().strip(),
                ], check=True, timeout=30)
        finally:
            directory.cleanup()


if __name__ == "__main__":
    main()
