"""Regression tests for the embedding service's device / resource configuration.

The service used to build its `EmbeddingConfig` without a `device`, inheriting
the library default of ``"auto"``.  Because python-bridge installs the CUDA
build of torch, ``"auto"`` resolved to ``"cuda"`` on any GPU box and the
process took ~4.8 GB of extra Windows private commit (≈7 GB total for a 90 MB
model) the first time a request arrived.  These tests pin the CPU default and
the bounded resource knobs so that cannot silently come back.

Nothing here imports torch: the module-level configuration is deliberately
resolved before the heavy imports, which happen lazily inside `_get_provider`.
"""

import importlib

import pytest

import embedding_server


@pytest.fixture(autouse=True)
def _restore_module():
    """Reload the module after tests that mutate the environment."""
    yield
    importlib.reload(embedding_server)


class TestResolveDevice:
    def test_unset_defaults_to_cpu(self):
        assert embedding_server.resolve_device(None) == "cpu"

    def test_empty_defaults_to_cpu(self):
        assert embedding_server.resolve_device("   ") == "cpu"

    def test_auto_is_rejected_and_falls_back_to_cpu(self):
        """`auto` is the trap: it silently means `cuda` on a GPU box."""
        assert embedding_server.resolve_device("auto") == "cpu"
        assert "auto" not in embedding_server.VALID_DEVICES

    def test_unknown_device_falls_back_instead_of_crashing(self):
        assert embedding_server.resolve_device("tpu") == "cpu"

    @pytest.mark.parametrize("raw", ["cuda", "CUDA", "  Cuda  "])
    def test_cuda_is_opt_in_and_case_insensitive(self, raw):
        assert embedding_server.resolve_device(raw) == "cuda"

    def test_mps_is_accepted(self):
        assert embedding_server.resolve_device("mps") == "mps"


class TestModuleDefaults:
    def test_device_defaults_to_cpu_when_env_unset(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_DEVICE", raising=False)
        module = importlib.reload(embedding_server)
        assert module.DEVICE == "cpu"

    def test_device_honours_explicit_opt_in(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_DEVICE", "cuda")
        module = importlib.reload(embedding_server)
        assert module.DEVICE == "cuda"

    def test_torch_threads_are_bounded(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_TORCH_THREADS", raising=False)
        module = importlib.reload(embedding_server)
        assert 1 <= module.TORCH_THREADS <= 8

    def test_cache_is_bounded(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_CACHE_SIZE", raising=False)
        module = importlib.reload(embedding_server)
        assert module.CACHE_SIZE > 0


class TestEnvInt:
    def test_reads_valid_value(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_TORCH_THREADS", "6")
        assert embedding_server._env_int("EMBEDDING_TORCH_THREADS", 4, minimum=1) == 6

    def test_non_integer_falls_back(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_TORCH_THREADS", "many")
        assert embedding_server._env_int("EMBEDDING_TORCH_THREADS", 4, minimum=1) == 4

    def test_below_minimum_falls_back(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_CACHE_SIZE", "0")
        assert embedding_server._env_int("EMBEDDING_CACHE_SIZE", 4096, minimum=1) == 4096

    def test_unset_uses_default(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_CACHE_SIZE", raising=False)
        assert embedding_server._env_int("EMBEDDING_CACHE_SIZE", 4096, minimum=1) == 4096
