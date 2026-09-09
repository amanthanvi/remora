import importlib.util
import json
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
from unittest.mock import Mock


SPEC = importlib.util.spec_from_file_location(
    "paired_relay_runner", Path(__file__).resolve().parents[1] / "scripts/verify-paired-relay.py"
)
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class PairedRelayRunnerTests(unittest.TestCase):
    def test_executable_requires_one_actual_test_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "compiled-test"
            path.touch()
            artifact = {"reason": "compiler-artifact", "target": {"name": "codex_mobile_client"},
                        "profile": {"test": True}, "executable": str(path)}
            self.assertEqual(RUNNER.test_executable(json.dumps(artifact)), str(path))
            for changes in ({"profile": {"test": False}}, {"target": {"name": "other"}},
                            {"executable": str(path) + "-missing"}):
                with self.subTest(changes=changes), self.assertRaises(RuntimeError):
                    RUNNER.test_executable(json.dumps(artifact | changes))
            duplicate = artifact | {"executable": str(path) + "-other"}
            with self.assertRaises(RuntimeError):
                RUNNER.test_executable(json.dumps(artifact) + "\n" + json.dumps(duplicate))
            with self.assertRaises((ValueError, RuntimeError)):
                RUNNER.test_executable("not JSON")

    def test_fixture_secrets_are_private_and_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "private" / "invitation"
            RUNNER.private_file(path, "synthetic-invitation")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                RUNNER.private_file(path, "replacement")
            self.assertEqual(path.read_text(), "synthetic-invitation")

    def test_stop_only_signals_the_recorded_live_child(self):
        exited = Mock()
        exited.poll.return_value = 0
        RUNNER.stop(exited)
        exited.send_signal.assert_not_called()
        live = Mock()
        live.poll.return_value = None
        live.wait.side_effect = [subprocess.TimeoutExpired("owned fixture", 15), 0]
        RUNNER.stop(live)
        live.send_signal.assert_called_once_with(signal.SIGINT)
        live.kill.assert_called_once_with()
        self.assertEqual(live.wait.call_count, 2)

    def test_termination_enters_normal_cleanup(self):
        with self.assertRaises(KeyboardInterrupt):
            RUNNER.interrupted(signal.SIGTERM, None)


if __name__ == "__main__":
    unittest.main()
