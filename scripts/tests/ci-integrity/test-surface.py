# Unit-test surface() and digest() AS THEY ARE WRITTEN in
# .github/workflows/ci-integrity.yml. Both functions are lifted out of the live
# workflow rather than copied, so this test cannot drift from the shipped code.
#
# These two functions are the whole security property: everything the guard
# claims to detect reduces to "does the surface digest change?". A field
# silently dropped from surface() is a bypass, so every field the header
# advertises has a case here.
import ast, base64, hashlib, io, json, os, re, subprocess, sys, textwrap, time, yaml

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


def lift_line(name):
    """A one-line module constant (`NAME = ...`) from the run block."""
    for l in run.split("\n"):
        if l.lstrip().startswith(name + " = "):
            return l.strip()
    print("could not lift %s from the workflow" % name); sys.exit(1)


# `token` is lifted alongside the digest functions because the guard's own
# docstring tells authors WHICH token to write into the PR body, and an untested
# rule there misroutes every author of the file it governs. Deleting token()'s
# composite-action branch used to leave this suite green while telling every
# action author to write `action#workflow`, which matches nothing.
ns = {"yaml": yaml, "hashlib": hashlib, "json": json, "os": os, "base64": base64}
exec(compile(lift("canon") + "\n\n" + lift("digest") + "\n\n"
             + lift("uses_yaml_merge_key") + "\n\n"
             + lift_line("ALIAS_EXPANSION_BUDGET") + "\n\n"
             + lift("yaml_alias_expansion_exceeds") + "\n\n"
             + lift_line("SCALAR_LENGTH_CAP") + "\n\n"
             + lift("yaml11_divergences") + "\n\n"
             + lift("yaml_duplicate_keys") + "\n\n" + lift("decode_blob") + "\n\n"
             + lift("surface") + "\n\n"
             + lift("token") + "\n\n" + lift_line("WHOLE_FILE_GATES") + "\n\n"
             + lift("file_findings") + "\n\n" + lift("path_findings") + "\n\n"
             + lift("findings_for"),
             "lifted", "exec"), ns)
surface, digest, token = ns["surface"], ns["digest"], ns["token"]
file_findings, path_findings = ns["file_findings"], ns["path_findings"]
uses_yaml_merge_key = ns["uses_yaml_merge_key"]
yaml_alias_expansion_exceeds = ns["yaml_alias_expansion_exceeds"]
yaml11_divergences = ns["yaml11_divergences"]
yaml_duplicate_keys = ns["yaml_duplicate_keys"]
decode_blob = ns["decode_blob"]
findings_for = ns["findings_for"]
SCALAR_LENGTH_CAP = ns["SCALAR_LENGTH_CAP"]
ALIAS_EXPANSION_BUDGET = ns["ALIAS_EXPANSION_BUDGET"]

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

print("\n3. Additive and genuinely cosmetic changes must NOT move an existing digest.")
added_job = BASE + "  extra:\n    name: new gate\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n"
eq("adding a NEW job leaves the existing job's digest alone", False, job_changed(added_job))
eq("adding a NEW job is visible as a new key", True, "extra" in jobs_of(added_job))
eq("a YAML-LEVEL comment does not move the digest", False,
   job_changed(BASE.replace("      - run: cargo test", "      # a comment\n      - run: cargo test")))
eq("key ORDER does not move the digest", False,
   job_changed(BASE.replace("    name: security\n    runs-on: ubuntu-latest",
                            "    runs-on: ubuntu-latest\n    name: security")))

# COMMENTS ARE NOT UNIFORMLY COSMETIC, which is why the assertion above names
# the YAML-level case rather than "a comment-only edit". A block scalar's VALUE
# is the shell text, so a `#` line inside `run: |` is part of the digested
# string. The guard's docstring states this; without the cases below nothing
# would fail if a future edit to surface() made it false again, and an author
# would be told the weak label sufficed for an edit that needs `alters-a-gate`.
BLOCK = BASE.replace("      - run: cargo test",
                     "      - run: |\n          cargo test\n          cargo clippy")
eq("a SHELL comment inside a `run: |` block DOES move the digest", True,
   digest(jobs_of(BLOCK)["test"]) != digest(jobs_of(
       BLOCK.replace("          cargo test", "          # why we test\n          cargo test"))["test"]))
eq("a trailing comment INSIDE a block scalar moves it too", True,
   digest(jobs_of(BLOCK)["test"]) != digest(jobs_of(
       BLOCK.replace("          cargo test", "          cargo test   # trailing"))["test"]))
# The MIRROR of that, and the reason the assertion above names the block scalar
# rather than "a run: line": on a PLAIN `run: cargo test` the value is a FLOW
# scalar, so ` # trailing` is a YAML comment and the parser strips it. Naming
# this case wrongly would push an author to `alters-a-gate` for an edit that
# needs only `declared` -- over-declaring is the same defect mirrored.
eq("a trailing comment on a PLAIN run: scalar is stripped (no move)", False,
   digest(jobs_of(BASE)["test"]) != digest(jobs_of(
       BASE.replace("      - run: cargo test", "      - run: cargo test   # trailing"))["test"]))
# ...but only a RELATIVE re-indent does. Stripping the block's own detected
# indentation means shifting the WHOLE block is cosmetic, and a folded `>`
# scalar folds re-wrapped lines back into one string. Stating otherwise would
# over-declare, which the docstring's own "in either direction" rule forbids.
eq("re-indenting the WHOLE block scalar is cosmetic", False,
   digest(jobs_of(BLOCK)["test"]) != digest(jobs_of(
       BLOCK.replace("          cargo test\n          cargo clippy",
                     "            cargo test\n            cargo clippy"))["test"]))
eq("re-indenting ONE line inside the block is NOT cosmetic", True,
   digest(jobs_of(BLOCK)["test"]) != digest(jobs_of(
       BLOCK.replace("          cargo clippy", "            cargo clippy"))["test"]))
FOLDED = BASE.replace("      - run: cargo test",
                      "      - run: >\n          cargo test\n          --all-targets")
eq("a FOLDED re-wrap at the same indent across ONE space is cosmetic", False,
   digest(jobs_of(FOLDED)["test"]) != digest(jobs_of(
       FOLDED.replace("          cargo test\n          --all-targets",
                      "          cargo\n          test --all-targets"))["test"]))
# Same indent is NECESSARY but not SUFFICIENT, which is why the name above says
# "across ONE space": wrapping at a double space changes the value even though
# nothing moved indent. Stated as the measured OUTCOME on these fixtures and not
# as a YAML rule -- the general rule is subtler than it looks (a trailing space
# before the break reproduces a two-space gap, so "a wrap cannot widen a gap" is
# false), and a wrong mechanism in a comment is exactly what this suite exists to
# stop the docstring doing.
DOUBLE = BASE.replace("      - run: cargo test",
                      "      - run: >\n          cargo test  --all-targets")
eq("a FOLDED re-wrap across a DOUBLE space is NOT cosmetic", True,
   digest(jobs_of(DOUBLE)["test"]) != digest(jobs_of(
       DOUBLE.replace("          cargo test  --all-targets",
                      "          cargo test\n          --all-targets"))["test"]))
# ...but folding is NOT a general licence, which is why no prose here says
# "re-wrapping a folded scalar is safe". A line indented MORE than the block is
# not folded at all -- YAML keeps both the newline and the extra indent -- and
# that hanging-indent style is the ordinary way people wrap. Folded scalars do
# occur under this guard's trigger globs, so this case is reachable, not
# theoretical.
eq("re-wrapping a FOLDED scalar to a HANGING indent is NOT cosmetic", True,
   digest(jobs_of(FOLDED)["test"]) != digest(jobs_of(
       FOLDED.replace("          cargo test\n          --all-targets",
                      "          cargo test\n            --all-targets"))["test"]))
eq("a blank line introduced by a re-wrap is NOT cosmetic", True,
   digest(jobs_of(FOLDED)["test"]) != digest(jobs_of(
       FOLDED.replace("          cargo test\n          --all-targets",
                      "          cargo test\n\n          --all-targets"))["test"]))

print("\n3b. token() -- WHICH identifier the author must write.")
eq("a workflow job token is <stem>#<job-key>", "ci#test",
   token(".github/workflows/ci.yml", "test"))
eq("a non-jobs change is <stem>#workflow", "ci#workflow",
   token(".github/workflows/ci.yml", "workflow"))
eq(".yaml spelling resolves the same stem", "ci#test",
   token(".github/workflows/ci.yaml", "test"))
# The branch a mutation test showed was uncovered: without it every composite
# action author is told to write `action#workflow`, which matches nothing.
eq("a COMPOSITE ACTION's stem is its DIRECTORY, not 'action'", "checkout-sibling#workflow",
   token(".github/actions/checkout-sibling/action.yml", "workflow"))
eq("...and the .yaml spelling of it too", "checkout-sibling#workflow",
   token(".github/actions/checkout-sibling/action.yaml", "workflow"))
eq("a job key with whitespace is collapsed, not left to forge a TSV row", "ci#a b",
   token(".github/workflows/ci.yml", "a\tb"))

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

print("\n5. Merge keys must be REFUSED; anchors and aliases are digested as resolved.")
# PyYAML resolves anchors, aliases and merge keys. GitHub Actions resolves
# anchors and aliases too (since 2025-09-18); the merge key is not documented as
# supported there.
# So an anchored file digests to exactly what Actions runs, while a merge-key
# rewrite digests identically to the plain original although Actions may refuse
# the file -- a weakening that would read here as "no change at all". The guard
# therefore declines to certify a merge key, and certifies anchors normally.
PLAIN = """
jobs:
  g:
    runs-on: ubuntu-latest
    steps:
      - run: bash gate.sh
"""
ANCHORED = """
jobs:
  g:
    runs-on: ubuntu-latest
    steps: &s
      - run: bash gate.sh
  h:
    runs-on: windows-latest
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

eq("plain YAML is not flagged", False, uses_yaml_merge_key(PLAIN))
eq("an alias is not flagged", False, uses_yaml_merge_key(ANCHORED))
eq("a bare anchor with no alias is not flagged", False, uses_yaml_merge_key(INLINE_ANCHOR))
eq("a merge key is flagged", True, uses_yaml_merge_key(MERGED))
# A key explicitly TAGGED as a merge key merges exactly like a plain `<<:`
# (PyYAML's flatten_mapping keys on the tag), whatever its text, so it must be
# refused too: inline, merging an aliased mapping, and an aliased plain `<<`.
TAGGED_MERGE = """
jobs:
  g:
    !!merge k: {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}
"""
TAGGED_MERGE_ALIASED = """
base: &b
  runs-on: ubuntu-latest
  steps:
    - run: bash gate.sh
jobs:
  g:
    !!merge k: *b
"""
ALIASED_MERGE_KEY = """
m: &m <<
base: &b
  runs-on: ubuntu-latest
  steps:
    - run: bash gate.sh
jobs:
  g:
    *m : *b
"""
eq("a !!merge-tagged key DOES digest like the plain job (hence the refusal)", True,
   digest(jobs_of(TAGGED_MERGE)["g"]) == digest(jobs_of(PLAIN)["g"]))
eq("an inline !!merge-tagged key is flagged", True, uses_yaml_merge_key(TAGGED_MERGE))
eq("a !!merge-tagged key merging an alias is flagged", True,
   uses_yaml_merge_key(TAGGED_MERGE_ALIASED))
eq("an aliased plain `<<` key is flagged", True, uses_yaml_merge_key(ALIASED_MERGE_KEY))
# flatten_mapping keys on the key NODE's tag, not on its kind, so a `!!merge`
# SEQUENCE or MAPPING key merges too (complex `? key` syntax, block or flow).
SEQ_MERGE_KEY = """
jobs:
  g:
    ? !!merge [x]
    : {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}
"""
MAP_MERGE_KEY_FLOW = """
jobs:
  g: {? !!merge {} : {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}}
"""
# The other spellings of the same tag: a %TAG shorthand and a verbatim tag.
TAG_DIRECTIVE_MERGE = """%TAG !m! tag:yaml.org,2002:
---
jobs:
  g:
    !m!merge k: {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}
"""
VERBATIM_MERGE = """
jobs:
  g:
    !<tag:yaml.org,2002:merge> k: {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}
"""
# A plain `<<` as a FLOW key resolves to the merge tag implicitly.
FLOW_PLAIN_MERGE = """
jobs:
  g: {<<: {runs-on: ubuntu-latest, steps: [{run: bash gate.sh}]}}
"""
for name, text in [("a !!merge SEQUENCE key", SEQ_MERGE_KEY),
                   ("a !!merge MAPPING key in flow style", MAP_MERGE_KEY_FLOW),
                   ("a %TAG-shorthand merge key", TAG_DIRECTIVE_MERGE),
                   ("a verbatim-tag merge key", VERBATIM_MERGE),
                   ("a plain `<<` flow key", FLOW_PLAIN_MERGE)]:
    eq(name + " DOES digest like the plain job (hence the refusal)", True,
       digest(jobs_of(text)["g"]) == digest(jobs_of(PLAIN)["g"]))
    eq(name + " is refused as !MERGE",
       [("!MERGE", ".github/workflows/x.yml", "-")],
       path_findings(".github/workflows/x.yml", PLAIN, text))
eq("path_findings refuses a !!merge-tagged key as !MERGE",
   [("!MERGE", ".github/workflows/x.yml", "-")],
   path_findings(".github/workflows/x.yml", PLAIN, TAGGED_MERGE_ALIASED))
eq("unparseable input does not crash the detector", False,
   uses_yaml_merge_key("jobs: [oops\n"))
# The reason the refusal is needed at all: resolution really does hide the edit.
eq("a merge-key rewrite DOES digest identically (hence the refusal)", True,
   digest(jobs_of(MERGED)["g"]) == digest(jobs_of(PLAIN)["g"]))
# Why an anchor is safe to certify: the alias is digested as what it resolves
# to, so gutting the anchored block changes the digest of every job using it.
GUTTED = ANCHORED.replace("bash gate.sh", "true")
eq("an aliased job digests as the steps it resolves to", True,
   digest(jobs_of(ANCHORED)["h"]["steps"]) == digest(jobs_of(PLAIN)["g"]["steps"]))
eq("editing the anchored block changes the ALIASING job's digest", False,
   digest(jobs_of(ANCHORED)["h"]) == digest(jobs_of(GUTTED)["h"]))
eq("an anchored file at head is CHANGED per job, not refused", [
   ("CHANGED", ".github/workflows/x.yml", "x#g"),
   ("CHANGED", ".github/workflows/x.yml", "x#h"),
], path_findings(".github/workflows/x.yml", ANCHORED, GUTTED))

print("\n5b. An alias bomb is REFUSED by name before anything walks it.")
# canon() walks aliases in full, so nested aliases cost 10^levels there. The
# detector sums expanded sizes over the composed graph (shared nodes counted by
# identity), which is linear in the file however large the expansion.
def alias_bomb(levels, fan=10):
    lines = ["x0: &a0 [\"lol\"]"]
    for i in range(1, levels + 1):
        lines.append("x%d: &a%d [%s]" % (i, i, ", ".join(["*a%d" % (i - 1)] * fan)))
    return "\n".join(lines) + "\njobs:\n  g:\n    runs-on: ubuntu-latest\n    steps: *a%d\n" % levels
BOMB = alias_bomb(9)
eq("the bomb fixture is small (under 1 KiB)", True, len(BOMB) < 1024)
import time
t0 = time.monotonic()
eq("a 9-level alias bomb is over the budget", True, yaml_alias_expansion_exceeds(BOMB))
eq("...and detecting it takes under a second", True, time.monotonic() - t0 < 1.0)
t0 = time.monotonic()
eq("path_findings refuses it by name, without digesting it",
   [("!ALIASBOMB", ".github/workflows/x.yml", "-")],
   path_findings(".github/workflows/x.yml", PLAIN, BOMB))
eq("...also in under a second", True, time.monotonic() - t0 < 1.0)
eq("a recursive alias counts as over the budget", True,
   yaml_alias_expansion_exceeds("a: &r [1, *r]\njobs: {}\n"))
# The same attack in BYTES: one large scalar aliased many times. Few nodes,
# but canon()/digest() copy the scalar at every occurrence, so each scalar is
# charged 1 per 64 bytes of its text.
BIG = "x" * 50000
BYTE_BOMB = ("big: &s " + BIG + "\nrefs: &r [" + ", ".join(["*s"] * 2000)
             + "]\njobs:\n  g:\n    runs-on: ubuntu-latest\n    steps: *r\n")
eq("the byte-bomb fixture is small (under 70 KB)", True, len(BYTE_BOMB) < 70000)
t0 = time.monotonic()
eq("path_findings refuses an aliased large scalar as !ALIASBOMB",
   [("!ALIASBOMB", ".github/workflows/x.yml", "-")],
   path_findings(".github/workflows/x.yml", PLAIN, BYTE_BOMB))
eq("...in under a second", True, time.monotonic() - t0 < 1.0)
eq("the same large scalar used ONCE stays under the budget", False,
   yaml_alias_expansion_exceeds("big: " + BIG + "\njobs: {}\n"))
eq("a shallow alias (3 levels) stays under the budget", False,
   yaml_alias_expansion_exceeds(alias_bomb(3)))
eq("the ci.yml-shaped anchor fixture is under the budget", False,
   yaml_alias_expansion_exceeds(ANCHORED))
CI_YML = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "..", "..", "..", ".github", "workflows", "ci.yml")
eq("the live ci.yml is under the budget", False,
   yaml_alias_expansion_exceeds(io.open(CI_YML, encoding="utf-8").read()))
eq("unparseable input is not an alias bomb (it is !PARSE)", False,
   yaml_alias_expansion_exceeds("jobs: [oops\n"))

print("\n5c. YAML 1.1 respellings Actions (YAML 1.2) reads differently are REFUSED.")
# PyYAML is YAML 1.1; Actions is YAML 1.2. A plain scalar the two resolve
# differently lets a gate be respelled with an identical digest.
CANON = """
on:
  push:
jobs:
  g:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    continue-on-error: false
    steps:
      - run: bash gate.sh
"""
eq("the canonical workflow (top-level `on:`, true/false, 60) passes", [],
   yaml11_divergences(CANON))
eq("...and path_findings certifies it unchanged", [],
   path_findings(".github/workflows/x.yml", CANON, CANON))
for spelling in ["yes", "Yes", "YES", "true", "True", "TRUE", "ON", "On", "!!bool on",
                 "!!bool yes"]:
    head = CANON.replace("\non:", "\n" + spelling + ":", 1)
    eq("top-level `%s:` digests like `on:` under YAML 1.1 (hence the refusal)" % spelling,
       True, digest(wf_of(head)) == digest(wf_of(CANON)))
    eq("top-level `%s:` is refused as !YAML11" % spelling,
       [("!YAML11", ".github/workflows/x.yml", "-")],
       path_findings(".github/workflows/x.yml", CANON, head))
for old, new_text, why in [
        ("continue-on-error: false", "continue-on-error: no", "a yes/no bool"),
        ("continue-on-error: false", "continue-on-error: off", "an on/off bool"),
        ("timeout-minutes: 60", "timeout-minutes: 1:00", "a sexagesimal int"),
        ("timeout-minutes: 60", "timeout-minutes: 074", "a leading-zero octal"),
        ("timeout-minutes: 60", "timeout-minutes: 6_0", "an underscored int"),
        ("timeout-minutes: 60", "timeout-minutes: 0b111100", "a binary int"),
        ("timeout-minutes: 60", "timeout-minutes: 0o74", "a 1.2-only octal"),
        ("timeout-minutes: 60", "timeout-minutes: 6e1", "a 1.2-only exponent float"),
        ("timeout-minutes: 60", "timeout-minutes: 2026-11-26", "a 1.1 timestamp"),
        ("continue-on-error: false", "continue-on-error: !!bool yes", "an explicit !!bool"),
        ("    steps:", "    on: x\n    steps:", "a NESTED `on` key")]:
    head = CANON.replace(old, new_text, 1)
    assert head != CANON, why
    eq("%s (`%s`) is refused as !YAML11" % (why, new_text.strip()),
       [("!YAML11", ".github/workflows/x.yml", "-")],
       path_findings(".github/workflows/x.yml", CANON, head))
QUOTED = CANON.replace("continue-on-error: false", 'continue-on-error: "no"').replace(
    "timeout-minutes: 60", "timeout-minutes: '1:00'")
eq("quoted scalars are strings to both parsers and pass", [], yaml11_divergences(QUOTED))
eq("an explicit !!str on a plain scalar passes", [],
   yaml11_divergences(CANON.replace("timeout-minutes: 60", "timeout-minutes: !!str 074")))
eq("a divergence is reported with its line, column and text", [(8, 24, "no")],
   yaml11_divergences(CANON.replace("continue-on-error: false", "continue-on-error: no")))
# PyYAML resolves no y/n bool, so `y:` is a different key to it, not a respelling.
eq("top-level `y:` is NOT a respelling under PyYAML (it digests as changed)", False,
   digest(wf_of(CANON.replace("\non:", "\ny:", 1))) == digest(wf_of(CANON)))
eq("a quoted top-level \"on\" passes", [], yaml11_divergences(CANON.replace("\non:", '\n"on":', 1)))
# Bypasses closed in review, each refused by name.
REF = [("!YAML11", ".github/workflows/x.yml", "-")]
for why, head in [
        ("an anchored top-level `on` aliased into a value",
         CANON.replace("\non:", "\n&x on:", 1).replace("    timeout-minutes: 60",
                                                      "    timeout-minutes: 60\n    env: {STRICT: *x}", 1)),
        ("a QUOTED value with an explicit !!bool", CANON.replace("continue-on-error: false",
                                                                 'continue-on-error: !!bool "yes"')),
        ("a quoted !!int", CANON.replace("timeout-minutes: 60", 'timeout-minutes: !!int "074"')),
        ("a single-quoted !!int sexagesimal", CANON.replace("timeout-minutes: 60",
                                                            "timeout-minutes: !!int '1:00'")),
        ("a block-scalar !!int", CANON.replace("timeout-minutes: 60",
                                               "timeout-minutes: !!int |-\n      074")),
        ("a non-specific `!` tag on a quoted value", CANON.replace("continue-on-error: false",
                                                                   'continue-on-error: ! "yes"')),
        ("a non-specific `!` tag on a plain bool", CANON.replace("continue-on-error: false",
                                                                 "continue-on-error: ! true")),
        ("a non-specific `!` tag on a plain int", CANON.replace("timeout-minutes: 60",
                                                                "timeout-minutes: ! 60")),
        ("a decimal over Python's digit limit", CANON.replace("timeout-minutes: 60",
                                                              "timeout-minutes: " + "9" * 5000)),
        ("a U+2028 line break hiding a duplicate key in a comment", CANON.replace(
            "    timeout-minutes: 60", "    timeout-minutes: 5 #\u2028    timeout-minutes: 60")),
        ("a U+0085 anywhere", CANON + "# \u0085\n"),
        ("a U+2029 anywhere", CANON + "# \u2029\n")]:
    assert head != CANON, why
    eq("%s is refused as !YAML11" % why, REF,
       path_findings(".github/workflows/x.yml", CANON, head))
eq("the U+2028 respelling DOES digest like 60 under YAML 1.1 (hence the refusal)", True,
   digest(jobs_of(CANON.replace("    timeout-minutes: 60",
                                "    timeout-minutes: 5 #\u2028    timeout-minutes: 60"))["g"])
   == digest(jobs_of(CANON)["g"]))
eq("a non-specific `!` tag on a plain string passes (a string to both)", [],
   yaml11_divergences(CANON.replace("runs-on: ubuntu-latest", "runs-on: ! ubuntu-latest")))
eq("an untagged quoted value passes", [],
   yaml11_divergences(CANON.replace("continue-on-error: false", "continue-on-error: 'yes'")))
# Keys of one mapping that build EQUAL (last-wins; True == 1 == 1.0 == 0x1).
DUP = [("!DUPKEY", ".github/workflows/x.yml", "-")]
NARROW = CANON.replace("on:\n  push:", "on:\n  workflow_dispatch:", 1)
for why, head in [
        ("`on:` narrowed plus `1:` with the original trigger", NARROW + "1:\n  push:\n"),
        ("`on:` narrowed plus `1.0:`", NARROW + "1.0:\n  push:\n"),
        ("`on:` narrowed plus `0x1:`", NARROW + "0x1:\n  push:\n"),
        ("`on:` narrowed plus `01:`", NARROW + "01:\n  push:\n"),
        ("a plain duplicate key", CANON.replace("    timeout-minutes: 60",
                                                "    timeout-minutes: 1\n    timeout-minutes: 60")),
        ("a non-scalar key", CANON.replace("    timeout-minutes: 60",
                                           "    timeout-minutes: 60\n    ? [a]\n    : b"))]:
    eq("%s is refused" % why, True,
       path_findings(".github/workflows/x.yml", CANON, head) in (DUP, REF))
    eq("...by yaml_duplicate_keys itself (%s)" % why, True, bool(yaml_duplicate_keys(head)))
eq("`on:` narrowed + `1:` DOES digest like the original (hence the refusal)", True,
   digest(wf_of(NARROW + "1:\n  push:\n")) == digest(wf_of(CANON)))
eq("the `1:` collision is refused as !DUPKEY (it is not a 1.1/1.2 divergence)", DUP,
   path_findings(".github/workflows/x.yml", CANON, NARROW + "1:\n  push:\n"))
eq("distinct keys pass", [], yaml_duplicate_keys(CANON))
# A long plain scalar is refused WITHOUT being built (sexagesimal ints build
# in time quadratic in their length).
LONG_SEXA = CANON.replace("timeout-minutes: 60", "timeout-minutes: " + ":".join(["1"] + ["59"] * 70000))
# Counted, not timed: every construct_object call on a scalar over the cap.
_real_construct = yaml.constructor.SafeConstructor.construct_object
_overlong_built = []
def _counting_construct(self, node, deep=False):
    if isinstance(node, yaml.ScalarNode) and len(node.value) > SCALAR_LENGTH_CAP:
        _overlong_built.append(len(node.value))
    return _real_construct(self, node, deep=deep)
yaml.constructor.SafeConstructor.construct_object = _counting_construct
try:
    _long_verdict = path_findings(".github/workflows/x.yml", CANON, LONG_SEXA)
finally:
    yaml.constructor.SafeConstructor.construct_object = _real_construct
eq("a ~210 KB sexagesimal value is refused as !YAML11", REF, _long_verdict)
eq("...and is never BUILT on the way (construct_object not called on it)", [], _overlong_built)
eq("a plain scalar exactly at the cap is still checked normally", [],
   yaml11_divergences(CANON.replace("runs-on: ubuntu-latest", "runs-on: " + "u" * SCALAR_LENGTH_CAP)))
eq("a long QUOTED scalar is not capped (a string to both, built linearly)", [],
   yaml11_divergences(CANON.replace("runs-on: ubuntu-latest", 'runs-on: "' + "u" * 20000 + '"')))
for directive in ("%YAML 1.1", "%YAML 1.2"):
    eq("a `%s` directive is refused as !YAML11" % directive, REF,
       path_findings(".github/workflows/x.yml", CANON, directive + "\n---" + CANON))
eq("a %TAG directive alone is not refused here (tags are resolved and checked)", [],
   yaml11_divergences("%TAG !e! tag:example.com,2000:\n---" + CANON))
eq("a U+FEFF mid-file is refused as !YAML11", REF,
   path_findings(".github/workflows/x.yml", CANON, CANON + "# \ufeff\n"))
eq("a U+FEFF as the very first character (a BOM) passes", [],
   yaml11_divergences("\ufeff" + CANON))
eq("decode_blob decodes valid UTF-8", "on: é",
   decode_blob(base64.b64encode("on: é".encode("utf-8")).decode()))
try:
    decode_blob(base64.b64encode(b"on: \xff").decode())
    strict = False
except UnicodeDecodeError:
    strict = True
eq("decode_blob REFUSES invalid UTF-8 (no replacement characters)", True, strict)
os.environ.setdefault("MERGE_BASE", "base"); os.environ.setdefault("HEAD_SHA", "head")
BAD = base64.b64encode(b"on: \xff\n").decode()
GOOD = base64.b64encode(CANON.encode("utf-8")).decode()
eq("findings_for: invalid UTF-8 at HEAD is !UTF8, naming the head",
   [("!UTF8", ".github/workflows/x.yml", "head")],
   findings_for(".github/workflows/x.yml",
                lambda p, ref: decode_blob(BAD if ref == os.environ["HEAD_SHA"] else GOOD)))
eq("findings_for: invalid UTF-8 at the BASE is !UTF8, naming the base",
   [("!UTF8", ".github/workflows/x.yml", "base")],
   findings_for(".github/workflows/x.yml",
                lambda p, ref: decode_blob(GOOD if ref == os.environ["HEAD_SHA"] else BAD)))
eq("findings_for: valid UTF-8 on both sides goes through path_findings", [],
   findings_for(".github/workflows/x.yml", lambda p, ref: decode_blob(GOOD)))
eq("unparseable input is not a divergence (it is !PARSE)", [],
   yaml11_divergences("jobs: [oops\n"))
# No false positives on this repo's own gating files: every tracked
# .github/**/*.y{a,}ml must pass, the live ci.yml among them.
GH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", ".github")
# TRACKED files only (an untracked scratch file must not decide this).
tracked = sorted(os.path.join(GH, "..", f) for f in subprocess.run(
    ["git", "-C", GH, "ls-files", "--full-name", "--", "."],
    capture_output=True, text=True, check=True).stdout.split()
    if f.endswith((".yml", ".yaml")))
eq("this repo has gating YAML to check (the sweep is not vacuous)", True, len(tracked) >= 30)
sweep = {}
for f in tracked:
    t = io.open(f, encoding="utf-8").read()
    sweep[os.path.relpath(f, GH)] = (yaml11_divergences(t)[:3], yaml_duplicate_keys(t)[:3])
eq("no tracked .github YAML file has a YAML 1.1 divergence", [],
   [(f, d) for f, (d, _) in sweep.items() if d])
eq("no tracked .github YAML file carries a %YAML directive", [],
   [f for f in tracked if re.search(r"(?m)^%YAML", io.open(f, encoding="utf-8").read())])
eq("no tracked .github YAML file has a duplicate key", [],
   [(f, d) for f, (_, d) in sweep.items() if d])
eq("the live ci.yml has no YAML 1.1 divergence", [],
   yaml11_divergences(io.open(os.path.join(GH, "workflows", "ci.yml"), encoding="utf-8").read()))

print("\n6. path_findings() -- routing, and the non-YAML gating script.")
# The defect this closes: surface() YAML-parses its input, the script is
# Python, and a None from surface() is the hard `!PARSE` error -- so before the
# whole-file branch existed NO PR could ever edit the script and pass.
PY_PATH = "scripts/check_untimed_subprocess.py"
PY_BASE = 'EXPECTED_SCAN_ROOTS: tuple[str, ...] = (\n    "crates/a/src",\n)\n'
PY_HEAD = 'EXPECTED_SCAN_ROOTS: tuple[str, ...] = (\n    "crates/a/src",\n    "crates/b/src",\n)\n'
CHG = [("CHANGED", PY_PATH, "check_untimed_subprocess#file")]
eq("the script is unparseable as a surface (the original deadlock)", None,
   surface(PY_BASE + "def f():\n    return 1\n"))
eq("an edited script is CHANGED check_untimed_subprocess#file, not !PARSE", CHG,
   path_findings(PY_PATH, PY_BASE, PY_HEAD))
eq("a comment-only edit still counts (no structure to be cosmetic about)", CHG,
   path_findings(PY_PATH, PY_BASE, PY_BASE + "# note\n"))
eq("an unchanged script yields nothing", [],
   path_findings(PY_PATH, PY_BASE, PY_BASE))
eq("a deleted script is a REMOVAL",
   [("REMOVED", PY_PATH, "check_untimed_subprocess#file")],
   path_findings(PY_PATH, PY_BASE, None))
eq("a script absent at base is a new gate: additive, nothing to name", [],
   path_findings(PY_PATH, None, PY_HEAD))
# Routing is by EXACT path: any other non-YAML file under the trigger still
# fails CLOSED, and a near-miss spelling of the script is not the script.
for other in (".github/actions/a/run.sh", ".github/workflows/README.md",
              "scripts/Check_untimed_subprocess.py", "scripts/check_untimed_subprocess.py.bak"):
    eq("non-gate non-YAML %s fails closed (!PARSE)" % other,
       [("!PARSE", other, "-")],
       path_findings(other, "echo base\n", "echo head\n"))
# The YAML branch is unchanged: a job edit still names <stem>#<job>.
eq("a YAML workflow still goes through surface()",
   [("CHANGED", ".github/workflows/ci.yml", "ci#test")],
   path_findings(".github/workflows/ci.yml", BASE,
                 BASE.replace("cargo test", "cargo test || true")))
eq("token() is unchanged for a .py.yml workflow (no new collision)",
   "ci.py#test", token(".github/workflows/ci.py.yml", "test"))

print("\n7. The trigger, the scope filter and WHOLE_FILE_GATES stay in step, and the job around them stays pinned.")
# A gating path is named more than once, and each name does a different job. The
# `paths:` trigger decides whether the run fires at all. The scope filter's `case`
# arm decides whether the run drops the path before scanning it; the workflow says
# "either half alone guards nothing". WHOLE_FILE_GATES decides whether a non-YAML
# path is compared or hits the `!PARSE` deadlock section 6 exists for. Each of these
# fails silently: the guard reports "No gating workflow modified" for a path the
# filter dropped, and a table entry alone reads as guarded while guarding nothing.
#
# Agreement between the lists is not enough, so the shape of each is pinned too. An
# arm that `continue`s, or one placed after the default, drops a path while every
# list still names it. And a glob reading the same on both sides need not match the
# same paths, because GitHub's `*` stops at a `/` and the shell's does not. That is
# why a trigger glob must be `<dir>/**`, which does mean what its arm `<dir>/*` means.
#
# Agreement and shape still leave the rest of the job open, so its posture is
# pinned too. The workflow, job and step may carry only the keys they carry now
# (no `if:`, `continue-on-error`, `shell:`, `defaults` or `BASH_ENV`), and the
# run block's shell is pinned line for line, its Python by digest. Each of those
# edits passes the self-guard under an honest `alters-a-gate` declaration, which
# names the edit but cannot judge it; this suite goes red on it unless the same
# PR also edits the suite or the pin (see the note at PIN below).
SCOPE_FILTER_OPEN = 'case "${f}" in'
# GitHub only runs workflows from the first, and `uses: ./` actions live in the second.
GATE_DIRECTORIES = {".github/workflows/", ".github/actions/"}


def gate_lists(wf_text):
    """Every name for a gating path in one workflow's text."""
    doc = yaml.safe_load(wf_text)
    body = doc["jobs"]["guard-gating-workflows"]["steps"][0]["run"].split("\n")
    # YAML 1.1 parses a bare `on:` key as the boolean True.
    event = (doc.get(True) or doc.get("on") or {})["pull_request_target"]
    trigger = event["paths"]
    # Only the scope filter's own `case`. Scanning the whole run block let an empty
    # arm in any other `case` stand in for a removed scope arm (a false green), and
    # let an unrelated empty arm read as scope-filter drift (a false red).
    opens = [i for i, l in enumerate(body) if l.strip() == SCOPE_FILTER_OPEN]
    rows = []
    for l in body[opens[0] + 1:] if opens else []:
        l = l.strip()
        if l.split("#")[0].strip() == "esac":
            break
        if l and not l.startswith("#"):
            rows.append(l)
    arms, shape = set(), []
    for n, row in enumerate(rows):
        m = re.match(r"^([^()]+?)\)\s*(.*?)\s*;;\s*(#.*)?$", row)
        pattern, action = m.groups()[:2] if m else (None, None)
        if n == len(rows) - 1:
            if (pattern, action) != ("*", "continue"):
                shape.append("the last arm is not `*) continue ;;`: " + row)
        elif action != "":
            shape.append("an arm before the default does not fall through: " + row)
        else:
            arms.update(a.strip() for a in pattern.split("|") if a.strip())

    def is_glob(p):
        return any(c in p for c in "*?[")

    plain = [p for p in trigger if not p.startswith("!")]
    trigger_globs = [p for p in plain if is_glob(p)]
    case_globs = [a for a in arms if is_glob(a)]
    table = next((l.strip() for l in body if l.lstrip().startswith("WHOLE_FILE_GATES = ")), None)
    table = ast.literal_eval(table.split(" = ", 1)[1]) if table else {}
    return {
        "event_keys": sorted(event),
        "opens": len(opens),
        "shape": shape,
        "negated": sorted(p for p in trigger if p.startswith("!")),
        "odd_globs": sorted(p for p in trigger_globs if not re.match(r"^[^*?\[]+/\*\*$", p)),
        "trigger_exact": sorted(p for p in plain if not is_glob(p)),
        "trigger_globs": sorted(p.rstrip("*") for p in trigger_globs),
        "case_exact": sorted(a for a in arms if not is_glob(a)),
        "case_globs": sorted(a.rstrip("*") for a in case_globs),
        "gates": sorted(table),
        "tokens": list(table.values()),
        **guard_posture(doc, body, opens),
    }


# Everything the guard's job may carry. An allowlist rather than a list of
# forbidden keys, for the reason surface() captures whole mappings: a job-level
# `if: false` or `continue-on-error: true`, a step `shell:` that drops `-e`, a
# `defaults.run.shell`, or a `BASH_ENV` in any env mapping each leave the trigger,
# the scope filter and WHOLE_FILE_GATES in perfect agreement while the guard no
# longer gates, and the next way to do that is one nobody has named yet.
GUARD_WF_KEYS = {"name", True, "on", "permissions", "concurrency", "jobs"}
GUARD_JOB_KEYS = {"name", "runs-on", "timeout-minutes", "steps"}
GUARD_STEP_KEYS = {"name", "env", "run"}
GUARD_STEP_ENV = {"GH_TOKEN", "PR_NUMBER", "REPO", "BASE_SHA", "HEAD_SHA"}
# The run block's heredocs: NON_GATING's data and the embedded Python. Any other
# `<<WORD` is reported rather than skipped, so a `: $((1<<true))` cannot hide the
# lines after it from the pin below.
HEREDOCS = {"NONGATING", "PY"}


def shell_lines(body):
    """(heredoc openers not in HEREDOCS, stripped shell code). Comment lines and
    the bodies of the known heredocs are dropped; nothing else is. A terminator
    ends its heredoc only when the line is EXACTLY the word, as in bash: `<<'X'`
    (no `-`) does not end on an indented `X`, so the shell after an indented one
    is heredoc data that never runs, and is read here as never running."""
    out, odd, term = [], [], None
    for l in body:
        if term is not None:
            if l == term:
                term = None
            continue
        s = l.strip()
        if not s or s.startswith("#"):
            continue
        out.append(s)
        for m in re.finditer(r"<<-?\s*(['\"]?)(\w+)\1",
                             s.split(" #")[0].replace("<<<", "   ")):
            if m.group(2) in HEREDOCS and term is None and "<<-" not in m.group(0):
                term = m.group(2)
            else:
                odd.append(s)
    return odd, out


def guard_posture(doc, body, opens):
    job = doc["jobs"]["guard-gating-workflows"]
    steps = job.get("steps") or [{}]
    return {
        "wf_keys": sorted(str(k) for k in doc if k not in GUARD_WF_KEYS),
        "job_keys": sorted(k for k in job if k not in GUARD_JOB_KEYS),
        "steps": len(steps),
        "step_keys": sorted(k for k in steps[0] if k not in GUARD_STEP_KEYS),
        "step_env": sorted(k for k in (steps[0].get("env") or {}) if k not in GUARD_STEP_ENV),
    }


ONLY_PATHS = "the trigger filters on `paths:` alone (no base-branch or `types:` filter)"
ONE_FILTER = "exactly one scope filter was found (else its arms measure nothing)"
SHAPE = "the scope filter is fall-through arms, then one `*) continue ;;`"
EXACT_ANY = "the trigger names at least one exact path (else this section is vacuous)"
COVERS = "the trigger covers every workflow and composite action"
NEGATED = "the trigger has no `!` entry (NON_GATING is the one exception list)"
GLOB_FORM = "every trigger glob is `<dir>/**`"
TRIG_EXACT = "scope-filter exact arms == trigger exact paths"
TRIG_GLOB = "scope-filter glob arms == trigger globs"
TABLE_KEYS = "WHOLE_FILE_GATES keys == trigger exact paths"
TOKENS = "WHOLE_FILE_GATES tokens are distinct"
WF_KEYS = "the guard's workflow carries only name/on/permissions/concurrency/jobs (no `defaults`, no `env`)"
JOB_KEYS = "the guard job carries no key beyond name/runs-on/timeout-minutes/steps (no `if`, `continue-on-error`)"
STEPS = "the guard job is one step (no earlier step can poison GITHUB_ENV)"
STEP_KEYS = "the guard step carries only name/env/run (no `if`, `continue-on-error`, `shell`)"
STEP_ENV = "the guard step's env names only the five values it reads (no BASH_ENV)"
# (name, expected, actual) per check. One table, so the live workflow and every
# mutation below are judged by the same rules.
LIST_CHECKS = [
    (ONLY_PATHS, lambda l: ["paths"], lambda l: l["event_keys"]),
    (ONE_FILTER, lambda l: 1, lambda l: l["opens"]),
    (SHAPE, lambda l: [], lambda l: l["shape"]),
    (EXACT_ANY, lambda l: True, lambda l: bool(l["trigger_exact"])),
    (COVERS, lambda l: [], lambda l: sorted(GATE_DIRECTORIES - set(l["trigger_globs"]))),
    (NEGATED, lambda l: [], lambda l: l["negated"]),
    (GLOB_FORM, lambda l: [], lambda l: l["odd_globs"]),
    (TRIG_EXACT, lambda l: l["trigger_exact"], lambda l: l["case_exact"]),
    (TRIG_GLOB, lambda l: l["trigger_globs"], lambda l: l["case_globs"]),
    (TABLE_KEYS, lambda l: l["trigger_exact"], lambda l: l["gates"]),
    (TOKENS, lambda l: len(l["tokens"]), lambda l: len(set(l["tokens"]))),
    (WF_KEYS, lambda l: [], lambda l: l["wf_keys"]),
    (JOB_KEYS, lambda l: [], lambda l: l["job_keys"]),
    (STEPS, lambda l: 1, lambda l: l["steps"]),
    (STEP_KEYS, lambda l: [], lambda l: l["step_keys"]),
    (STEP_ENV, lambda l: [], lambda l: l["step_env"]),
]
live_lists = gate_lists(io.open(WF, encoding="utf-8").read())
for name, want, got in LIST_CHECKS:
    eq(name, want(live_lists), got(live_lists))
# From this file, not from WF: a workflow copy passed as argv[1] for mutation
# testing lives outside the tree, and the scripts it names do not.
REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))
# A renamed or deleted script leaves all three lists agreeing on a path that no
# longer exists: the trigger then never fires for the script that replaced it.
eq("every WHOLE_FILE_GATES path exists in the tree", [],
   sorted(p for p in live_lists["gates"] if not os.path.isfile(os.path.join(REPO, p))))

# The run block's shell, pinned line for line in guard-shell.pinned (its comment
# lines, blank lines and indentation excluded), plus the embedded Python's digest,
# which covers that body byte for byte, comments included.
# Rules written per line were tried first and each had a bypass that kept them
# green while the guard stopped gating: an `exit 0` or `set +e` before the loop,
# the loop wrapped in `if false`, a second `case "$f"` built from the allowed exit
# lines, `f` rewritten before the filter, NON_GATING reassigned, MERGE_BASE set to
# HEAD_SHA, `|| true` on the Python, the declaration-check `exit 1` deleted,
# `exec true`. An exact pin has no such gaps. What it buys is bounded, though: a
# drift nobody meant goes red, and a deliberate edit costs a second, visible diff
# to the pinned file in the same PR. Neither this suite nor the pin is under the
# guard's own trigger, so a PR that edits both is judged by nothing automated;
# the pin makes that edit conspicuous, not impossible. To refresh it after a deliberate
# change: WRITE_GUARD_SHELL_PIN=1 python3 scripts/tests/ci-integrity/test-surface.py
# rewrites the pinned file from the live workflow; review its diff before committing.
PIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "guard-shell.pinned")


def py_body(run_text):
    """The embedded Python's lines, verbatim (its indentation is syntax)."""
    lines, out, inside = run_text.split("\n"), [], False
    for l in lines:
        if inside:
            if l == "PY":
                return out
            out.append(l)
        elif not l.lstrip().startswith("#") and re.search(r"<<\s*'PY'", l):
            inside = True
    return None


def shell_posture(run_text):
    odd, code = shell_lines(run_text.split("\n"))
    body = py_body(run_text)
    # The Python is pinned by digest, not line by line: a `sys.exit(0)` at its top
    # empties the findings and every gate edit reads as "purely additive".
    tag = "PY body sha256 " + (hashlib.sha256("\n".join(body).encode()).hexdigest()
                               if body is not None else "<no PY heredoc>")
    return {"heredocs": odd, "shell": code + [tag]}


if os.environ.get("WRITE_GUARD_SHELL_PIN") == "1":
    # Only from the live workflow, only by hand, and only when everything above
    # passed: regenerating from a mutated copy would pin the mutation, and a
    # regen in CI would make the pin check unable to fail.
    live = shell_posture(run)
    refusal = ("a workflow path was given; regen reads only the live workflow" if len(sys.argv) > 1
               else "CI is set; the pin is regenerated by hand, never in CI" if os.environ.get("CI")
               else "%d assertion(s) above failed" % failures if failures
               else "unknown heredoc(s): %r" % live["heredocs"] if live["heredocs"] else None)
    if refusal:
        print("refusing to rewrite %s: %s" % (PIN, refusal)); sys.exit(2)
    io.open(PIN, "w", encoding="utf-8").write("\n".join(live["shell"]) + "\n")
    print("rewrote %s" % PIN)
    sys.exit(0)
PINNED = io.open(PIN, encoding="utf-8").read().split("\n")
PINNED = PINNED[:-1] if PINNED and PINNED[-1] == "" else PINNED

HEREDOC_ODD = "the run block's only heredocs are NONGATING and PY, each closed at column 0"
SHELL_PIN = "the run block's shell (line for line) and its Python (by digest) are guard-shell.pinned"


def first_diff(want, got):
    for i, (w, g) in enumerate(zip(want, got)):
        if w != g:
            return "line %d: pinned %r, found %r" % (i + 1, w, g)
    return "pinned %d lines, found %d" % (len(want), len(got)) if len(want) != len(got) else None


SHELL_CHECKS = [
    (HEREDOC_ODD, lambda p: [], lambda p: p["heredocs"]),
    # The first differing line, not the whole list: 140 lines in a FAIL row bury it.
    (SHELL_PIN, lambda p: None, lambda p: first_diff(PINNED, p["shell"])),
]
for name, want, got in SHELL_CHECKS:
    eq(name, want(None), got(shell_posture(run)))


def shell_drift(edits):
    """The SHELL_CHECKS rows the LIVE run block fails after `edits`, or None when an
    anchor is gone (the row then fails loudly rather than passing vacuously)."""
    text = run
    for old, new in edits:
        if old not in text:
            return None
        text = text.replace(old, new, 1)
    got = shell_posture(text)
    return sorted(n for n, want, read in SHELL_CHECKS if want(None) != read(got))


# Every bypass the per-line rules missed, kept as a row against the live text: the
# pin is a statement about it. A missing anchor fails the row instead of skipping it.
SET = "set -euo pipefail\n"
LOOP = "while IFS= read -r f; do\n"
FILTER = 'case "${f}" in\n'
EXPORT = "export HIT_PATHS MERGE_BASE\n"
PY_OPEN = "python3 - <<'PY' > \"${RUNNER_TEMP}/findings.tsv\""
for name, edits, fired in [
    # Stays green.
    ("a comment line is not code", [(SET, SET + "# exit 0 trap set\n")], []),
    ("re-indenting a line is not a change", [(LOOP, "    " + LOOP)], []),
    # Before or inside the scan.
    ("an early `exit 0`", [(SET, SET + "exit 0\n")], [SHELL_PIN]),
    ("`set +e` on line 2", [(SET, SET + "set +e\n")], [SHELL_PIN]),
    ("the loop wrapped in `if false`", [(LOOP, "if false; then\n" + LOOP)], [SHELL_PIN]),
    ("a second `case` built from allowed exit lines",
     [(FILTER, 'case "$f" in\n*ci.yml) ;;\n*) continue ;;\nesac\n' + FILTER)], [SHELL_PIN]),
    ("`f` rewritten before the filter", [(FILTER, 'f="${f/secret-scan/x}"\n' + FILTER)], [SHELL_PIN]),
    ("NON_GATING reassigned after its heredoc",
     [("NONGATING\n\n", "NONGATING\nNON_GATING=.github/workflows/ci.yml\n")], [SHELL_PIN]),
    ("an indented heredoc terminator swallows the script",
     [("\nNONGATING\n", "\n  NONGATING\n")], [SHELL_PIN]),
    # After the scan.
    ("a trap", [(EXPORT, "trap 'exit 0' EXIT\n" + EXPORT)], [SHELL_PIN]),
    ("`exec true`", [(EXPORT, "exec true\n" + EXPORT)], [SHELL_PIN]),
    ("MERGE_BASE set to the head", [(EXPORT, 'MERGE_BASE="${HEAD_SHA}"\n' + EXPORT)], [SHELL_PIN]),
    ("HIT_PATHS rewritten",
     [(EXPORT, 'HIT_PATHS="$(grep -v ci.yml <<< "${HIT_PATHS}" || true)"\n' + EXPORT)], [SHELL_PIN]),
    ("`|| true` on the Python", [(PY_OPEN, PY_OPEN + " || true")], [SHELL_PIN]),
    ("an early `sys.exit(0)` in the Python",
     [("import base64, hashlib, json, os, subprocess\n",
       "import base64, hashlib, json, os, subprocess, sys\nsys.exit(0)\n")], [SHELL_PIN]),
    ("an arithmetic `<<` cannot hide the lines after it",
     [(EXPORT, ": $((1<<true))\ntrap 'exit 0' EXIT\ntrue\n" + EXPORT)], [HEREDOC_ODD, SHELL_PIN]),
    ("a `<<WORD` in a trailing comment is not a heredoc",
     [(EXPORT, EXPORT.rstrip("\n") + "  # <<EOF\n")], [SHELL_PIN]),
]:
    eq("shell drift: " + name, sorted(fired), shell_drift(edits))

# The checks above pass on a list extractor that has quietly stopped working, as
# long as it stopped working the same way on both sides. So each drift they exist
# to catch is kept here as an assertion, naming exactly the checks it must trip.
# A fixture rather than the live workflow, so an ordinary edit to the guard cannot
# break these and they stay a statement about the rules.
LIST_FIXTURE = textwrap.dedent("""\
    on:
      pull_request_target:
        paths:
          - '.github/workflows/**'
          - '.github/actions/**'
          # a YAML comment is not an entry
          - 'scripts/gate.py'
    jobs:
      guard-gating-workflows:
        steps:
          - env:
              PR_NUMBER: "1"
            run: |
              set -euo pipefail
              read -r -d '' NON_GATING <<'NONGATING' || true
              NONGATING
              CHANGED="$(gh api --paginate "repos/${REPO}/pulls/${PR_NUMBER}/files" \\
                --jq '.[] | .filename, (.previous_filename // empty)')"
              HIT_PATHS=""
              while IFS= read -r f; do
                [ -z "${f}" ] && continue
                case "${f}" in
                  .github/workflows/*|.github/actions/*) ;;
                  # a shell comment is not an arm
                  scripts/gate.py) ;;
                  *) continue ;;
                esac
                if grep -qxF "${f}" <<< "${NON_GATING}"; then continue; fi
                case $'\\n'"${HIT_PATHS}" in
                  *$'\\n'"${f}"$'\\n'*) continue ;;
                esac
                HIT_PATHS="${HIT_PATHS}${f}"$'\\n'
              done <<< "${CHANGED}"
              case "${OTHER}" in
                unrelated.py) ;;
              esac
              python3 - <<'PY'
              WHOLE_FILE_GATES = {"scripts/gate.py": "gate#file"}
              PY
    """)
GLOB_ARM = ".github/workflows/*|.github/actions/*) ;;"
EXACT_ARM = "scripts/gate.py) ;;"
DEFAULT_ARM = "*) continue ;;"
TRIGGER_EXACT = "- 'scripts/gate.py'"
TRIGGER_ACTIONS = "- '.github/actions/**'"
TRIGGER_WORKFLOWS = "- '.github/workflows/**'"
TABLE = 'WHOLE_FILE_GATES = {"scripts/gate.py": "gate#file"}'
ARM_BREAK, TRIGGER_BREAK = "\n              ", "\n      "


def list_drift(edits):
    """The name of every LIST_CHECKS row the fixture fails after `edits`, or None."""
    text = LIST_FIXTURE
    for old, new in edits:
        if old not in text:
            return None
        text = text.replace(old, new, 1)
    lists = gate_lists(text)
    return sorted(n for n, want, got in LIST_CHECKS if want(lists) != got(lists))


for name, edits, fired in [
    # Stays green. A red here is the suite crying wolf, which teaches people to ignore it.
    ("the fixture itself is clean", [], []),
    ("two spaces before `;;`", [(EXACT_ARM, "scripts/gate.py)  ;;")], []),
    ("a trailing comment on an arm", [(EXACT_ARM, EXACT_ARM + " # why")], []),
    ("spaces around an alternation", [(GLOB_ARM, ".github/workflows/* | .github/actions/*) ;;")], []),
    ("a commented-out arm is not an arm", [(EXACT_ARM, EXACT_ARM + ARM_BREAK + "# old.py) ;;")], []),
    # The exact half.
    ("the table gains a path",
     [(TABLE, 'WHOLE_FILE_GATES = {"scripts/gate.py": "gate#file", "x.py": "x#file"}')], [TABLE_KEYS]),
    ("the table loses a path", [(TABLE, "WHOLE_FILE_GATES = {}")], [TABLE_KEYS]),
    ("the trigger gains an exact path",
     [(TRIGGER_EXACT, TRIGGER_EXACT + TRIGGER_BREAK + "- 'x.py'")], [TABLE_KEYS, TRIG_EXACT]),
    ("the exact arm is renamed", [(EXACT_ARM, "scripts/gatee.py) ;;")], [TRIG_EXACT]),
    ("the exact arm is removed", [(EXACT_ARM, "")], [TRIG_EXACT]),
    ("an alternation arm adds a path", [(EXACT_ARM, "scripts/gate.py|x.py) ;;")], [TRIG_EXACT]),
    ("another `case` cannot stand in for the scope filter's arm",
     [(EXACT_ARM, ""), ("unrelated.py) ;;", EXACT_ARM)], [TRIG_EXACT]),
    ("two table paths share a token",
     [(TABLE, 'WHOLE_FILE_GATES = {"scripts/gate.py": "t#file", "x.py": "t#file"}')], [TABLE_KEYS, TOKENS]),
    ("every exact path is gone from all three lists",
     [(TRIGGER_EXACT, ""), (EXACT_ARM, ""), (TABLE, "WHOLE_FILE_GATES = {}")], [EXACT_ANY]),
    # The glob half.
    ("the trigger gains a glob with no arm",
     [(TRIGGER_EXACT, TRIGGER_EXACT + TRIGGER_BREAK + "- '.github/scripts/**'")], [TRIG_GLOB]),
    ("a glob arm is narrowed to a subdirectory",
     [(GLOB_ARM, ".github/workflows/gates/*|.github/actions/*) ;;")], [TRIG_GLOB]),
    ("a glob arm is dropped", [(GLOB_ARM, ".github/workflows/*) ;;")], [TRIG_GLOB]),
    ("a glob arm is added with no trigger",
     [(GLOB_ARM, ".github/workflows/*|.github/actions/*|docs/*) ;;")], [TRIG_GLOB]),
    ("both sides narrow to the same subdirectory",
     [(TRIGGER_ACTIONS, "- '.github/actions/gates/**'"),
      (GLOB_ARM, ".github/workflows/*|.github/actions/gates/*) ;;")], [COVERS]),
    ("both sides narrow workflows to the same subdirectory",
     [(TRIGGER_WORKFLOWS, "- '.github/workflows/gates/**'"),
      (GLOB_ARM, ".github/workflows/gates/*|.github/actions/*) ;;")], [COVERS]),
    ("the trigger stops at one directory level", [(TRIGGER_ACTIONS, "- '.github/actions/*'")], [GLOB_FORM]),
    ("both sides narrow to one extension",
     [(TRIGGER_ACTIONS, "- '.github/actions/**/*.yml'"),
      (GLOB_ARM, ".github/workflows/*|.github/actions/**/*.yml) ;;")], [COVERS, GLOB_FORM]),
    ("a `?` in a trigger entry makes it a glob",
     [(TRIGGER_EXACT, TRIGGER_EXACT + TRIGGER_BREAK + "- 'scripts/g?te.py'")], [GLOB_FORM, TRIG_GLOB]),
    ("a `[` in a trigger entry makes it a glob",
     [(TRIGGER_EXACT, TRIGGER_EXACT + TRIGGER_BREAK + "- 'scripts/g[a]te.py'")], [GLOB_FORM, TRIG_GLOB]),
    # What narrows the trigger without touching a path. A `!` entry is an exception
    # list with no justification attached, and the workflow's header records why it
    # carries no base-branch filter.
    ("a trigger negation exempts one workflow",
     [(TRIGGER_EXACT, TRIGGER_EXACT + TRIGGER_BREAK + "- '!.github/workflows/ci.yml'")], [NEGATED]),
    ("a base-branch filter comes back", [("    paths:\n", "    branches: [main]\n    paths:\n")], [ONLY_PATHS]),
    # Every list agrees in these, and the path is dropped anyway.
    ("a `continue` carve-out of one gate",
     [(GLOB_ARM, ".github/workflows/secret-scan.yml) continue ;;" + ARM_BREAK + GLOB_ARM)], [SHAPE]),
    ("a `continue` carve-out by glob", [(GLOB_ARM, "*.sh) continue ;;" + ARM_BREAK + GLOB_ARM)], [SHAPE]),
    ("the exact arm sits after the default",
     [(EXACT_ARM, ""), (DEFAULT_ARM, DEFAULT_ARM + ARM_BREAK + EXACT_ARM)], [SHAPE, TRIG_EXACT]),
    ("the default moves above every arm",
     [(DEFAULT_ARM, ""), (GLOB_ARM, DEFAULT_ARM + ARM_BREAK + GLOB_ARM)], [SHAPE, TRIG_EXACT]),
    ("the default arm stops dropping paths", [(DEFAULT_ARM, "*) ;;")], [SHAPE]),
    ("the last arm drops only some paths", [(DEFAULT_ARM, "docs/*) continue ;;")], [SHAPE]),
    ("`esac` with a trailing comment still ends the filter", [("  esac\n", "  esac  # scope filter\n")], []),
    ("an arm this parser cannot read", [(EXACT_ARM, "scripts/gate.py)" + ARM_BREAK + ";;")], [SHAPE, TRIG_EXACT]),
    # The filter cannot be found, or found twice: nothing it reports can be trusted.
    ("the scope filter's opening line is reworded",
     [('case "${f}" in\n', 'case "$f" in\n')], [ONE_FILTER, TRIG_EXACT, TRIG_GLOB]),
    ("a second `case` on the same variable", [('case "${OTHER}" in\n', 'case "${f}" in\n')], [ONE_FILTER]),
    # Every list agrees in these too, and the job can no longer fail.
    ("a job-level `if:`", [("    steps:\n", "    if: ${{ false }}\n    steps:\n")], [JOB_KEYS]),
    ("a job-level `continue-on-error`",
     [("    steps:\n", "    continue-on-error: true\n    steps:\n")], [JOB_KEYS]),
    ("a step-level `continue-on-error`",
     [("        run: |", "        continue-on-error: true\n        run: |")], [STEP_KEYS]),
    ("a step `shell:` that drops -e",
     [("        run: |", "        shell: bash {0}\n        run: |")], [STEP_KEYS]),
    ("a BASH_ENV in the step's env",
     [('          PR_NUMBER: "1"', '          PR_NUMBER: "1"\n          BASH_ENV: /tmp/x')], [STEP_ENV]),
    ("workflow `defaults` set the shell",
     [("jobs:\n", "defaults:\n  run:\n    shell: sh\njobs:\n")], [WF_KEYS]),
    ("a second step joins the job",
     [("          PY\n", "          PY\n      - run: echo BASH_ENV=/tmp/x >> \"$GITHUB_ENV\"\n")], [STEPS]),
]:
    eq("list drift: " + name, sorted(fired), list_drift(edits))

print()
if failures:
    print("%d of %d assertion(s) FAILED." % (failures, checks)); sys.exit(1)
print("all %d assertion(s) passed." % checks)
