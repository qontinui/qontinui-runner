"""Generate the FOREIGN fixture repository at run time.

Plan 2026-09-20-...-development-environment-exists, Fork 2: no public fixture
repository is published (its licensing is an open operator question), so the
clean-room run builds its foreign repo here, from this script, on every run.

What makes it foreign, deliberately:

  * a different language stack from every Qontinui repo (Rust, TypeScript,
    Python): it is a Go module, and it carries no Cargo.toml, package.json or
    pyproject.toml;
  * no Qontinui files at all -- no .qontinui/, no .claude/, no plans/;
  * a name that is not a sibling-checkout name (asserted against
    fleet-nouns.toml's repo_layout class by the caller's preflight, and by
    `generate()` itself against every class);
  * a real, small service with a FAILING test, so an agent session opened on it
    has an honest first task.

The content is fixed text so every run gets the same repo, and the commit uses
fixed author/committer identity and dates passed on the command line -- nothing
is read from, or written to, the box's git configuration.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from clean_room.fleet_nouns import load_vocabulary
else:
    from .fleet_nouns import load_vocabulary

FIXTURE_NAME = "tally-service"

FILES: dict[str, str] = {
    "go.mod": "module example.com/tally-service\n\ngo 1.21\n",
    "README.md": (
        "# tally-service\n"
        "\n"
        "A tiny HTTP service that adds up numbers.\n"
        "\n"
        "    go run .          # serves GET /total?values=1,2,3 on :8080\n"
        "    go test ./...     # FAILS: see below\n"
        "\n"
        "Known bug: `Sum` drops the last value, so `TestSum` fails. Fixing `Sum`\n"
        "in `tally.go` makes the test pass.\n"
    ),
    "tally.go": (
        "package main\n"
        "\n"
        "import (\n"
        '\t"strconv"\n'
        '\t"strings"\n'
        ")\n"
        "\n"
        "// Sum adds up values. BUG: the loop bound skips the last element.\n"
        "func Sum(values []int) int {\n"
        "\ttotal := 0\n"
        "\tfor i := 0; i < len(values)-1; i++ {\n"
        "\t\ttotal += values[i]\n"
        "\t}\n"
        "\treturn total\n"
        "}\n"
        "\n"
        "// ParseValues reads a comma-separated list of integers.\n"
        "func ParseValues(raw string) ([]int, error) {\n"
        '\tif strings.TrimSpace(raw) == "" {\n'
        "\t\treturn nil, nil\n"
        "\t}\n"
        '\tparts := strings.Split(raw, ",")\n'
        "\tout := make([]int, 0, len(parts))\n"
        "\tfor _, p := range parts {\n"
        "\t\tn, err := strconv.Atoi(strings.TrimSpace(p))\n"
        "\t\tif err != nil {\n"
        "\t\t\treturn nil, err\n"
        "\t\t}\n"
        "\t\tout = append(out, n)\n"
        "\t}\n"
        "\treturn out, nil\n"
        "}\n"
    ),
    "main.go": (
        "package main\n"
        "\n"
        "import (\n"
        '\t"fmt"\n'
        '\t"log"\n'
        '\t"net/http"\n'
        ")\n"
        "\n"
        "func totalHandler(w http.ResponseWriter, r *http.Request) {\n"
        '\tvalues, err := ParseValues(r.URL.Query().Get("values"))\n'
        "\tif err != nil {\n"
        "\t\thttp.Error(w, err.Error(), http.StatusBadRequest)\n"
        "\t\treturn\n"
        "\t}\n"
        '\tfmt.Fprintf(w, "%d\\n", Sum(values))\n'
        "}\n"
        "\n"
        "func main() {\n"
        '\thttp.HandleFunc("/total", totalHandler)\n'
        '\tlog.Fatal(http.ListenAndServe(":8080", nil))\n'
        "}\n"
    ),
    "tally_test.go": (
        "package main\n"
        "\n"
        'import "testing"\n'
        "\n"
        "func TestSum(t *testing.T) {\n"
        "\tif got := Sum([]int{1, 2, 3}); got != 6 {\n"
        '\t\tt.Fatalf("Sum([1 2 3]) = %d, want 6", got)\n'
        "\t}\n"
        "}\n"
        "\n"
        "func TestParseValues(t *testing.T) {\n"
        '\tgot, err := ParseValues("4, 5")\n'
        "\tif err != nil || len(got) != 2 || got[0] != 4 || got[1] != 5 {\n"
        '\t\tt.Fatalf("ParseValues = %v, %v", got, err)\n'
        "\t}\n"
        "}\n"
    ),
}

# Stacks the fixture must NOT look like: the Qontinui repos' own manifests.
QONTINUI_STACK_MARKERS = (
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "tsconfig.json",
)

_GIT_IDENTITY = {
    "GIT_AUTHOR_NAME": "Fixture Author",
    "GIT_AUTHOR_EMAIL": "fixture@example.com",
    "GIT_COMMITTER_NAME": "Fixture Author",
    "GIT_COMMITTER_EMAIL": "fixture@example.com",
    "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
    "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
}


class FixtureError(Exception):
    pass


def _git(repo: Path, *args: str) -> None:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(_GIT_IDENTITY)
    # No global/system config: nothing from the box leaks into the fixture.
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    subprocess.run(
        ["git", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", *args],
        cwd=repo,
        env=env,
        check=True,
        capture_output=True,
        text=True,
        timeout=60,
    )


def generate(
    parent: Path, vocabulary_path: str | None = None, name: str = FIXTURE_NAME
) -> Path:
    """Write the fixture repo under `parent` and commit it. Returns its path.

    Refuses (FixtureError) when the target already exists, or when -- given a
    vocabulary -- any fixture file or the repo name itself hits a fleet-noun
    class: a fixture that carries a fleet noun would plant the very string the
    dynamic scan exists to find.
    """
    repo = parent / name
    if repo.exists():
        raise FixtureError(
            f"{repo} already exists; the fixture is generated fresh, never reused"
        )

    if vocabulary_path is not None:
        vocab = load_vocabulary(vocabulary_path)
        leaks = [(n, h.class_id) for n in FILES for h in vocab.scan_text(FILES[n])]
        leaks += [(name, h.class_id) for h in vocab.scan_text(f"../{name}")]
        if leaks:
            raise FixtureError(f"fixture text hits fleet-noun classes: {leaks}")

    repo.mkdir(parents=True)
    for rel, content in FILES.items():
        (repo / rel).write_text(content, encoding="utf-8", newline="\n")
    for marker in QONTINUI_STACK_MARKERS:
        if (repo / marker).exists():  # pragma: no cover - guards a future edit of FILES
            raise FixtureError(
                f"fixture carries {marker}: it must not share a Qontinui stack"
            )

    _git(repo, "init", "-q")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "tally-service: initial import")
    return repo


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Generate the clean-room foreign fixture repo."
    )
    ap.add_argument(
        "--parent", required=True, help="directory to create the fixture repo in"
    )
    ap.add_argument(
        "--vocabulary",
        help="fleet-nouns.toml; when given, the fixture is checked against it",
    )
    ap.add_argument("--out", help="write {path, name, files} JSON here")
    args = ap.parse_args(argv)
    repo = generate(Path(args.parent), args.vocabulary)
    info = {"path": str(repo), "name": repo.name, "files": sorted(FILES)}
    if args.out:
        Path(args.out).write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")
    print(f"foreign fixture: {repo}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
