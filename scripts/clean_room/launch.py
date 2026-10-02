"""Launch the installed runner the way an external operator does.

No QONTINUI_* variable, no isolated config dir, no alternate port: the
published exe is started with the environment the box has (minus any
QONTINUI_* the preflight would already have reported) and must come up on its
own default port with its own default config. That is the operator's first
launch; the dev harness's isolated-instance env table (contract-smoke.ps1
Start-DirectRunner) is deliberately NOT used here, because every one of those
variables is a thing a user does not set.

The working directory is the foreign repo's PARENT, so nothing the runner might
discover by walking up from its cwd is the harness checkout.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Launched:
    process: subprocess.Popen
    stdout_path: Path
    stderr_path: Path


def operator_env(base: dict[str, str] | None = None) -> dict[str, str]:
    env = dict(os.environ if base is None else base)
    for k in list(env):
        if k.upper().startswith("QONTINUI_"):
            del env[k]
    # An agent CLI started from inside another agent session refuses to run;
    # an operator's desktop has no such variable.
    env.pop("CLAUDECODE", None)
    return env


def launch(exe: str, cwd: Path, log_dir: Path) -> Launched:
    log_dir.mkdir(parents=True, exist_ok=True)
    out = log_dir / "runner-stdout.log"
    err = log_dir / "runner-stderr.log"
    with open(out, "wb") as fo, open(err, "wb") as fe:
        proc = subprocess.Popen(
            [exe],
            cwd=str(cwd),
            env=operator_env(),
            stdout=fo,
            stderr=fe,
            stdin=subprocess.DEVNULL,
        )
    return Launched(process=proc, stdout_path=out, stderr_path=err)


def stop(launched: Launched, grace_s: float = 30.0) -> int | None:
    """Stop exactly the process this run started. Returns its exit code."""
    p = launched.process
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(timeout=grace_s)
        except subprocess.TimeoutExpired:
            p.kill()
            try:
                p.wait(timeout=grace_s)
            except subprocess.TimeoutExpired:
                return None
    return p.returncode
