"""Which `bash` a Python script in this repo should spawn.

THE DEFECT THIS CLOSES
----------------------
`subprocess.run(["bash", ...])` on Windows does not mean "the bash first on
PATH". Measured 2026-09-15 on host spaceship, with Git's usr\\bin ahead of
System32 on PATH: `C:\\Windows\\System32\\bash.exe` (WSL's launcher) exists, a
bare `"bash"` spawned from Python answered `uname -sr` with
`Linux ...-microsoft-standard-WSL2`, and `bash <windows path to a script>`
exited 127 ("No such file or directory"). That is the documented CreateProcess
search order, which tries the system directory before PATH:
https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw

Measured 2026-09-14 on host spaceship by two independent agents:
`python scripts/lint-mutation-discharge.py` (check #53) could not run
`run-guard-tests.sh --list` and exited 1 on origin/main itself, and sessions
were writing ad-hoc wrappers that swapped in Git Bash's absolute path. The
Microsoft Store alias under `...\\Microsoft\\WindowsApps\\bash.exe` is the same
launcher by another door.

RESOLUTION ORDER (`resolve()`)
------------------------------
Not Windows (`os.name != "nt"`):
  1. `$QONTINUI_BASH` when set and non-empty -- returned only if `which()`
     finds it; a typo resolves to NOTHING with a reason, never to a silent skip.
  2. otherwise the bare string `"bash"` -- exactly the argv0 every caller used
     before this module, so Linux/CI behaviour is unchanged. Nothing is probed.

Windows:
  1. `$QONTINUI_BASH` when set and non-empty. A bare name (`bash.exe`) is
     looked up on PATH first, as on the non-Windows arm; a path (including a
     drive-relative `C:bash.exe`) is taken as written. An override that
     resolves to a WSL launcher, to nothing on PATH, or to a file that does
     not exist resolves to NOTHING rather than falling through: an explicit
     choice that is wrong must be loud.
  2. Git Bash derived from `git` on PATH. The hit is passed through
     `realpath` first, then the walk goes up to three levels from its directory
     (`<root>\\cmd`, `<root>\\bin`, `<root>\\mingw64\\bin`, `<root>\\mingw32\\bin`,
     `<root>\\clangarm64\\bin`). A directory is
     accepted as a root ONLY if it looks like Git for Windows -- it carries
     `git-bash.exe`, or `git.exe` under `mingw64\\bin`, `mingw32\\bin` or
     `clangarm64\\bin` (64-bit, 32-bit and ARM64 builds, MinGit included) --
     and then `<root>\\bin\\bash.exe` and `<root>\\usr\\bin\\bash.exe` are
     tried, `bin` FIRST: it is Git's wrapper, and measured on spaceship with a
     PATH of only System32 it started with `MSYSTEM=MINGW64` and
     `PATH=/mingw64/bin:/usr/bin:...` (grep found), where `usr\\bin\\bash.exe`
     left MSYSTEM unset and PATH at System32 alone (grep not found). Without that test a package-manager shim (scoop's
     `...\\scoop\\shims\\git.exe`, chocolatey's `...\\chocolatey\\bin\\git.exe`)
     would make the walk pick an unrelated `<somewhere>\\bin\\bash.exe`.
  3. The standard installs, in this order: `%ProgramFiles%\\Git`,
     `%ProgramW6432%\\Git` (`C:\\Program Files\\Git` when neither is set), then
     the per-user install `%LOCALAPPDATA%\\Programs\\Git`; `bin\\bash.exe` then
     `usr\\bin\\bash.exe`.
  4. Every PATH entry in PATH order (surrounding double quotes stripped),
     testing `<dir>\\bash.exe` -- an MSYS2 or Cygwin install. Every entry, not
     only the first `bash` a lookup would return: System32 is on the machine
     PATH, so a first-hit lookup can return the WSL launcher and stop there.
  Every candidate is refused when it is a WSL launcher (a path component
  `System32` / `Sysnative` / `SysWOW64`, or `Microsoft\\WindowsApps`). When no
  candidate survives, `path` is None and `reason` names the System32 trap and
  each refused launcher once, so a caller can fail with a sentence instead of a
  silent 127.

KNOWN GAPS, NOT HANDLED:
  * The launcher test reads path COMPONENTS as spelled. An 8.3 short-name
    spelling of a system directory (`C:\\WINDOWS\\SYSTEM~1\\bash.exe`) is not
    recognised. No resolver arm produces such a spelling from the standard
    locations; only an override or a PATH entry written that way could.
  * Paths differing only in case are compared case-insensitively for the
    launcher test and the refused list, but are NOT otherwise folded: the first
    candidate that exists wins, however it is spelled.

Pure: every host dependency (`os.name`, the environment, `shutil.which`,
`os.path.isfile`, `os.path.realpath`) is injectable, which is how
`scripts/bash-resolve-selftest.py` drives the Windows arms from any OS.
"""

from __future__ import annotations

import ntpath
import os
import shutil
from typing import Callable, Mapping, NamedTuple

ENV_OVERRIDE = "QONTINUI_BASH"

#: Path components that mark a Windows system directory. Any bash.exe below one
#: is WSL's launcher, never Git Bash.
LAUNCHER_DIRS = frozenset({"system32", "sysnative", "syswow64"})

#: The per-architecture directories a Git for Windows root keeps git.exe under.
GIT_ARCH_DIRS = ("mingw64", "mingw32", "clangarm64")

SYSTEM32_TRAP = (
    "a bare `bash` is not a substitute on Windows: CreateProcess tries the system "
    "directory before PATH, and System32\\bash.exe is WSL's launcher, so a bare "
    "`bash` runs WSL (measured: exit 127 on a Windows script path) instead of Git Bash. "
    f"Install Git for Windows, or set {ENV_OVERRIDE} to a Git Bash bash.exe"
)


class Resolution(NamedTuple):
    #: The argv0 to spawn, or None when no usable bash exists on this host.
    path: str | None
    #: How `path` was chosen, or -- when it is None -- why nothing was.
    reason: str


def is_windows_launcher(path: str) -> bool:
    """True when `path` is WSL's bash launcher rather than a real bash."""
    parts = [p for p in ntpath.normcase(ntpath.normpath(path)).split("\\") if p]
    if LAUNCHER_DIRS.intersection(parts):
        return True
    return any(a == "microsoft" and b == "windowsapps" for a, b in zip(parts, parts[1:]))


def looks_like_git_for_windows(root: str, isfile: Callable[[str], bool]) -> bool:
    """True when `root` is a Git for Windows install root."""
    if isfile(ntpath.join(root, "git-bash.exe")):
        return True
    return any(isfile(ntpath.join(root, arch, "bin", "git.exe")) for arch in GIT_ARCH_DIRS)


def _git_derived(git: str, isfile: Callable[[str], bool]) -> list[str]:
    out: list[str] = []
    d = ntpath.dirname(git)
    for _ in range(3):
        if looks_like_git_for_windows(d, isfile):
            out.append(ntpath.join(d, "bin", "bash.exe"))
            out.append(ntpath.join(d, "usr", "bin", "bash.exe"))
            break
        parent = ntpath.dirname(d)
        if not parent or parent == d:
            break
        d = parent
    return out


def resolve(
    *,
    os_name: str | None = None,
    environ: Mapping[str, str] | None = None,
    which: Callable[[str], str | None] | None = None,
    isfile: Callable[[str], bool] | None = None,
    realpath: Callable[[str], str] | None = None,
) -> Resolution:
    """Resolve the bash to spawn. See the module docstring for the order."""
    os_name = os.name if os_name is None else os_name
    environ = os.environ if environ is None else environ
    which = shutil.which if which is None else which
    isfile = os.path.isfile if isfile is None else isfile
    realpath = os.path.realpath if realpath is None else realpath

    override = environ.get(ENV_OVERRIDE, "")
    if os_name != "nt":
        if override:
            if which(override) is None:
                return Resolution(None, f"${ENV_OVERRIDE}={override} not found (not an executable on PATH or on disk)")
            return Resolution(override, f"${ENV_OVERRIDE}")
        return Resolution("bash", "bare `bash` from PATH (not Windows)")

    if override:
        # A bare name (no drive, no separator) is looked up on PATH first, as the
        # non-Windows arm does; anything path-shaped is taken as written. The
        # launcher and exists checks then apply to whatever was chosen, so a bare
        # `bash.exe` that PATH resolves to System32 is still refused.
        chosen = override
        if (not ntpath.isabs(override) and not ntpath.splitdrive(override)[0]
                and "\\" not in override and "/" not in override):
            found = which(override)
            if not found:
                return Resolution(None, f"${ENV_OVERRIDE}={override} not found on PATH; {SYSTEM32_TRAP}")
            chosen = found
        if is_windows_launcher(chosen):
            return Resolution(None, f"${ENV_OVERRIDE}={override} resolves to WSL's launcher ({chosen}) and was refused; {SYSTEM32_TRAP}")
        if not isfile(chosen):
            return Resolution(None, f"${ENV_OVERRIDE}={override} does not exist; {SYSTEM32_TRAP}")
        return Resolution(chosen, f"${ENV_OVERRIDE}")

    candidates: list[tuple[str, str]] = []
    git = which("git")
    if git:
        git = realpath(git)
        candidates += [(c, f"derived from git at {git}") for c in _git_derived(git, isfile)]

    roots = [ntpath.join(b, "Git") for b in (environ.get(v, "") for v in ("ProgramFiles", "ProgramW6432")) if b]
    roots = roots or ["C:\\Program Files\\Git"]
    localappdata = environ.get("LOCALAPPDATA", "")
    if localappdata:
        roots.append(ntpath.join(localappdata, "Programs", "Git"))
    for root in roots:
        for rel in (("bin", "bash.exe"), ("usr", "bin", "bash.exe")):
            candidates.append((ntpath.join(root, *rel), "a standard Git for Windows install"))

    path_dirs = [d.strip().strip('"') for d in environ.get("PATH", "").split(";")]
    path_dirs = [d for d in path_dirs if d]
    for d in path_dirs:
        candidates.append((ntpath.join(d, "bash.exe"), "bash.exe on PATH"))

    refused: list[str] = []
    refused_seen: set[str] = set()
    for cand, how in candidates:
        if is_windows_launcher(cand):
            key = ntpath.normcase(ntpath.normpath(cand))
            if key not in refused_seen and isfile(cand):
                refused_seen.add(key)
                refused.append(cand)
            continue
        if isfile(cand):
            return Resolution(cand, how)
    detail = "no Git Bash found (checked $%s, git on PATH, %%ProgramFiles%%\\Git, %%LOCALAPPDATA%%\\Programs\\Git, every PATH entry)" % ENV_OVERRIDE
    if refused:
        detail += "; refused WSL launcher(s): " + ", ".join(refused)
    return Resolution(None, f"{detail}; {SYSTEM32_TRAP}")


def resolve_bash(**kwargs) -> str | None:
    """The argv0 to spawn, or None. `resolve(...).reason` says why."""
    return resolve(**kwargs).path
