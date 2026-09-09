"""Exercise Android resume through disposable SSH and a real local Codex app-server.

Requires Docker, codex, adb, a running emulator, and built app/test APKs.
The model provider is a deterministic local SSE fixture, not hosted inference.
Use --serve to expose the same fixture for an iOS journey for up to ten minutes.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import secrets
import shutil
import shlex
import signal
import socket
import socketserver
import select
import subprocess
import tempfile
import threading
import time


ROOT = Path(__file__).resolve().parents[3]


class Transport(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def __init__(self, upstream_port):
        super().__init__(("127.0.0.1", 0), Forwarder)
        self.upstream_port = upstream_port
        self.gate = threading.Lock()
        self.available = True
        self.connections = set()

    def set_available(self, available):
        with self.gate:
            self.available = available
            if not available:
                for connection in self.connections:
                    try:
                        connection.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass


class Forwarder(socketserver.BaseRequestHandler):
    def handle(self):
        with socket.create_connection(("127.0.0.1", self.server.upstream_port)) as upstream:
            connections = {self.request, upstream}
            with self.server.gate:
                if not self.server.available:
                    return
                self.server.connections.update(connections)
            try:
                while True:
                    ready, _, _ = select.select(list(connections), [], [], 30)
                    for source in ready:
                        data = source.recv(65536)
                        if not data:
                            return
                        (upstream if source is self.request else self.request).sendall(data)
            except OSError:
                pass
            finally:
                with self.server.gate:
                    self.server.connections.difference_update(connections)


class Provider(http.server.BaseHTTPRequestHandler):
    calls = 0
    calls_lock = threading.Lock()

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path in ("/disconnect", "/reconnect"):
            self.server.transport.set_available(self.path == "/reconnect")
            print("TRANSPORT", self.path.removeprefix("/"), flush=True)
            self.send_response(200)
            self.end_headers()
            return
        if self.path != "/responses":
            self.send_error(404)
            return
        with Provider.calls_lock:
            Provider.calls += 1
            request_number = Provider.calls
        response_id = f"resume-response-{request_number}"
        print("MODEL_REQUEST", request_number, flush=True)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.event({"type": "response.created", "response": {"id": response_id}})
        time.sleep(10)
        self.event({"type": "response.output_item.done", "item": {
            "type": "message", "role": "assistant", "id": f"resume-message-{request_number}",
            "content": [{"type": "output_text", "text": "REMOTE_RESUME_COMPLETE"}],
        }})
        self.event({"type": "response.completed", "response": {
            "id": response_id, "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
        }})
        print("MODEL_COMPLETED", flush=True)

    def event(self, value):
        self.wfile.write(("data: " + json.dumps(value) + "\n\n").encode())
        self.wfile.flush()

    def log_message(self, format, *args):
        pass


def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=kwargs.pop("timeout", 60), **kwargs)


def terminate(_signal, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--serial", default="emulator-5554")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, terminate)
    codex = shutil.which("codex")
    adb = shutil.which("adb")
    if not codex or (not args.serve and not adb):
        parser.error("codex and (unless --serve) adb must be on PATH")
    directory = Path(tempfile.mkdtemp(prefix="remora-native-remote-"))
    print("ARTIFACTS", directory, flush=True)
    print("FIXTURE_PID", os.getpid(), flush=True)
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        server_port = reservation.getsockname()[1]
    transport = Transport(server_port)
    provider.transport = transport
    threading.Thread(target=transport.serve_forever, daemon=True).start()
    home = directory / "codex-home"
    home.mkdir()
    (home / "config.toml").write_text(f'''model = "gpt-5.4"
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
    environment = {key: value for key, value in os.environ.items()
                   if not any(secret in key for secret in ["TOKEN", "API_KEY", "AUTH"])}
    environment["CODEX_HOME"] = str(home)
    cidfile = directory / "container-id"
    details = directory / "connection.json"
    server = None
    try:
        with (directory / "app-server.log").open("w") as server_log:
            server = subprocess.Popen(
                [codex, "app-server", "--listen", f"ws://127.0.0.1:{server_port}"],
                cwd=directory, env=environment, stdout=server_log, stderr=subprocess.STDOUT,
            )
        run(["docker", "run", "--detach", "--rm", "--cidfile", str(cidfile),
             "--publish", "127.0.0.1::22", "--env", "DEBIAN_FRONTEND=noninteractive",
             "debian:trixie", "sleep", "infinity"], stdout=subprocess.DEVNULL, timeout=120)
        container = cidfile.read_text().strip()
        run(["docker", "exec", container, "sh", "-ec",
             "apt-get update -qq && apt-get install -y -qq --no-install-recommends "
             "openssh-server socat iproute2 && useradd --create-home --shell /bin/sh remora-test "
             "&& mkdir -p /run/sshd"], timeout=240, stdout=subprocess.DEVNULL)
        password = secrets.token_urlsafe(32)
        run(["docker", "exec", "-i", container, "chpasswd"],
            input=f"remora-test:{password}\n", text=True)
        # Bootstrap may choose another port after an outage; each launch forwards to the same real server.
        wrapper = directory / "codex"
        version = run([codex, "--version"], capture_output=True, text=True, env=environment).stdout.strip()
        wrapper.write_text(f'''#!/bin/sh
case "$1" in --version) echo {shlex.quote(version)}; exit 0;; esac
while [ "$#" -gt 1 ]; do
    if [ "$1" = --listen ]; then
        port="${{2##*:}}"
        case "$port" in ''|*[!0-9]*) exit 1;; esac
        exec socat "TCP-LISTEN:$port,bind=127.0.0.1,fork,reuseaddr" "TCP:host.docker.internal:{transport.server_address[1]}"
    fi
    shift
done
exit 1
''')
        wrapper.chmod(0o755)
        run(["docker", "cp", str(wrapper), f"{container}:/usr/local/bin/codex"])
        run(["docker", "exec", "--detach", container, "socat",
             "TCP-LISTEN:8390,bind=127.0.0.1,fork,reuseaddr", f"TCP:host.docker.internal:{transport.server_address[1]}"])
        run(["docker", "exec", "--detach", container, "/usr/sbin/sshd", "-D", "-e"])
        ports = json.loads(subprocess.check_output(
            ["docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container], text=True, timeout=30))
        binding, = ports["22/tcp"]
        assert binding["HostIp"] == "127.0.0.1"
        with open(details, "x", opener=lambda path, flags: os.open(path, flags, 0o600)) as output:
            json.dump({"port": int(binding["HostPort"]), "username": "remora-test",
                       "password": password, "host": "127.0.0.1", "controlPort": provider.server_port}, output)
        print("FIXTURE_READY", details, flush=True)
        if args.serve:
            time.sleep(600)
            return
        run([adb, "-s", args.serial, "install", "-r", str(ROOT / "apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk")])
        try:
            result = subprocess.run(
                [adb, "-s", args.serial, "shell", "am", "instrument", "-w", "-r", "-e", "class",
                 "com.remora.android.state.RemoteTurnResumeJourneyTest", "-e", "remoteSshPort", binding["HostPort"],
                 "-e", "remoteControlPort", str(provider.server_port),
                 "-e", "remoteSshPassword", password, "com.remora.android.test/androidx.test.runner.AndroidJUnitRunner"],
                capture_output=True, text=True, timeout=180,
            )
        except subprocess.TimeoutExpired:
            raise RuntimeError("Remote instrumentation timed out") from None
        print(result.stdout, result.stderr, flush=True)
        (directory / "instrumentation.log").write_text(result.stdout + result.stderr)
        screenshot = subprocess.run(
            [adb, "-s", args.serial, "exec-out", "run-as", "com.remora.android", "cat",
             "cache/remote-resume-journey.png"], capture_output=True, timeout=30,
        )
        if screenshot.returncode == 0:
            (directory / "resumed-conversation.png").write_bytes(screenshot.stdout)
        accessibility = subprocess.run(
            [adb, "-s", args.serial, "exec-out", "run-as", "com.remora.android", "cat",
             "cache/remote-resume-accessibility.txt"], capture_output=True, timeout=30,
        )
        if accessibility.returncode == 0:
            (directory / "resumed-accessibility.txt").write_bytes(accessibility.stdout)
        if result.returncode != 0:
            raise RuntimeError(f"Remote instrumentation exited {result.returncode}")
        assert "OK (1 test)" in result.stdout, "Instrumentation did not pass exactly one test"
        assert Provider.calls == 1, f"Expected one model request, got {Provider.calls}"
        assert screenshot.returncode == 0, "Instrumentation did not capture the resumed conversation"
    finally:
        try:
            if cidfile.exists():
                run(["docker", "rm", "--force", cidfile.read_text().strip()])
        finally:
            details.unlink(missing_ok=True)
            if server is not None:
                server.terminate()
                try:
                    server.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=30)
            provider.shutdown()
            provider.server_close()
            transport.set_available(False)
            transport.shutdown()
            transport.server_close()
            if adb and not args.serve:
                run([adb, "-s", args.serial, "shell", "am", "start", "-W", "-n",
                     "com.remora.android/com.remora.android.MainActivity"])
            print("CLEANED_OWNED_RESOURCES", flush=True)


if __name__ == "__main__":
    main()
