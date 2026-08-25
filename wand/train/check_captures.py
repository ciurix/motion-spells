#!/usr/bin/env python3
"""Report how many usable gestures each capture file holds.

Run this after recording, before training, so a bad session shows up while the
sensor is still on the desk rather than an hour later.

    python train/check_captures.py data
"""

import pathlib
import sys
import importlib.util

HERE = pathlib.Path(__file__).parent
spec = importlib.util.spec_from_file_location("train_spells", HERE / "train_spells.py")
ts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ts)

EXPECTED = ["left", "right", "up", "down", "push", "circular", "negative"]


def main():
    data_dir = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "data")
    files = sorted(data_dir.glob("*.txt"))
    if not files:
        sys.exit(f"No .txt files in {data_dir}/ - nothing recorded yet.")

    total = 0
    found = {}
    print(f"{'file':<16}{'gestures':>9}   status")
    print("-" * 46)
    for path in files:
        windows = ts.parse_capture_file(path)
        found[path.stem.lower()] = len(windows)
        total += len(windows)
        if len(windows) == 0:
            status = "EMPTY - nothing usable recorded"
        elif len(windows) < 20:
            status = "thin - aim for 40+"
        elif len(windows) < 40:
            status = "usable - more would help"
        else:
            status = "ok"
        print(f"{path.name:<16}{len(windows):>9}   {status}")

    print("-" * 46)
    print(f"{'total':<16}{total:>9}")

    missing = [g for g in EXPECTED if g not in found]
    if missing:
        print(f"\nNot recorded yet: {', '.join(missing)}")
    if "negative" not in found or found.get("negative", 0) < 10:
        print("\nnegative.txt is what stops the model firing spells at random -"
              " record plenty of idle motion.")


if __name__ == "__main__":
    main()
