"""The native XcodeGen cache must track source additions and removals."""

import pathlib
import shutil
import subprocess
import tempfile
import unittest


@unittest.skipUnless(shutil.which("xcodegen"), "requires XcodeGen")
class XcodeGenCacheTests(unittest.TestCase):
    def test_source_inventory_and_missing_project(self):
        source = pathlib.Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            project = pathlib.Path(temporary) / "apps" / "ios"
            scripts = project / "scripts"
            scripts.mkdir(parents=True)
            script = scripts / "regenerate-project.sh"
            shutil.copyfile(source / "apps/ios/scripts/regenerate-project.sh", script)
            sources = project / "Sources"
            sources.mkdir()
            (sources / "Initial.swift").write_text("struct Initial {}\n")
            (project / "project.yml").write_text(
                "name: Remora\ntargets:\n  Remora:\n    type: framework\n"
                "    platform: iOS\n    sources: [Sources]\n"
            )

            def generate(*args):
                subprocess.run(["bash", str(script), *args], check=True,
                               capture_output=True, text=True)

            output = project / "Remora.xcodeproj/project.pbxproj"
            generate()
            stamp = output.stat().st_mtime_ns
            generate()
            self.assertEqual(stamp, output.stat().st_mtime_ns)
            added = sources / "Added.swift"
            added.write_text("struct Added {}\n")
            generate()
            self.assertIn("Added.swift", output.read_text())
            added.unlink()
            generate()
            self.assertNotIn("Added.swift", output.read_text())
            output.unlink()
            generate("--repair-only")
            self.assertIn("Initial.swift", output.read_text())


if __name__ == "__main__":
    unittest.main()
