# Unit-test surface() and digest() AS THEY ARE WRITTEN in
# .github/workflows/ci-integrity.yml. Both functions are lifted out of the live
# workflow rather than copied, so this test cannot drift from the shipped code.
#
# These two functions are the whole security property: everything the guard
# claims to detect reduces to "does the surface digest change?". A field
# silently dropped from surface() is a bypass, so every field the header
# advertises has a case here.
import hashlib, io, json, os, sys, textwrap, yaml

WF = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "..", ".github", "workflows", "ci-integrity.yml")

run = yaml.safe_load(io.open(WF, encoding="utf-8"))[
    "jobs"]["guard-gating-workflows"]["steps"][0]["run"]


def lift(fn_name):
    lines = run.split("\n")
    start = next((i for i, l in enumerate(lines)
                  if l.lstrip().startswith("def %s(" % fn_name)), None)
    if start is None:
        print("could not lift %s() from the workflow" % fn_name); sys.exit(1)
    ind = len(lines[start]) - len(lines[start].lstrip())
    end = start + 1
    for i in range(start + 1, len(lines)):
        l = lines[i]
        if l.strip() and (len(l) - len(l.lstrip())) <= ind:
            break
        end = i + 1
    return textwrap.dedent("\n".join(lines[start:end]))


ns = {"yaml": yaml, "hashlib": hashlib, "json": json}
exec(compile(lift("canon") + "\n\n" + lift("digest") + "\n\n"
             + lift("uses_yaml_aliases") + "\n\n" + lift("surface"),
             "lifted", "exec"), ns)
surface, digest = ns["surface"], ns["digest"]
uses_yaml_aliases = ns["uses_yaml_aliases"]

checks = failures = 0


def eq(name, expected, actual):
    global checks, failures
    checks += 1
    if expected == actual:
        print("  PASS  %-62s %s" % (name, actual))
    else:
        print("  FAIL  %-62s expected %r, got %r" % (name, expected, actual))
        failures += 1


BASE = """
name: CI
on:
  pull_request:
    paths: ['src/**']
permissions:
  contents: read
jobs:
  test:
    name: security
    runs-on: ubuntu-latest
    strategy:
      matrix:
        platform: [ubuntu-22.04, windows-latest]
    steps:
      - run: cargo test
"""


def jobs_of(text):
    s = surface(text)
    return None if s is None else s[1]


def wf_of(text):
    s = surface(text)
    return None if s is None else s[0]


def job_changed(mutated, key="test"):
    """Did the mutation change this job's surface digest?"""
    return digest(jobs_of(BASE)[key]) != digest(jobs_of(mutated)[key])


def wf_changed(mutated, _aspect=None):
    # Whole-surface comparison, exactly as the guard does it. Comparing a
    # NAMED key missed `on:` entirely: YAML 1.1 parses it as the boolean
    # True, so `.get("on")` was None on both sides and always "unchanged".
    return digest(wf_of(BASE)) != digest(wf_of(mutated))


print("ci-integrity guard -- surface() (lifted from the workflow)\n")

print("1. Parse outcomes.")
eq("absent file -> empty surface (all base jobs read as removed)", ({}, {}), surface(None))
eq("unparseable YAML -> None (hard error, not 'nothing changed')",
   None, surface("jobs: [not a mapping\n"))
eq("`jobs:` present but not a mapping -> None", None, surface("jobs:\n  - a\n  - b\n"))
eq("top-level scalar -> None", None, surface("just a string\n"))
eq("workflow with no jobs -> no jobs, but a workflow surface",
   {}, surface("name: x\non: push\n")[1])
ACT = """
name: a
runs:
  using: composite
  steps: []
"""
eq("composite action has no jobs...", {}, surface(ACT)[1])
eq("...its `runs:` lands in the workflow surface", True, "runs" in surface(ACT)[0])
eq("changing a composite action's `runs:` is detected", True,
   digest(surface(ACT)[0]) != digest(surface(ACT.replace("steps: []", "steps: [{run: evil}]"))[0]))

print("\n2. Every weakening the header advertises must move the digest.")
# The bypasses that defeated the previous job-name-only revision. Each of these
# keeps the job NAME identical.
eq("gutting a step's `run:` is detected", True,
   job_changed(BASE.replace("run: cargo test", "run: 'true'")))
eq("adding `continue-on-error:` is detected", True,
   job_changed(BASE.replace("    runs-on: ubuntu-latest",
                            "    continue-on-error: true\n    runs-on: ubuntu-latest")))
eq("adding a job-level `if:` is detected", True,
   job_changed(BASE.replace("    runs-on: ubuntu-latest",
                            "    if: false\n    runs-on: ubuntu-latest")))
eq("dropping a matrix platform is detected", True,
   job_changed(BASE.replace("[ubuntu-22.04, windows-latest]", "[ubuntu-22.04]")))
eq("changing `runs-on:` is detected", True,
   job_changed(BASE.replace("runs-on: ubuntu-latest", "runs-on: self-hosted")))
eq("adding a step-level `if:` is detected", True,
   job_changed(BASE.replace("      - run: cargo test",
                            "      - if: false\n        run: cargo test")))
eq("swapping `run:` for a `uses:` is detected", True,
   job_changed(BASE.replace("      - run: cargo test", "      - uses: evil/action@v1")))
eq("renaming the reported check name is detected", True,
   job_changed(BASE.replace("name: security", "name: sekurity")))
eq("narrowing `on.paths` is detected", True, wf_changed(BASE.replace("['src/**']", "['nope/**']"), "on"))
eq("dropping a trigger path glob entirely is detected", True,
   wf_changed(BASE.replace("    paths: ['src/**']\n", ""), "on"))
eq("widening `permissions:` is detected", True,
   wf_changed(BASE.replace("contents: read", "contents: write"), "permissions"))

print("\n2b. Fields an earlier revision MISSED ENTIRELY must move the digest.")
# surface() used to name the fields it captured; this is what that allowlist let
# through. `shell:` is the sharpest — GitHub expands `bash` to
# `bash --noprofile --norc -eo pipefail {0}`, so a custom value drops -e and a
# multi-line `run:` then passes iff its LAST line passes. These cases are the
# reason surface() now captures whole mappings instead of a field list: a unit
# test can assert that a captured field IS captured, but it can never fail on a
# field nobody thought to name.
SH = """
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: |
          bash run-the-gate.sh
          echo done
        shell: bash
"""


def sh_job_changed(mutated):
    return digest(jobs_of(SH)["test"]) != digest(jobs_of(mutated)["test"])


eq("step `shell:` (dropping -e from a multi-line run) is detected", True,
   sh_job_changed(SH.replace("shell: bash", "shell: bash --noprofile --norc {0}")))
eq("step `working-directory:` is detected", True,
   sh_job_changed(SH.replace("        shell: bash",
                             "        working-directory: elsewhere\n        shell: bash")))
eq("step `timeout-minutes:` is detected", True,
   sh_job_changed(SH.replace("        shell: bash",
                             "        timeout-minutes: 1\n        shell: bash")))

for _field, _snippet in [
        ("env", "    env:\n      SKIP: '1'\n"),
        ("container", "    container: evil:latest\n"),
        ("services", "    services:\n      db:\n        image: x\n"),
        ("outputs", "    outputs:\n      ok: 'true'\n"),
        ("environment", "    environment: production\n"),
        ("defaults", "    defaults:\n      run:\n        shell: bash --norc {0}\n")]:
    eq("job-level `%s:` is detected" % _field, True,
       sh_job_changed(SH.replace("    runs-on: ubuntu-latest",
                                 _snippet + "    runs-on: ubuntu-latest")))

for _field, _snippet in [
        ("defaults.run.shell", "defaults:\n  run:\n    shell: bash --norc {0}\n"),
        ("env", "env:\n  SKIP: '1'\n"),
        ("concurrency", "concurrency: one-at-a-time\n")]:
    eq("workflow-level `%s` is detected" % _field, True,
       digest(wf_of(SH)) != digest(wf_of(_snippet + SH)))

print("\n2c. Key COERCION must not let a change hide.")
# YAML 1.1 parses a bare `on:` as the boolean True. Both `str(k)` and
# `json.dumps` collapse True and the string "True" onto one key, so a head file
# could narrow `on.paths` to nothing and re-add a STRING "True" key carrying the
# original value: last-wins left the digest IDENTICAL to base, the guard found
# nothing, and its own trigger was disabled under the weak self-appliable label.
TRIG = """
on:
  pull_request_target:
    branches: [main]
    paths: ['.github/workflows/**']
jobs:
  g:
    runs-on: ubuntu-latest
    steps:
      - run: guard
"""
HIDDEN = """
on:
  pull_request_target:
    paths: ['nothing-matches-this/**']
"True":
  pull_request_target:
    branches: [main]
    paths: ['.github/workflows/**']
jobs:
  g:
    runs-on: ubuntu-latest
    steps:
      - run: guard
"""
eq('a decoy "True" key cannot hide a narrowed `on:`', True,
   digest(wf_of(TRIG)) != digest(wf_of(HIDDEN)))
eq("True and the string 'True' stay distinct keys", True,
   digest({True: 1}) != digest({"True": 1}))
eq("mixed bool/str keys digest without raising (json.dumps cannot)", True,
   isinstance(digest({True: 1, "True": 2, "b": 3}), str))
eq("1 and '1' stay distinct", True, digest({1: "x"}) != digest({"1": "x"}))
eq("digest is stable across key insertion order", True,
   digest({"a": 1, "b": 2}) == digest({"b": 2, "a": 1}))

print("\n3. Additive and cosmetic changes must NOT move an existing digest.")
added_job = BASE + "  extra:\n    name: new gate\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n"
eq("adding a NEW job leaves the existing job's digest alone", False, job_changed(added_job))
eq("adding a NEW job is visible as a new key", True, "extra" in jobs_of(added_job))
eq("a comment-only edit does not move the digest", False,
   job_changed(BASE.replace("      - run: cargo test", "      # a comment\n      - run: cargo test")))
eq("key ORDER does not move the digest", False,
   job_changed(BASE.replace("    name: security\n    runs-on: ubuntu-latest",
                            "    runs-on: ubuntu-latest\n    name: security")))

print("\n4. Job removal is a key difference, not a digest difference.")
removed = BASE.replace("""  test:
    name: security
    runs-on: ubuntu-latest
    strategy:
      matrix:
        platform: [ubuntu-22.04, windows-latest]
    steps:
      - run: cargo test
""", "  other:\n    runs-on: x\n")
eq("removing the job drops its key", False, "test" in jobs_of(removed))

print("\n5. Anchors/aliases must be REFUSED, not silently resolved.")
# PyYAML resolves anchors, aliases and merge keys; GitHub Actions does not
# support them. So an anchored rewrite digests identically to the plain original
# while the gate may stop running entirely — a weakening that would read here as
# "no change at all". The guard therefore declines to certify such a file rather
# than adjudicating whose parser is right.
PLAIN = """
jobs:
  g:
    runs-on: ubuntu-latest
    steps:
      - run: bash gate.sh
"""
ANCHORED = """
x: &s
  - run: bash gate.sh
jobs:
  g:
    runs-on: ubuntu-latest
    steps: *s
"""
MERGED = """
base: &b
  runs-on: ubuntu-latest
  steps:
    - run: bash gate.sh
jobs:
  g:
    <<: *b
"""
INLINE_ANCHOR = """
jobs:
  g:
    runs-on: &r ubuntu-latest
    steps:
      - run: bash gate.sh
"""

eq("plain YAML is not flagged", False, uses_yaml_aliases(PLAIN))
eq("an alias is flagged", True, uses_yaml_aliases(ANCHORED))
eq("a merge key is flagged", True, uses_yaml_aliases(MERGED))
eq("a bare anchor with no alias is flagged", True, uses_yaml_aliases(INLINE_ANCHOR))
eq("unparseable input does not crash the detector", False,
   uses_yaml_aliases("jobs: [oops\n"))
# The reason the refusal is needed at all: resolution really does hide the edit.
eq("an anchored rewrite DOES digest identically (hence the refusal)", True,
   digest(jobs_of(ANCHORED)["g"]) == digest(jobs_of(PLAIN)["g"]))
eq("a merge-key rewrite DOES digest identically too", True,
   digest(jobs_of(MERGED)["g"]) == digest(jobs_of(PLAIN)["g"]))

print()
if failures:
    print("%d of %d assertion(s) FAILED." % (failures, checks)); sys.exit(1)
print("all %d assertion(s) passed." % checks)
