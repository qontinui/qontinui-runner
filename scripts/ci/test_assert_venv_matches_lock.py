"""Cases for assert-venv-matches-lock.py; each must fail against a naive check."""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "avml", Path(__file__).with_name("assert-venv-matches-lock.py")
)
avml = importlib.util.module_from_spec(spec)
spec.loader.exec_module(avml)

LOCK = {
    "package": [
        {"name": "Pillow", "version": "13.0.0", "groups": ["main"]},
        {"name": "yarl", "version": "1.20.0", "groups": ["main"]},
        {"name": "colorama", "version": "0.4.6", "groups": ["main"],
         "markers": {"main": 'sys_platform == "win32"'}},
        {"name": "sibling", "version": "0.1.1", "groups": ["main"],
         "source": {"type": "directory", "url": "../../sibling"}},
        {"name": "torch-extra", "version": "1.0", "groups": ["ml"]},
    ]
}
OK = {"pillow": "13.0.0", "yarl": "1.20.0", "sibling": "0.1.1"}
LINUX = {"sys_platform": "linux"}


def run(installed, env=LINUX, groups=frozenset({"main", "dev"})):
    return avml.check(LOCK, installed, set(groups), env)


def test_conforming_venv_passes():
    assert run(OK) == []


def test_version_drift_is_named():
    assert run({**OK, "pillow": "12.3.0"}) == ["pillow: installed 12.3.0, lock 13.0.0"]


def test_missing_locked_package_is_named():
    assert run({"pillow": "13.0.0", "sibling": "0.1.1"}) == [
        "yarl: locked 1.20.0 but not installed"
    ]


def test_path_dependency_version_is_the_siblings_own():
    assert run({**OK, "sibling": "0.2.0"}) == []


def test_unlisted_distribution_is_named_but_build_tooling_is_not():
    assert run({**OK, "requests": "2.0", "pyinstaller": "6"}) == [
        "requests: installed 2.0 but not in the lock"
    ]


def test_marker_excluded_package_is_not_required():
    assert run(OK) == []
    assert run(OK, env={"sys_platform": "win32"}) == [
        "colorama: locked 0.4.6 but not installed"
    ]


def test_optional_group_is_not_required():
    assert "torch-extra" not in " ".join(run(OK))


def _write_lock(tmp_path):
    lock = tmp_path / "poetry.lock"
    lock.write_text(
        '[[package]]\nname = "pillow"\nversion = "13.0.0"\ngroups = ["main"]\n'
    )
    return lock


def test_main_exits_1_on_drift_and_0_on_conformance(tmp_path, monkeypatch):
    lock = _write_lock(tmp_path)
    monkeypatch.setattr(avml, "installed_distributions", lambda: {"pillow": "12.3.0"})
    assert avml.main([str(lock)]) == 1
    monkeypatch.setattr(avml, "installed_distributions", lambda: {"pillow": "13.0.0"})
    assert avml.main([str(lock)]) == 0


def test_main_exits_2_on_unreadable_lock(tmp_path):
    assert avml.main([str(tmp_path / "missing.lock")]) == 2
