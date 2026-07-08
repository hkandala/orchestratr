"""Self-test for the orchestratr Python SDK.

Skips cleanly when the orcr binary is not on PATH (or $ORCR_BIN). Uses a temp
ORCR_STORE so it never touches ~/.orcr or the user's default herdr session — the calls
exercised here (ps, show of a missing id) read sqlite only and never start a herdr
session.
"""

import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import orcr  # noqa: E402


def _orcr_available() -> bool:
    bin_ = os.environ.get("ORCR_BIN", "orcr")
    return shutil.which(bin_) is not None or os.path.isfile(bin_)


@unittest.skipUnless(_orcr_available(), "orcr binary not found")
class CliBackedTests(unittest.TestCase):
    def setUp(self):
        self._store = tempfile.mkdtemp(prefix="orcr-sdk-py-")
        self._saved = os.environ.get("ORCR_STORE")
        os.environ["ORCR_STORE"] = self._store

    def tearDown(self):
        if self._saved is None:
            os.environ.pop("ORCR_STORE", None)
        else:
            os.environ["ORCR_STORE"] = self._saved
        shutil.rmtree(self._store, ignore_errors=True)

    def test_ps_returns_empty_agent_list_from_fresh_store(self):
        self.assertEqual(orcr.ps(), [])

    def test_show_of_missing_id_raises_not_found(self):
        with self.assertRaises(orcr.NotFoundErr) as ctx:
            orcr.show("a999")
        self.assertEqual(ctx.exception.exit_code, 6)


class BinaryFreeTests(unittest.TestCase):
    def test_missing_binary_maps_to_env_config_err(self):
        saved = os.environ.get("ORCR_BIN")
        os.environ["ORCR_BIN"] = "/nonexistent/orcr-binary"
        try:
            with self.assertRaises(orcr.EnvConfigErr):
                orcr.ps()
        finally:
            if saved is None:
                os.environ.pop("ORCR_BIN", None)
            else:
                os.environ["ORCR_BIN"] = saved

    def test_exit_code_error_mapping(self):
        self.assertIsInstance(orcr._error_for(3, "m", "c"), orcr.TimeoutErr)
        self.assertIsInstance(orcr._error_for(4, "m", "c"), orcr.BlockedErr)
        self.assertIsInstance(orcr._error_for(6, "m", "c"), orcr.NotFoundErr)
        self.assertIsInstance(orcr._error_for(7, "m", "c"), orcr.StateConflictErr)
        self.assertIsInstance(orcr._error_for(1, "m", "c"), orcr.OrcrError)


if __name__ == "__main__":
    unittest.main()
