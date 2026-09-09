"""Exercise the iOS renderer's real cache lifecycle without compiling Ghostty."""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


class IOSGhosttyCacheTests(unittest.TestCase):
    def test_warm_cache_and_toolchain_invalidation(self):
        source = pathlib.Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            scripts = root / "apps/ios/scripts"
            scripts.mkdir(parents=True)
            script = scripts / "build-ghostty.sh"
            shutil.copyfile(source / "apps/ios/scripts/build-ghostty.sh", script)
            ghostty = root / "shared/third_party/ghostty"
            (ghostty / "include").mkdir(parents=True)
            (ghostty / "build.zig").write_text("emit-ios-static\n")
            (ghostty / "include/ghostty.h").write_text(
                "ghostty_surface_write external_pty_write GHOSTTY_PLATFORM_IOS\n"
            )
            (root / "tools/scripts").mkdir(parents=True)
            (root / "xcode/Platforms/iPhoneOS.platform").mkdir(parents=True)
            sdk = root / "clt/SDKs/MacOSX.sdk"
            (sdk / "usr/lib").mkdir(parents=True)
            (sdk / "usr/lib/libSystem.tbd").write_text("arm64-macos\n")
            (sdk / "SDKSettings.json").write_text('{"Version":"1"}\n')
            binaries = root / "bin"
            binaries.mkdir()

            def executable(path, body):
                path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body)
                path.chmod(0o755)

            executable(scripts / "sync-ghostty.sh", "exit 0\n")
            executable(root / "tools/scripts/resolve-zig.sh",
                       'printf "%s\\n" "$FIXTURE_ROOT/bin/zig"\n')
            executable(binaries / "git", 'printf "fixture\\n"\n')
            executable(binaries / "zig", '''
if [ "$1" = version ]; then printf '0.15.2\n'; exit 0; fi
while [ "$#" -gt 0 ]; do
    if [ "$1" = --prefix ]; then prefix="$2"; break; fi
    shift
done
mkdir -p "$prefix/lib" "$ZIG_LOCAL_CACHE_DIR" "$ZIG_GLOBAL_CACHE_DIR"
printf 'renderer fixture\n' > "$prefix/lib/ghostty-internal.a"
''')
            for tool in ("metal", "metallib"):
                executable(binaries / tool, "exit 0\n")
            executable(binaries / "env", '''
for arg in "$@"; do
    case "$arg" in
        /usr/bin/xcodebuild) printf 'Xcode fixture\n'; exit 0 ;;
        /usr/bin/xcrun)
            if [[ " $* " == *--show-sdk* && "${FIXTURE_FAIL_SDK_LOOKUP:-}" = 1 ]]; then exit 1; fi
            case " $* " in
                *--find*) printf '%s/bin/%s\n' "$FIXTURE_ROOT" "${!#}" ;;
                *--show-sdk-build-version*) printf 'sdk-fixture\n' ;;
                *) printf '%s/clt/SDKs/MacOSX.sdk\n' "$FIXTURE_ROOT" ;;
            esac
            exit 0
            ;;
    esac
done
exec /usr/bin/env "$@"
''')
            environment = dict(os.environ, FIXTURE_ROOT=str(root),
                               PATH=f"{binaries}:{os.environ['PATH']}",
                               GHOSTTY_XCODE_DEVELOPER_DIR=str(root / "xcode"),
                               GHOSTTY_CLT_DEVELOPER_DIR=str(root / "clt"))
            environment.pop("GHOSTTY_METAL_TOOLCHAIN_DIR", None)
            environment.pop("GHOSTTY_ZIG_CACHE_DIR", None)
            environment.pop("GHOSTTY_BUILD_DIR", None)

            def build():
                subprocess.run(["bash", str(script)], env=environment, check=True,
                               capture_output=True, text=True)

            cache = root / "apps/ios/GeneratedRust/ghostty-build/zig-cache"
            build()
            sentinel = cache / "local/warm-cache-proof"
            sentinel.write_text("keep me\n")
            build()
            self.assertTrue(sentinel.exists(), "unchanged builds must retain Zig cache")
            executable(binaries / "metal", "# toolchain update\nexit 0\n")
            build()
            self.assertFalse(sentinel.exists(), "changed Metal must invalidate cached shaders")
            sentinel.write_text("keep me\n")
            build()
            self.assertTrue(sentinel.exists())
            (sdk / "SDKSettings.json").write_text('{"Version":"2"}\n')
            build()
            self.assertFalse(sentinel.exists(), "changed CLT SDK must invalidate cached builds")
            sentinel.write_text("keep me\n")
            environment["FIXTURE_FAIL_SDK_LOOKUP"] = "1"
            with self.assertRaises(subprocess.CalledProcessError):
                build()
            self.assertTrue(sentinel.exists(), "failed tool discovery must not alter a valid cache")


if __name__ == "__main__":
    unittest.main()
