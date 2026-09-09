"""Verify real Iroh pairing, detached Codex repair and relay ACK using owned fixtures.

Requires built remora-link/remora-relay binaries and codex on PATH (or an
explicit REMORA_CODEX_BINARY when a shell launcher depends on the real HOME). Uses only
loopback model/provider fixtures, a disposable host identity and software device
keys in the Rust test. No production push delivery or hardware custody claim.
"""

import http.server
import json
import os
from pathlib import Path
import platform
import runpy
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request


ROOT = Path(__file__).resolve().parents[2]
TEST = "real_paired_host_relay_repairs_detached_turn_before_ack"


def test_executable(build_output):
    artifacts = [json.loads(line) for line in build_output.splitlines() if line.strip()]
    executables = {
        entry["executable"] for entry in artifacts
        if entry.get("reason") == "compiler-artifact"
        and entry.get("target", {}).get("name") == "codex_mobile_client"
        and entry.get("profile", {}).get("test") is True
        and entry.get("executable")
    }
    if len(executables) != 1:
        raise RuntimeError("Expected exactly one compiled mobile-client test executable")
    executable = Path(executables.pop())
    if not executable.is_file():
        raise RuntimeError("Compiled mobile-client test executable is missing")
    return str(executable)


def private_file(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(path, "x", opener=lambda name, flags: os.open(name, flags, 0o600)) as output:
        output.write(value)


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def stop(child):
    if child.poll() is None:
        child.send_signal(signal.SIGINT)
        try:
            child.wait(timeout=15)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


def interrupted(_signal, _frame):
    raise KeyboardInterrupt


def main():
    signal.signal(signal.SIGTERM, interrupted)
    codex = os.environ.get("REMORA_CODEX_BINARY") or shutil.which("codex")
    host = ROOT / "services/remora-link/target/debug/remora-link"
    relay = ROOT / "services/remora-relay/target/release/remora-relay"
    if not codex or not host.is_file() or not relay.is_file():
        raise SystemExit("Build remora-link and remora-relay, and put codex on PATH first")
    build = subprocess.run(["cargo", "test", "--locked", "--offline", "--manifest-path",
                            str(ROOT / "shared/rust-bridge/Cargo.toml"), "-p", "codex-mobile-client",
                            "--lib", "--no-run", "--message-format=json"],
                           cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True, timeout=600)
    executable = test_executable(build.stdout)
    fixture = runpy.run_path(str(ROOT / "apps/android/scripts/verify-remote-resume.py"))
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), fixture["Provider"])
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    children = []
    with tempfile.TemporaryDirectory(prefix="remora-paired-relay-") as temporary:
        directory = Path(temporary)
        home = directory / "home"
        home.mkdir(mode=0o700)
        codex_home = directory / "codex"
        app_port, relay_port = free_port(), free_port()
        private_file(codex_home / "config.toml", f'''model = "fixture-model"
model_provider = "local_fixture"
approval_policy = "never"
sandbox_mode = "read-only"
[model_providers.local_fixture]
name = "Local verification fixture"
base_url = "http://127.0.0.1:{provider.server_port}"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''')
        private_file(directory / "bootstrap", secrets.token_urlsafe(32))
        private_file(directory / "relay.toml", f'''deployment_profile = "local_development"
[server]
bind = "127.0.0.1:{relay_port}"
[database]
kind = "local_sqlite"
path = {json.dumps(str(directory / "relay.sqlite"))}
[security]
token_key_path = {json.dumps(str(directory / "relay-token.key"))}
allow_unauthenticated_bootstrap_on_loopback = true
[push]
mode = "mock"
''')
        environment = {key: value for key, value in os.environ.items()
                       if not any(secret in key.upper() for secret in ["TOKEN", "API_KEY", "AUTH"])}
        environment.update(HOME=str(home), CODEX_HOME=str(codex_home),
                           XDG_CONFIG_HOME=str(home / "config"), XDG_STATE_HOME=str(home / "state"))
        if platform.system() == "Darwin":
            host_config = home / "Library/Application Support/com.remora.remora-link/host.toml"
        elif platform.system() == "Linux":
            host_config = home / "config/remora-link/host.toml"
        else:
            raise SystemExit("This owned-process integration fixture currently runs on macOS/Linux")
        private_file(host_config, f'''[agents.codex]
enabled = true
bin = {json.dumps(codex)}
host = "127.0.0.1"
port = {app_port}
[background_relay]
origin = "http://127.0.0.1:{relay_port}"
bootstrap_token_file = {json.dumps(str(directory / "bootstrap"))}
allow_loopback_http = true
''')
        try:
            for name, command in [
                ("relay", [str(relay), "serve", "--config", str(directory / "relay.toml")]),
                ("codex", [codex, "app-server", "--listen", f"ws://127.0.0.1:{app_port}"]),
                ("host", [str(host), "serve"]),
            ]:
                with (directory / f"{name}.log").open("w") as log:
                    children.append(subprocess.Popen(command, cwd=directory, env=environment,
                                                     stdout=log, stderr=subprocess.STDOUT))
            deadline = time.monotonic() + 30
            while True:
                if any(child.poll() is not None for child in children):
                    raise RuntimeError("owned fixture process exited before readiness")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{relay_port}/health/ready", timeout=1) as response:
                        ready = response.status == 200
                    status = subprocess.run([str(host), "status"], env=environment, capture_output=True, timeout=3)
                    with socket.create_connection(("127.0.0.1", app_port), timeout=1):
                        pass
                    if ready and status.returncode == 0:
                        break
                except (OSError, subprocess.TimeoutExpired):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError("fixture readiness")
                time.sleep(0.2)
            pairing = subprocess.run([str(host), "pair", "--runtime", "codex", "--unattended",
                                      "--i-understand-first-claimer-wins"], env=environment,
                                     capture_output=True, text=True, check=True, timeout=10)
            lines = [line.strip() for line in pairing.stdout.splitlines() if line.strip().startswith("remora-link:")]
            if len(lines) != 1:
                raise RuntimeError("expected one private pairing code, output withheld")
            code_file = directory / "invitation"
            private_file(code_file, lines[0])
            test_environment = os.environ.copy()
            test_environment.update(REMORA_RELAY_LIVE_CODE_FILE=str(code_file), REMORA_RELAY_LIVE_CWD=str(directory))
            # Execute the compiled artifact directly: another build cannot consume
            # the short-lived invitation while this test waits for Cargo's lock.
            result = subprocess.run([executable, TEST, "--ignored", "--nocapture"],
                                    cwd=ROOT, env=test_environment, text=True, capture_output=True, timeout=240)
            evidence = Path(tempfile.gettempdir()) / "remora-paired-relay-live-result.log"
            evidence.write_text(result.stdout + result.stderr)
            if result.returncode or "1 passed; 0 failed" not in result.stdout:
                raise RuntimeError(f"paired relay integration failed; see {evidence}")
            print(f"PASS: real pairing, detached turn repair, durable barrier, ACK, and cleanup; {evidence}")
        except BaseException:
            for name in ("relay", "codex", "host"):
                source = directory / f"{name}.log"
                if source.exists():
                    shutil.copyfile(source, Path(tempfile.gettempdir()) / f"remora-paired-relay-{name}.log")
            for index, source in enumerate(home.rglob("daemon.log*")):
                shutil.copyfile(source, Path(tempfile.gettempdir()) / f"remora-paired-relay-daemon-{index}.log")
            raise
        finally:
            for child in reversed(children):
                stop(child)
            provider.shutdown()
            provider.server_close()


if __name__ == "__main__":
    main()
