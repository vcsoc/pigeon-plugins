"""The vendored SDK copies are current (sdk/sync_sdk.py --check) and the sync is reproducible."""
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


class SyncTests(unittest.TestCase):
    def test_vendored_copies_are_current(self):
        result = subprocess.run([sys.executable, str(ROOT / "sdk/sync_sdk.py"), "--check"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_vendored_rust_crate_matches_source(self):
        src = (ROOT / "sdk/rust/pigeon-plugin-sdk/src/runtime.rs").read_bytes()
        vendored = (ROOT / "sdk/rust/examples/minimal-plugin/vendor/pigeon-plugin-sdk/src/runtime.rs").read_bytes()
        self.assertEqual(src, vendored)


if __name__ == "__main__":
    unittest.main()
