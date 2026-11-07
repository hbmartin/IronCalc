"""The retry example catches workbook rejections, not unrelated Python failures."""
from pathlib import Path
import runpy

import ironcalc as ic
import pytest

EXAMPLE = Path(__file__).parents[1] / "docs" / "examples" / "collaboration.py"


def test_retry_example_handles_workbook_error():
    assert issubclass(ic.WorkbookError, Exception)
    runpy.run_path(str(EXAMPLE))


def test_retry_example_propagates_unrelated_error(monkeypatch):
    original = ic.UserModel
    calls = []

    class Proxy:
        def __init__(self, *args):
            self.inner = original(*args)

        def __getattr__(self, name):
            return getattr(self.inner, name)

        def apply_external_diffs(self, data):
            calls.append(data)
            if len(calls) == 3:
                raise ValueError("unrelated failure")
            self.inner.apply_external_diffs(data)

    monkeypatch.setattr(ic, "UserModel", Proxy)
    with pytest.raises(ValueError, match="unrelated failure"):
        runpy.run_path(str(EXAMPLE))
