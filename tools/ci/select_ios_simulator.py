"""Select one available iPhone from `xcrun simctl list devices available -j`."""

import json
import sys


def select_iphone(inventory, preferred_name="iPhone 17 Pro"):
    iphones = [
        device
        for runtime, devices in inventory["devices"].items()
        if runtime.startswith("com.apple.CoreSimulator.SimRuntime.iOS-")
        for device in devices
        if device.get("isAvailable") and device["name"].startswith("iPhone")
    ]
    if not iphones:
        raise ValueError("No available iPhone simulator found")
    return next(
        (device["udid"] for device in iphones if device["name"] == preferred_name),
        iphones[0]["udid"],
    )


if __name__ == "__main__":
    try:
        print(select_iphone(json.load(sys.stdin)))
    except (ValueError, KeyError, TypeError) as error:
        sys.exit(str(error))
