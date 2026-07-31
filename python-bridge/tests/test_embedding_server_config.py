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
import sys
import types
from unittest.mock import Mock

import pytest

import embedding_server


@pytest.fixture(autouse=True)
def _restore_module():
    """Reload the module after tests that mutate the environment.

    Ordering is load-bearing: because this fixture is autouse, pytest sets it up
    *before* any test-requested `monkeypatch`, so its finaliser runs *after*
    monkeypatch has restored the environment — and the reload therefore observes
    the real env. If this ever stops being autouse (or starts depending on
    monkeypatch), the reload would capture the mutated env and leak e.g.
    DEVICE="cuda" into every later test.
    """
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
    def _call(self, name, default, minimum=1, maximum=10_000):
        return embedding_server._env_int(name, default, minimum=minimum, maximum=maximum)

    def test_reads_valid_value(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_TORCH_THREADS", "6")
        assert self._call("EMBEDDING_TORCH_THREADS", 4) == 6

    def test_non_integer_falls_back(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_TORCH_THREADS", "many")
        assert self._call("EMBEDDING_TORCH_THREADS", 4) == 4

    def test_below_minimum_falls_back(self, monkeypatch):
        monkeypatch.setenv("EMBEDDING_CACHE_SIZE", "0")
        assert self._call("EMBEDDING_CACHE_SIZE", 4096) == 4096

    def test_above_maximum_falls_back(self, monkeypatch):
        """A runaway cache size is exactly the failure this module exists to stop."""
        monkeypatch.setenv("EMBEDDING_CACHE_SIZE", "100000000")
        assert self._call("EMBEDDING_CACHE_SIZE", 4096) == 4096

    def test_unset_uses_default(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_CACHE_SIZE", raising=False)
        assert self._call("EMBEDDING_CACHE_SIZE", 4096) == 4096


class TestUsableDevice:
    """An accelerator that is named but unusable must not reach the model load.

    If it does, the failure surfaces as HTTP 200 with an empty embedding (the
    endpoint catches the exception), and the Rust client stores that empty
    vector — it deserialises `embedding` and never reads `success`.
    """

    @staticmethod
    def _torch(cuda: bool, mps: bool | None):
        torch = types.SimpleNamespace()
        torch.cuda = types.SimpleNamespace(is_available=lambda: cuda)
        if mps is None:
            torch.backends = types.SimpleNamespace()
        else:
            torch.backends = types.SimpleNamespace(
                mps=types.SimpleNamespace(is_available=lambda: mps)
            )
        return torch

    def test_cpu_is_always_usable(self):
        assert embedding_server._usable_device("cpu", self._torch(cuda=False, mps=None)) == "cpu"

    def test_cuda_kept_when_available(self):
        assert embedding_server._usable_device("cuda", self._torch(cuda=True, mps=None)) == "cuda"

    def test_cuda_downgrades_when_unavailable(self):
        assert embedding_server._usable_device("cuda", self._torch(cuda=False, mps=None)) == "cpu"

    def test_mps_downgrades_when_backend_absent(self):
        assert embedding_server._usable_device("mps", self._torch(cuda=False, mps=None)) == "cpu"

    def test_mps_downgrades_when_unavailable(self):
        assert embedding_server._usable_device("mps", self._torch(cuda=False, mps=False)) == "cpu"

    def test_mps_kept_when_available(self):
        assert embedding_server._usable_device("mps", self._torch(cuda=False, mps=True)) == "mps"


class TestBuildCache:
    """The LRU bound must be a byte bound, not just an entry-count bound."""

    class _FakeCache:
        def __init__(self, max_size, persist_to_disk):
            self.max_size = max_size
            self.persist_to_disk = persist_to_disk
            self.stored = {}

        def set(self, text, model, embedding):
            self.stored[text] = embedding

    def test_disk_persistence_is_off(self):
        cache = embedding_server._build_cache(self._FakeCache, 32)
        assert cache.persist_to_disk is False
        assert cache.max_size == 32

    def test_entries_are_copied_not_viewed(self):
        """Rows of a batch result are numpy *views* that pin the whole batch."""
        np = pytest.importorskip("numpy")
        cache = embedding_server._build_cache(self._FakeCache, 32)
        batch = np.zeros((500, 384), dtype=np.float32)
        row = batch[0]
        assert row.base is not None, "precondition: a batch row is a view"

        cache.set("t", "m", row)

        stored = cache.stored["t"]
        assert stored.base is None, "cached entry still pins its parent batch array"
        assert np.array_equal(stored, row)


class TestGetProvider:
    """The regression itself: `EmbeddingConfig` must be built WITH a device.

    The original bug was a missing `device=` at this exact call site, which made
    the config inherit the library default `"auto"` -> CUDA. Asserting on the
    module constants alone would not catch its removal.
    """

    @staticmethod
    def _install_fakes(monkeypatch, captured, cuda_available=False):
        torch = types.SimpleNamespace(
            cuda=types.SimpleNamespace(is_available=lambda: cuda_available),
            backends=types.SimpleNamespace(),
            set_num_threads=lambda n: captured.__setitem__("threads", n),
            get_num_threads=lambda: captured.get("threads", 0),
        )
        monkeypatch.setitem(sys.modules, "torch", torch)

        class FakeCache:
            def __init__(self, max_size, persist_to_disk):
                captured["cache_max_size"] = max_size
                captured["persist_to_disk"] = persist_to_disk

        def fake_get_provider(config):
            captured["config"] = config
            return Mock(dimension=384)

        embeddings = types.ModuleType("qontinui.embeddings")
        embeddings.EmbeddingCache = FakeCache
        embeddings.EmbeddingConfig = lambda **kw: types.SimpleNamespace(
            get_model_name=lambda: "all-MiniLM-L6-v2", **kw
        )
        embeddings.EmbeddingProviderType = types.SimpleNamespace(
            SENTENCE_TRANSFORMERS="sentence-transformers"
        )
        embeddings.get_embedding_provider = fake_get_provider
        embeddings.CachedEmbeddingProvider = lambda base, cache: Mock(dimension=384)
        monkeypatch.setitem(sys.modules, "qontinui.embeddings", embeddings)

    def test_config_is_built_with_an_explicit_device(self, monkeypatch):
        captured: dict = {}
        module = importlib.reload(embedding_server)
        self._install_fakes(monkeypatch, captured)

        module._get_provider()

        assert captured["config"].device == "cpu"
        assert captured["config"].device != "auto"

    def test_cache_is_bounded_and_memory_only(self, monkeypatch):
        captured: dict = {}
        module = importlib.reload(embedding_server)
        self._install_fakes(monkeypatch, captured)

        module._get_provider()

        assert captured["persist_to_disk"] is False
        assert captured["cache_max_size"] == module.CACHE_SIZE

    def test_torch_threads_are_pinned(self, monkeypatch):
        captured: dict = {}
        module = importlib.reload(embedding_server)
        self._install_fakes(monkeypatch, captured)

        module._get_provider()

        assert captured["threads"] == module.TORCH_THREADS

    def test_requested_cuda_downgrades_when_unavailable(self, monkeypatch):
        captured: dict = {}
        monkeypatch.setenv("EMBEDDING_DEVICE", "cuda")
        module = importlib.reload(embedding_server)
        assert module.DEVICE == "cuda"
        self._install_fakes(monkeypatch, captured, cuda_available=False)

        module._get_provider()

        assert captured["config"].device == "cpu"
        assert module.EFFECTIVE_DEVICE == "cpu"
