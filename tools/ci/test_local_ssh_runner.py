import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/verify-local-ssh.py"
SPEC = importlib.util.spec_from_file_location("verify_local_ssh", SCRIPT)
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class LocalSshRunnerTests(unittest.TestCase):
    def test_startup_timeout_cleans_only_the_recorded_container(self):
        for created in (False, True):
            with self.subTest(created=created):
                calls = []
                cidfiles = []

                def run(command, **kwargs):
                    calls.append(command)
                    if command[:2] == ["docker", "run"]:
                        cidfile = Path(command[command.index("--cidfile") + 1])
                        cidfiles.append(cidfile)
                        if created:
                            cidfile.write_text("a" * 64)
                        raise subprocess.TimeoutExpired(command, 120)
                    self.assertEqual(command, ["docker", "rm", "--force", "a" * 64])

                with patch.object(runner.subprocess, "run", side_effect=run):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        runner.main()
                self.assertEqual(len(calls), 2 if created else 1)
                self.assertFalse(cidfiles[0].parent.exists())


if __name__ == "__main__":
    unittest.main()
