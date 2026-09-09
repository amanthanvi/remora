"""Android builds resolve rust-objcopy's LLVM without changing the toolchain."""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


@unittest.skipUnless(shutil.which("cc"), "requires a native C compiler for the tool stub")
class AndroidRustBuildTests(unittest.TestCase):
    def run_build(self, platform, llvm_present, existing_path):
        source = pathlib.Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="remora android build ") as temporary:
            root = pathlib.Path(temporary)
            script = root / "tools/scripts/build-android-rust.sh"
            script.parent.mkdir(parents=True)
            shutil.copyfile(source / "tools/scripts/build-android-rust.sh", script)
            (root / "shared/rust-bridge").mkdir(parents=True)
            ghostty = root / "apps/android/core/bridge/src/main/jniLibs/arm64-v8a/libghostty.so"
            ghostty.parent.mkdir(parents=True)
            ghostty.touch()
            sync = root / "apps/ios/scripts/sync-codex.sh"
            sync.parent.mkdir(parents=True)
            sync.write_text("#!/bin/sh\nexit 0\n")
            sync.chmod(0o755)
            sysroot = root / "toolchain"
            (sysroot / "lib").mkdir(parents=True)
            if llvm_present:
                (sysroot / "lib/libLLVM.dylib").touch()
            tools = root / "bin"
            tools.mkdir()
            for name, body in {
                "cargo-ndk": "exit 0",
                "rustup": "exit 0",
                "rustc": 'printf "%s\\n" "$TEST_SYSROOT"',
                "uname": 'printf "%s\\n" "$TEST_PLATFORM"',
            }.items():
                tool = tools / name
                tool.write_text(f"#!/bin/sh\n{body}\n")
                tool.chmod(0o755)

            # A native stub preserves DYLD variables like Cargo does. A shell
            # interpreter would discard them under macOS SIP before recording.
            capture_source = root / "capture.c"
            capture_source.write_text(
                '#include <stdio.h>\n#include <stdlib.h>\n'
                'int main(void) {\n'
                '    FILE *output = fopen(getenv("TEST_CAPTURE"), "w");\n'
                '    if (!output) return 1;\n'
                '    const char *value = getenv("DYLD_LIBRARY_PATH");\n'
                '    fputs(value ? value : "", output);\n'
                '    return fclose(output);\n'
                '}\n'
            )
            subprocess.run(
                [shutil.which("cc"), str(capture_source), "-o", str(tools / "cargo")],
                check=True, capture_output=True, text=True,
            )
            capture = root / "lookup.txt"
            environment = {
                **os.environ,
                "PATH": f"{tools}{os.pathsep}/usr/bin{os.pathsep}/bin",
                "ANDROID_NDK_HOME": str(root / "ndk"),
                "ANDROID_ABIS": "arm64-v8a",
                "CARGO_INCREMENTAL": "1",
                "TEST_SYSROOT": str(sysroot),
                "TEST_PLATFORM": platform,
                "TEST_CAPTURE": str(capture),
                "TEST_EXISTING_PATH": existing_path,
            }
            subprocess.run(
                ["bash", "-c", 'export DYLD_LIBRARY_PATH="$TEST_EXISTING_PATH"; source "$0"',
                 str(script)],
                env=environment, check=True, capture_output=True, text=True,
            )
            return capture.read_text(), str(sysroot / "lib")

    def test_darwin_prepends_selected_sysroot_and_preserves_existing_lookup(self):
        actual, library = self.run_build("Darwin", True, "/existing/lib")
        self.assertEqual(actual, f"{library}:/existing/lib")

    def test_darwin_empty_lookup_does_not_add_current_directory(self):
        actual, library = self.run_build("Darwin", True, "")
        self.assertEqual(actual, library)

    def test_darwin_without_llvm_preserves_existing_lookup(self):
        actual, _ = self.run_build("Darwin", False, "/existing/lib")
        self.assertEqual(actual, "/existing/lib")

    def test_other_platforms_preserve_existing_lookup(self):
        actual, _ = self.run_build("Linux", True, "/existing/lib")
        self.assertEqual(actual, "/existing/lib")


if __name__ == "__main__":
    unittest.main()
