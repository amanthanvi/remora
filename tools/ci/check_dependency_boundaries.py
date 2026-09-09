"""Fail when supported Cargo graphs cross the reviewed dependency boundaries."""

import json
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "shared/rust-bridge/Cargo.toml"
FIXTURE_MANIFEST = ROOT / "shared/third_party/codex/codex-rs/rmcp-client/Cargo.toml"
HOST_ROOTS = {"codex-mobile-client", "codex-debug-cli", "codex-tui"}
# Keep aligned with build-rust.sh and build-android-rust.sh, not installed SDKs.
MOBILE_TARGETS = (
    "aarch64-apple-ios",
    "aarch64-apple-ios-sim",
    "aarch64-apple-ios-macabi",
    "x86_64-apple-ios-macabi",
    "aarch64-linux-android",
    "x86_64-linux-android",
)
PACKAGE = re.compile(
    r"(?P<name>[A-Za-z0-9_-]+) v"
    r"(?P<major>0|[1-9][0-9]*)\.(?P<minor>0|[1-9][0-9]*)\."
    r"(?P<patch>0|[1-9][0-9]*)"
    r"(?P<prerelease>-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?"
    r"(?: \([^\n]+\))*"
)
FEATURES = re.compile(r"[A-Za-z0-9_+./?=-]+(?:,[A-Za-z0-9_+./?=-]+)*")


def check_graph(output, required_roots):
    packages = set()
    violations = set()
    for number, line in enumerate(output.splitlines(), 1):
        if not line.strip():
            continue
        package, separator, features = line.removesuffix(" (*)").partition("|")
        match = PACKAGE.fullmatch(package)
        if not separator or not match or (features and not FEATURES.fullmatch(features)):
            raise ValueError(f"Malformed cargo tree record on line {number}: {line!r}")
        name = match["name"]
        packages.add(name)
        enabled = set(features.split(",")) if features else set()
        if name == "rmcp" and enabled.intersection({
            "server-side-http", "transport-streamable-http-server",
            "transport-streamable-http-server-session",
        }):
            violations.add("rmcp HTTP server transport must remain test-only")
        if name == "codex-rmcp-client" and "http-test-server" in enabled:
            violations.add("codex-rmcp-client HTTP fixture must remain test-only")
        if name.startswith("hickory-") and any("dnssec" in feature.lower() for feature in enabled):
            violations.add(f"{name} DNSSEC must remain disabled")
        version = tuple(int(match[part]) for part in ("major", "minor", "patch"))
        if name == "rmcp" and (
            version < (1, 4, 0) or (version == (1, 4, 0) and match["prerelease"])
        ):
            violations.add(f"{package}: MCP must be at least 1.4.0")
        if name in {"hickory-proto", "hickory-net"} and (
            version < (0, 26, 1) or (version == (0, 26, 1) and match["prerelease"])
        ):
            violations.add(f"{package}: Hickory must be at least 0.26.1")
        if name in {"sqlx-mysql", "rsa"}:
            violations.add(f"{package}: MySQL and RSA must remain unselected")
        if name == "lru" and (version < (0, 18, 2) or (version == (0, 18, 2) and match["prerelease"])):
            violations.add(f"{package}: LRU must be at least 0.18.2")
    missing = required_roots - packages
    if missing or not packages - required_roots:
        raise ValueError(f"Empty or incomplete Cargo graph; required roots: {', '.join(sorted(required_roots))}")
    if violations:
        raise ValueError("; ".join(sorted(violations)))
    return len(packages)


def check_target(target):
    command = [
        "cargo", "tree", "--locked", "--manifest-path", str(MANIFEST),
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}|{f}",
        "--color", "never",
    ]
    if target is None:
        command += ["--workspace"]
        required_roots = HOST_ROOTS
    else:
        command += ["-p", "codex-mobile-client", "--target", target]
        required_roots = {"codex-mobile-client"}
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=True, timeout=180)
    return check_graph(result.stdout, required_roots)


def check_fixture_metadata(metadata):
    packages = metadata.get("packages", []) if isinstance(metadata, dict) else []
    if not isinstance(packages, list):
        raise ValueError("Malformed package list in Cargo metadata")
    clients = [package for package in packages if isinstance(package, dict) and package.get("name") == "codex-rmcp-client"]
    if len(clients) != 1 or not isinstance(clients[0].get("targets"), list) or not clients[0]["targets"]:
        raise ValueError("Missing codex-rmcp-client targets in Cargo metadata")
    fixture_names = {"test_streamable_http_server", "streamable_http_recovery", "streamable_http_remote"}
    for target in clients[0]["targets"]:
        if not isinstance(target, dict) or not isinstance(target.get("name"), str):
            raise ValueError("Malformed codex-rmcp-client target in Cargo metadata")
        if target["name"] in fixture_names:
            required = target.get("required-features", [])
            if not isinstance(required, list) or "http-test-server" not in required:
                raise ValueError(f"{target['name']} must require the http-test-server feature")


def check_fixture_targets():
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
         "--manifest-path", str(FIXTURE_MANIFEST)],
        cwd=ROOT, capture_output=True, text=True, check=True, timeout=180,
    )
    check_fixture_metadata(json.loads(result.stdout))


def main():
    label = "MCP fixture targets"
    try:
        check_fixture_targets()
        print("Dependency boundaries passed: MCP fixture targets")
        for target in (None, *MOBILE_TARGETS):
            label = target or "host workspace"
            count = check_target(target)
            print(f"Dependency boundaries passed: {label} ({count} package names)")
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Dependency boundary check failed ({label}): {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
