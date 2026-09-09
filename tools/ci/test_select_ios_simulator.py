import json
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).with_name("select_ios_simulator.py")


class SelectIosSimulatorTests(unittest.TestCase):
    def test_selection_emits_exactly_one_available_iphone_or_fails(self):
        fallback = {"name": "iPhone 16", "udid": "fallback", "isAvailable": True}
        preferred = {"name": "iPhone 17 Pro", "udid": "preferred", "isAvailable": True}
        cases = [
            ([fallback, preferred], "preferred\n"),
            ([preferred, fallback], "preferred\n"),
            ([fallback], "fallback\n"),
            ([dict(preferred, isAvailable=False), fallback], "fallback\n"),
            ([dict(preferred, name="iPhone 17 Pro Max", udid="pro-max"), preferred], "preferred\n"),
            ([], ""),
            ([dict(preferred, name="iPad Pro")], ""),
        ]
        for devices, expected in cases:
            with self.subTest(devices=devices):
                inventory = {"devices": {
                    "com.apple.CoreSimulator.SimRuntime.tvOS-26-0": [preferred],
                    "com.apple.CoreSimulator.SimRuntime.iOS-26-0": devices,
                }}
                result = subprocess.run(
                    [sys.executable, SCRIPT], input=json.dumps(inventory),
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.stdout, expected)
                self.assertEqual(result.returncode, 0 if expected else 1)
                if not expected:
                    self.assertIn("No available iPhone", result.stderr)

    def test_malformed_inventory_fails_without_a_destination(self):
        for payload in ("not json", "{}"):
            with self.subTest(payload=payload):
                result = subprocess.run(
                    [sys.executable, SCRIPT], input=payload,
                    capture_output=True, text=True, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
