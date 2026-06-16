"""Framework detection registry and detection logic.

Provides a registry of well-known frameworks with their fingerprints (manifest
files, dependency names, log locations, error patterns) and an async detection
function that inspects a project directory to identify the best-matching
framework.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Framework definition
# ---------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class FrameworkDefinition:
    """Describes a framework's detection fingerprint and log conventions."""

    name: str
    key: str
    language: str
    category: str  # "frontend" | "backend" | "fullstack" | "mobile" | "system"
    detection_files: list[str] = field(default_factory=list)
    detection_deps: list[str] = field(default_factory=list)
    manifest_file: str = ""
    default_log_locations: list[str] = field(default_factory=list)
    default_log_format: str = "plaintext"  # "plaintext" | "json" | "jsonl"
    default_parser: str = "generic"  # "javascript" | "python" | "rust" | "generic"
    error_patterns: list[str] = field(default_factory=list)
    warning_patterns: list[str] = field(default_factory=list)
    keywords: list[str] = field(default_factory=list)
    logs_to_file_by_default: bool = False
    build_tool: str | None = None


# ---------------------------------------------------------------------------
# Shared pattern sets
# ---------------------------------------------------------------------------

_JS_ERROR_PATTERNS: list[str] = [
    r"(?i)\berror\b",
    r"ERR!",
    r"TypeError",
    r"ReferenceError",
    r"SyntaxError",
    r"RangeError",
    r"Unhandled(?:Rejection|Promise)",
    r"FATAL",
    r"Module not found",
]
_JS_WARNING_PATTERNS: list[str] = [
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
    r"ExperimentalWarning",
]

_PY_ERROR_PATTERNS: list[str] = [
    r"(?i)\berror\b",
    r"Traceback \(most recent call last\)",
    r"(?:Key|Value|Type|Attribute|Import|Runtime|OS)Error",
    r"FATAL",
    r"CRITICAL",
]
_PY_WARNING_PATTERNS: list[str] = [
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
    r"DeprecationWarning",
    r"FutureWarning",
]

_RUST_ERROR_PATTERNS: list[str] = [
    r"(?i)\berror\b",
    r"panicked at",
    r"thread '.*' panicked",
    r"FATAL",
    r"unwrap\(\) on .* value",
]
_RUST_WARNING_PATTERNS: list[str] = [
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
]

_GENERIC_ERROR_PATTERNS: list[str] = [
    r"(?i)\berror\b",
    r"(?i)\bfatal\b",
    r"(?i)\bpanic\b",
    r"CRITICAL",
]
_GENERIC_WARNING_PATTERNS: list[str] = [
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
]


# ---------------------------------------------------------------------------
# Framework registry
# ---------------------------------------------------------------------------

FRAMEWORK_REGISTRY: list[FrameworkDefinition] = [
    # ── Node.js / TypeScript ──────────────────────────────────────────────
    FrameworkDefinition(
        name="Next.js",
        key="nextjs",
        language="typescript",
        category="fullstack",
        detection_files=["next.config.js", "next.config.mjs", "next.config.ts"],
        detection_deps=["next"],
        manifest_file="package.json",
        default_log_locations=[".next/server/logs"],
        default_log_format="plaintext",
        default_parser="javascript",
        error_patterns=_JS_ERROR_PATTERNS,
        warning_patterns=_JS_WARNING_PATTERNS,
        keywords=[
            "next",
            "nextjs",
            "react",
            "ssr",
            "server-side rendering",
            "app router",
            "pages router",
            "middleware",
            "api route",
        ],
        logs_to_file_by_default=False,
        build_tool="webpack/turbopack",
    ),
    FrameworkDefinition(
        name="React/Vite",
        key="react-vite",
        language="typescript",
        category="frontend",
        detection_files=[],
        detection_deps=["vite", "@vitejs/plugin-react"],
        manifest_file="package.json",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="javascript",
        error_patterns=_JS_ERROR_PATTERNS,
        warning_patterns=_JS_WARNING_PATTERNS,
        keywords=[
            "vite",
            "react",
            "hmr",
            "hot module replacement",
            "bundle",
            "rollup",
            "esbuild",
        ],
        logs_to_file_by_default=False,
        build_tool="vite",
    ),
    FrameworkDefinition(
        name="Express",
        key="express",
        language="typescript",
        category="backend",
        detection_files=[],
        detection_deps=["express"],
        manifest_file="package.json",
        default_log_locations=["logs"],
        default_log_format="plaintext",
        default_parser="javascript",
        error_patterns=_JS_ERROR_PATTERNS,
        warning_patterns=_JS_WARNING_PATTERNS,
        keywords=[
            "express",
            "middleware",
            "router",
            "http",
            "request",
            "response",
            "rest",
            "api",
        ],
        logs_to_file_by_default=False,
    ),
    FrameworkDefinition(
        name="NestJS",
        key="nestjs",
        language="typescript",
        category="backend",
        detection_files=[],
        detection_deps=["@nestjs/core"],
        manifest_file="package.json",
        default_log_locations=["logs"],
        default_log_format="json",
        default_parser="javascript",
        error_patterns=_JS_ERROR_PATTERNS,
        warning_patterns=_JS_WARNING_PATTERNS,
        keywords=[
            "nestjs",
            "nest",
            "decorator",
            "module",
            "controller",
            "provider",
            "guard",
            "interceptor",
            "pipe",
        ],
        logs_to_file_by_default=False,
    ),
    FrameworkDefinition(
        name="React Native",
        key="react-native",
        language="typescript",
        category="mobile",
        detection_files=[],
        detection_deps=["react-native"],
        manifest_file="package.json",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="javascript",
        error_patterns=_JS_ERROR_PATTERNS,
        warning_patterns=_JS_WARNING_PATTERNS,
        keywords=[
            "react-native",
            "metro",
            "bridge",
            "native module",
            "expo",
            "ios",
            "android",
        ],
        logs_to_file_by_default=False,
        build_tool="metro",
    ),
    # ── Python ────────────────────────────────────────────────────────────
    FrameworkDefinition(
        name="Django",
        key="django",
        language="python",
        category="fullstack",
        detection_files=["manage.py"],
        detection_deps=["django"],
        manifest_file="pyproject.toml",
        default_log_locations=["logs"],
        default_log_format="plaintext",
        default_parser="python",
        error_patterns=_PY_ERROR_PATTERNS,
        warning_patterns=_PY_WARNING_PATTERNS,
        keywords=[
            "django",
            "manage.py",
            "wsgi",
            "asgi",
            "migration",
            "orm",
            "template",
            "middleware",
            "settings",
        ],
        logs_to_file_by_default=False,
    ),
    FrameworkDefinition(
        name="Flask",
        key="flask",
        language="python",
        category="backend",
        detection_files=[],
        detection_deps=["flask"],
        manifest_file="pyproject.toml",
        default_log_locations=["logs"],
        default_log_format="plaintext",
        default_parser="python",
        error_patterns=_PY_ERROR_PATTERNS,
        warning_patterns=_PY_WARNING_PATTERNS,
        keywords=[
            "flask",
            "werkzeug",
            "jinja",
            "blueprint",
            "wsgi",
            "route",
            "endpoint",
        ],
        logs_to_file_by_default=False,
    ),
    FrameworkDefinition(
        name="FastAPI",
        key="fastapi",
        language="python",
        category="backend",
        detection_files=[],
        detection_deps=["fastapi"],
        manifest_file="pyproject.toml",
        default_log_locations=["logs"],
        default_log_format="json",
        default_parser="python",
        error_patterns=_PY_ERROR_PATTERNS,
        warning_patterns=_PY_WARNING_PATTERNS,
        keywords=[
            "fastapi",
            "uvicorn",
            "starlette",
            "pydantic",
            "openapi",
            "async",
            "endpoint",
            "router",
        ],
        logs_to_file_by_default=False,
    ),
    # ── Rust ──────────────────────────────────────────────────────────────
    FrameworkDefinition(
        name="Tauri",
        key="tauri",
        language="rust",
        category="frontend",
        detection_files=["tauri.conf.json", "tauri.conf.json5"],
        detection_deps=["tauri"],
        manifest_file="Cargo.toml",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="rust",
        error_patterns=_RUST_ERROR_PATTERNS,
        warning_patterns=_RUST_WARNING_PATTERNS,
        keywords=[
            "tauri",
            "webview",
            "ipc",
            "invoke",
            "command",
            "window",
            "desktop",
            "system tray",
        ],
        logs_to_file_by_default=False,
        build_tool="cargo",
    ),
    FrameworkDefinition(
        name="Rust/Cargo",
        key="rust-cargo",
        language="rust",
        category="system",
        detection_files=["Cargo.toml"],
        detection_deps=[],
        manifest_file="Cargo.toml",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="rust",
        error_patterns=_RUST_ERROR_PATTERNS,
        warning_patterns=_RUST_WARNING_PATTERNS,
        keywords=[
            "rust",
            "cargo",
            "crate",
            "trait",
            "impl",
            "borrow",
            "lifetime",
            "async",
            "tokio",
        ],
        logs_to_file_by_default=False,
        build_tool="cargo",
    ),
    # ── Other ─────────────────────────────────────────────────────────────
    FrameworkDefinition(
        name="Go",
        key="go",
        language="go",
        category="backend",
        detection_files=["go.mod"],
        detection_deps=[],
        manifest_file="go.mod",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="generic",
        error_patterns=_GENERIC_ERROR_PATTERNS + [r"goroutine \d+"],
        warning_patterns=_GENERIC_WARNING_PATTERNS,
        keywords=[
            "go",
            "golang",
            "goroutine",
            "channel",
            "interface",
            "struct",
            "module",
            "package",
        ],
        logs_to_file_by_default=False,
    ),
    FrameworkDefinition(
        name="Spring Boot",
        key="spring-boot",
        language="java",
        category="backend",
        detection_files=["pom.xml", "build.gradle"],
        detection_deps=["org.springframework.boot"],
        manifest_file="pom.xml",
        default_log_locations=["logs", "target/logs"],
        default_log_format="plaintext",
        default_parser="generic",
        error_patterns=_GENERIC_ERROR_PATTERNS
        + [
            r"(?:java\.lang\.\w+)?Exception",
            r"at [\w.$]+\([\w.]+:\d+\)",
        ],
        warning_patterns=_GENERIC_WARNING_PATTERNS,
        keywords=[
            "spring",
            "boot",
            "bean",
            "controller",
            "service",
            "repository",
            "autowired",
            "configuration",
        ],
        logs_to_file_by_default=True,
    ),
    FrameworkDefinition(
        name="Rails",
        key="rails",
        language="ruby",
        category="fullstack",
        detection_files=["Gemfile", "config/routes.rb"],
        detection_deps=["rails"],
        manifest_file="Gemfile",
        default_log_locations=["log"],
        default_log_format="plaintext",
        default_parser="generic",
        error_patterns=_GENERIC_ERROR_PATTERNS
        + [
            r"ActionController::\w+Error",
            r"ActiveRecord::\w+Error",
        ],
        warning_patterns=_GENERIC_WARNING_PATTERNS,
        keywords=[
            "rails",
            "ruby",
            "activerecord",
            "migration",
            "rake",
            "controller",
            "model",
            "view",
            "gem",
        ],
        logs_to_file_by_default=True,
    ),
    FrameworkDefinition(
        name="Flutter",
        key="flutter",
        language="dart",
        category="mobile",
        detection_files=["pubspec.yaml"],
        detection_deps=["flutter"],
        manifest_file="pubspec.yaml",
        default_log_locations=[],
        default_log_format="plaintext",
        default_parser="generic",
        error_patterns=_GENERIC_ERROR_PATTERNS
        + [
            r"FlutterError",
            r"════════",
        ],
        warning_patterns=_GENERIC_WARNING_PATTERNS,
        keywords=[
            "flutter",
            "dart",
            "widget",
            "stateful",
            "stateless",
            "build",
            "pubspec",
            "material",
            "cupertino",
        ],
        logs_to_file_by_default=False,
        build_tool="flutter",
    ),
]


# ---------------------------------------------------------------------------
# Registry lookup
# ---------------------------------------------------------------------------


def get_framework_definition(key: str) -> FrameworkDefinition | None:
    """Return the :class:`FrameworkDefinition` matching *key*, or ``None``."""
    for fw in FRAMEWORK_REGISTRY:
        if fw.key == key:
            return fw
    return None


# ---------------------------------------------------------------------------
# Manifest parsing helpers
# ---------------------------------------------------------------------------

_LANGUAGE_FROM_MANIFEST: dict[str, str] = {
    "package.json": "typescript",
    "pyproject.toml": "python",
    "Cargo.toml": "rust",
    "go.mod": "go",
    "pom.xml": "java",
    "build.gradle": "java",
    "build.gradle.kts": "java",
    "Gemfile": "ruby",
    "pubspec.yaml": "dart",
}

_CATEGORY_FROM_MANIFEST: dict[str, str] = {
    "package.json": "fullstack",
    "pyproject.toml": "backend",
    "Cargo.toml": "system",
    "go.mod": "backend",
    "pom.xml": "backend",
    "build.gradle": "backend",
    "build.gradle.kts": "backend",
    "Gemfile": "backend",
    "pubspec.yaml": "mobile",
}


def _read_text_safe(path: Path) -> str | None:
    """Read a file as UTF-8, returning ``None`` on any error."""
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        logger.debug("Could not read %s", path)
        return None


def _extract_node_deps(data: dict[str, Any]) -> set[str]:
    """Extract all dependency names from a parsed ``package.json``."""
    deps: set[str] = set()
    for section in ("dependencies", "devDependencies", "peerDependencies"):
        section_data = data.get(section)
        if isinstance(section_data, dict):
            deps.update(section_data.keys())
    return deps


def _extract_pyproject_deps(text: str) -> set[str]:
    """Extract dependency names from ``pyproject.toml`` text."""
    import tomllib

    deps: set[str] = set()
    try:
        data = tomllib.loads(text)
    except Exception:
        logger.debug("Failed to parse pyproject.toml")
        return deps

    # [tool.poetry.dependencies]
    poetry_deps = data.get("tool", {}).get("poetry", {}).get("dependencies", {})
    if isinstance(poetry_deps, dict):
        deps.update(poetry_deps.keys())

    # [tool.poetry.group.*.dependencies]
    groups = data.get("tool", {}).get("poetry", {}).get("group", {})
    if isinstance(groups, dict):
        for group in groups.values():
            if isinstance(group, dict):
                group_deps = group.get("dependencies", {})
                if isinstance(group_deps, dict):
                    deps.update(group_deps.keys())

    # [project.dependencies] — PEP 621 style (list of requirement strings)
    project_deps = data.get("project", {}).get("dependencies", [])
    if isinstance(project_deps, list):
        for req in project_deps:
            if isinstance(req, str):
                # Extract package name (before any version specifier).
                name = re.split(r"[><=!~;\s\[]", req, maxsplit=1)[0].strip()
                if name:
                    deps.add(name.lower())

    # [project.optional-dependencies]
    optional = data.get("project", {}).get("optional-dependencies", {})
    if isinstance(optional, dict):
        for group_reqs in optional.values():
            if isinstance(group_reqs, list):
                for req in group_reqs:
                    if isinstance(req, str):
                        name = re.split(r"[><=!~;\s\[]", req, maxsplit=1)[0].strip()
                        if name:
                            deps.add(name.lower())

    # Normalise (pip treats hyphens/underscores as equivalent).
    return {d.lower().replace("-", "_").replace(".", "_") for d in deps}


def _extract_cargo_deps(text: str) -> set[str]:
    """Extract dependency names from ``Cargo.toml`` text."""
    import tomllib

    deps: set[str] = set()
    try:
        data = tomllib.loads(text)
    except Exception:
        logger.debug("Failed to parse Cargo.toml")
        return deps

    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        section_data = data.get(section)
        if isinstance(section_data, dict):
            deps.update(section_data.keys())
    return deps


def _extract_gemfile_deps(text: str) -> set[str]:
    """Extract gem names from a ``Gemfile``."""
    deps: set[str] = set()
    for match in re.finditer(r"""gem\s+['"]([^'"]+)['"]""", text):
        deps.add(match.group(1))
    return deps


def _extract_pubspec_deps(text: str) -> set[str]:
    """Extract dependency names from ``pubspec.yaml`` text.

    Uses a simple regex approach to avoid requiring a YAML parser.
    """
    deps: set[str] = set()
    in_deps = False
    for line in text.splitlines():
        stripped = line.strip()
        # Detect dependency sections.
        if re.match(r"^(dependencies|dev_dependencies)\s*:", stripped):
            in_deps = True
            continue
        # A top-level key (no leading whitespace) that isn't a dep section ends
        # the current section.
        if not line.startswith(" ") and not line.startswith("\t") and ":" in stripped:
            in_deps = False
            continue
        if in_deps:
            m = re.match(r"^\s+([\w_-]+)\s*:", stripped)
            if m:
                deps.add(m.group(1))
    return deps


def _text_contains_dep(text: str, dep: str) -> bool:
    """Check whether *dep* appears in a text file (for pom.xml / build.gradle)."""
    return dep in text


# ---------------------------------------------------------------------------
# Detection logic
# ---------------------------------------------------------------------------

# Ordered list of manifests to probe; earlier entries take precedence when
# multiple manifests are found.
_MANIFEST_PROBE_ORDER: list[str] = [
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "pubspec.yaml",
]


def _score_framework(
    fw: FrameworkDefinition,
    project_path: Path,
    found_manifests: dict[str, str | None],
    manifest_deps: dict[str, set[str]],
) -> tuple[int, str]:
    """Score a framework definition against the project.

    Returns ``(score, detected_by)`` where *score* is 0 for no match and
    higher for better matches. *detected_by* describes what triggered the
    match.
    """
    score = 0
    reasons: list[str] = []

    # Manifest must be present for this framework to match.
    if fw.manifest_file and fw.manifest_file not in found_manifests:
        # Special case: build.gradle.kts counts for build.gradle manifests.
        if (
            fw.manifest_file == "pom.xml"
            and "build.gradle" not in found_manifests
            and "build.gradle.kts" not in found_manifests
        ):
            return 0, ""
        if (
            fw.manifest_file not in ("pom.xml",)
            and fw.manifest_file not in found_manifests
        ):
            return 0, ""

    # Detection files — each present file adds points.
    for df in fw.detection_files:
        # Support nested paths like "config/routes.rb".
        check_path = project_path / df
        if check_path.exists():
            score += 10
            reasons.append(f"file:{df}")

    # Detection deps — each found dependency adds points (more specific).
    deps = manifest_deps.get(fw.manifest_file, set())
    # Also check build.gradle / build.gradle.kts / pom.xml via text search.
    for dd in fw.detection_deps:
        dd_lower = dd.lower().replace("-", "_").replace(".", "_")
        # For node/python/rust deps, check parsed dependency sets.
        if dd_lower in {d.lower().replace("-", "_").replace(".", "_") for d in deps}:
            score += 20
            reasons.append(f"dep:{dd}")
        elif dd in deps:
            score += 20
            reasons.append(f"dep:{dd}")
        else:
            # For pom.xml / build.gradle, fall back to raw text search.
            for manifest_name in ("pom.xml", "build.gradle", "build.gradle.kts"):
                text = found_manifests.get(manifest_name)
                if text and _text_contains_dep(text, dd):
                    score += 20
                    reasons.append(f"dep:{dd}(in {manifest_name})")
                    break

    if score == 0:
        return 0, ""

    return score, ", ".join(reasons)


def _detect_sync(project_path_str: str) -> dict[str, Any]:
    """Synchronous framework detection implementation."""
    project_path = Path(project_path_str).resolve()
    if not project_path.is_dir():
        return {
            "framework": "unknown",
            "key": "unknown",
            "language": "unknown",
            "category": "unknown",
            "build_tool": None,
            "logs_to_file_by_default": False,
            "detected_by": "path is not a directory",
        }

    # 1. Discover which manifests exist and read their text.
    found_manifests: dict[str, str | None] = {}
    for manifest_name in _MANIFEST_PROBE_ORDER:
        manifest_path = project_path / manifest_name
        if manifest_path.is_file():
            found_manifests[manifest_name] = _read_text_safe(manifest_path)

    # Also check for Tauri config in a src-tauri subdirectory.
    for tauri_config in ("tauri.conf.json", "tauri.conf.json5"):
        for sub in ("", "src-tauri"):
            check = (
                project_path / sub / tauri_config
                if sub
                else project_path / tauri_config
            )
            if check.is_file():
                # Record as a detection file hit; use manifest key for text.
                found_manifests.setdefault(f"_tauri_config:{tauri_config}", "")

    if not found_manifests:
        return {
            "framework": "unknown",
            "key": "unknown",
            "language": "unknown",
            "category": "unknown",
            "build_tool": None,
            "logs_to_file_by_default": False,
            "detected_by": "no manifest files found",
        }

    # 2. Parse dependencies from manifests.
    manifest_deps: dict[str, set[str]] = {}

    if "package.json" in found_manifests:
        text = found_manifests["package.json"]
        if text:
            try:
                data = json.loads(text)
                manifest_deps["package.json"] = _extract_node_deps(data)
            except json.JSONDecodeError:
                logger.debug("Failed to parse package.json")
                manifest_deps["package.json"] = set()
        else:
            manifest_deps["package.json"] = set()

    if "pyproject.toml" in found_manifests:
        text = found_manifests["pyproject.toml"]
        manifest_deps["pyproject.toml"] = (
            _extract_pyproject_deps(text) if text else set()
        )

    if "Cargo.toml" in found_manifests:
        text = found_manifests["Cargo.toml"]
        manifest_deps["Cargo.toml"] = _extract_cargo_deps(text) if text else set()

    if "Gemfile" in found_manifests:
        text = found_manifests["Gemfile"]
        manifest_deps["Gemfile"] = _extract_gemfile_deps(text) if text else set()

    if "pubspec.yaml" in found_manifests:
        text = found_manifests["pubspec.yaml"]
        manifest_deps["pubspec.yaml"] = _extract_pubspec_deps(text) if text else set()

    # go.mod — no dep parsing needed; detection_files match is sufficient.
    if "go.mod" in found_manifests:
        manifest_deps["go.mod"] = set()

    # pom.xml and build.gradle use raw text matching, no parsed dep set.
    for gradle_name in ("pom.xml", "build.gradle", "build.gradle.kts"):
        if gradle_name in found_manifests:
            manifest_deps.setdefault(gradle_name, set())

    # 3. Score each framework in the registry.
    best_score = 0
    best_fw: FrameworkDefinition | None = None
    best_reason = ""

    for fw in FRAMEWORK_REGISTRY:
        score, reason = _score_framework(
            fw, project_path, found_manifests, manifest_deps
        )
        if score > best_score:
            best_score = score
            best_fw = fw
            best_reason = reason

    # 4. Build the result.
    if best_fw is not None:
        return {
            "framework": best_fw.name,
            "key": best_fw.key,
            "language": best_fw.language,
            "category": best_fw.category,
            "build_tool": best_fw.build_tool,
            "logs_to_file_by_default": best_fw.logs_to_file_by_default,
            "detected_by": best_reason,
        }

    # Fallback: derive language/category from the first manifest found.
    first_manifest = next(iter(found_manifests))
    # Strip any internal prefix (e.g. "_tauri_config:...").
    clean_manifest = (
        first_manifest.split(":")[-1] if ":" in first_manifest else first_manifest
    )
    return {
        "framework": "unknown",
        "key": "unknown",
        "language": _LANGUAGE_FROM_MANIFEST.get(clean_manifest, "unknown"),
        "category": _CATEGORY_FROM_MANIFEST.get(clean_manifest, "unknown"),
        "build_tool": None,
        "logs_to_file_by_default": False,
        "detected_by": f"manifest:{clean_manifest} (no framework matched)",
    }


async def detect_framework(project_path: str) -> dict[str, Any]:
    """Detect the primary framework used by a project.

    Inspects manifest files and their contents at *project_path* to identify
    the best-matching framework from :data:`FRAMEWORK_REGISTRY`.

    Args:
        project_path: Absolute or relative path to the project root.

    Returns:
        A dict with keys ``framework``, ``key``, ``language``, ``category``,
        ``build_tool``, ``logs_to_file_by_default``, and ``detected_by``.
    """
    return await asyncio.to_thread(_detect_sync, project_path)
