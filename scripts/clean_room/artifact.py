"""The `clean_room` block, and joining it into the published-parity artifact.

The block is assembled from what the scenario recorded; it is then written INTO
`parity-report/published-parity.json` (the manifest axis's machine artifact)
under the key `clean_room`, so one downloaded artifact carries both measurements.

When the manifest axis produced no JSON (it refused, or its job did not reach
the upload), the joined file still carries the clean-room block, with
`parity_report: null` and the reason -- a missing manifest report is stated,
never silently replaced by the clean-room one.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import sys
from collections import Counter
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from clean_room.outcome import OUTCOMES, UNKNOWN_REASONS
else:
    from .outcome import OUTCOMES, UNKNOWN_REASONS

BLOCK_SCHEMA = "clean_room/v1"
REQUIRED_KEYS = ("artifact_version", "platform", "ran_at", "harness_sha")


class ArtifactError(ValueError):
    pass


def summarize(steps: list[dict]) -> dict:
    by_outcome = Counter(s["outcome"] for s in steps)
    unknown_reasons = Counter(s["reason"] for s in steps if s["outcome"] == "unknown")
    return {
        **{o: by_outcome.get(o, 0) for o in OUTCOMES},
        "steps": len(steps),
        "unknown_reasons": dict(sorted(unknown_reasons.items())),
        # Exit criterion 1 reads "every step pass"; anything else is not green.
        "all_pass": bool(steps) and by_outcome.get("pass", 0) == len(steps),
    }


def validate_block(block: dict) -> None:
    for k in REQUIRED_KEYS:
        if not block.get(k):
            raise ArtifactError(f"clean_room block is missing {k!r}")
    for s in block.get("steps", []):
        if s.get("outcome") not in OUTCOMES:
            raise ArtifactError(
                f"step {s.get('step')!r} has outcome {s.get('outcome')!r}"
            )
        if s["outcome"] == "unknown" and s.get("reason") not in UNKNOWN_REASONS:
            raise ArtifactError(
                f"step {s['step']!r} has un-enumerated unknown reason {s.get('reason')!r}"
            )
    fn = block.get("fleet_nouns", {})
    if fn.get("verdict") not in ("clean", "red", "unknown"):
        raise ArtifactError(f"fleet_nouns.verdict {fn.get('verdict')!r}")


def join(
    parity_path: Path | None,
    block: dict,
    out_path: Path,
    parity_unavailable_reason: str | None = None,
) -> dict:
    """Write `block` under `clean_room` into the parity report at parity_path.

    `parity_path=None` means the caller could not obtain the report at all
    (the download failed, or the parity job resolved no release);
    `parity_unavailable_reason` says why, and the joined file says so rather
    than claiming the report was never produced.
    """
    validate_block(block)
    doc: dict
    if parity_path is None:
        doc = {
            "parity_report": None,
            "parity_report_unavailable": (
                f"{parity_unavailable_reason or 'the parity report could not be obtained'}; "
                "the manifest comparison is UNKNOWN for this run, not clean."
            ),
        }
    else:
        try:
            parsed = json.loads(parity_path.read_text(encoding="utf-8-sig"))
        except FileNotFoundError:
            doc = {
                "parity_report": None,
                "parity_report_unavailable": (
                    f"{parity_path} is absent from the parity artifact; "
                    "the manifest comparison is UNKNOWN for this run, not clean."
                ),
            }
        except (ValueError, UnicodeDecodeError) as exc:
            doc = {
                "parity_report": None,
                "parity_report_unavailable": f"{parity_path} is not JSON ({exc}); UNKNOWN, not clean.",
            }
        else:
            if isinstance(parsed, dict):
                doc = parsed
            else:
                doc = {
                    "parity_report": None,
                    "parity_report_unavailable": f"{parity_path} is JSON but not an object; UNKNOWN, not clean.",
                }
    doc["clean_room"] = block
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    return doc


def stub_block(
    reason: str,
    steps: tuple[str, ...],
    *,
    artifact_version: str,
    platform: str,
    harness_sha: str,
    ran_at: str,
) -> dict:
    """The block for a run whose clean-room job produced none: every step
    unknown(harness_error) with the reason, so the joined artifact states the
    absence instead of looking like a report that predates Phase E."""
    step_json = [
        {
            "step": s,
            "outcome": "unknown",
            "reason": "harness_error",
            "evidence_ref": None,
            "detail": reason,
        }
        for s in steps
    ]
    block = {
        "schema": BLOCK_SCHEMA,
        "artifact_version": artifact_version or "unknown",
        "platform": platform,
        "ran_at": ran_at,
        "harness_sha": harness_sha,
        "coord": "unpaired",
        "preflight": {"verdict": "unknown"},
        "steps": step_json,
        "summary": summarize(step_json),
        "fleet_nouns": {
            "verdict": "unknown",
            "reason": reason,
            "scanned_texts": 0,
            "hits": [],
        },
        "stub": reason,
    }
    validate_block(block)
    return block


def render_summary_markdown(block: dict) -> str:
    s = block["summary"]
    fn = block["fleet_nouns"]
    lines = [
        "",
        "### Clean-room acceptance run",
        "",
        (
            f"Artifact `{block['artifact_version']}` on `{block['platform']}` at {block['ran_at']} "
            f"(harness `{block['harness_sha'][:12]}`). Preflight: **{block['preflight'].get('verdict', 'unknown')}**."
        ),
        "",
        f"pass **{s['pass']}** / fail **{s['fail']}** / unknown **{s['unknown']}** of {s['steps']} steps.",
        "",
        "| Step | Outcome | Reason |",
        "|---|---|---|",
    ]
    for st in block["steps"]:
        reason = st["reason"].replace("|", "\\|")
        lines.append(f"| `{st['step']}` | {st['outcome']} | {reason} |")
    lines += [
        "",
        (
            f"Fleet nouns in rendered UI text and served responses: **{fn['verdict']}** "
            f"({len(fn['hits'])} hit(s); {fn.get('ui_texts_scanned', 0)} rendered-UI text(s), "
            f"{fn.get('http_texts_scanned', fn['scanned_texts'])} served response(s))."
        ),
    ]
    if fn.get("reason"):
        lines.append(f"UNKNOWN because: {fn['reason']}")
    for h in fn["hits"][:50]:
        m = h["match"].replace("`", "'")
        lines.append(f"- `{h['class_id']}` `{m}` in {h['source']} (step `{h['step']}`)")
    lines += ["", "_This report gates nothing._", ""]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Join the clean_room block into the parity artifact."
    )
    ap.add_argument("--block", help="clean-room.json written by the scenario")
    ap.add_argument(
        "--stub-reason",
        help="no block exists: write a stub block (every step unknown) carrying this reason",
    )
    ap.add_argument("--artifact-version", default="unknown", help="with --stub-reason")
    ap.add_argument("--platform", default="windows-x64", help="with --stub-reason")
    ap.add_argument("--harness-sha", default="unknown", help="with --stub-reason")
    ap.add_argument(
        "--parity", help="the parity report JSON; omit when it could not be obtained"
    )
    ap.add_argument("--parity-unavailable-reason", help="why --parity is omitted")
    ap.add_argument("--out", required=True)
    ap.add_argument(
        "--summary-out", help="append a markdown summary here ($GITHUB_STEP_SUMMARY)"
    )
    args = ap.parse_args(argv)
    if bool(args.block) == bool(args.stub_reason):
        ap.error("exactly one of --block / --stub-reason")
    if args.parity is None and not args.parity_unavailable_reason:
        ap.error("--parity omitted: say why with --parity-unavailable-reason")
    if args.block:
        block = json.loads(Path(args.block).read_text(encoding="utf-8"))
    else:
        if __package__ in (None, ""):
            from clean_room.scenario import STEPS
        else:
            from .scenario import STEPS
        block = stub_block(
            args.stub_reason,
            STEPS,
            artifact_version=args.artifact_version,
            platform=args.platform,
            harness_sha=args.harness_sha,
            ran_at=_dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        )
    join(
        Path(args.parity) if args.parity else None,
        block,
        Path(args.out),
        args.parity_unavailable_reason,
    )
    if args.summary_out:
        with open(args.summary_out, "a", encoding="utf-8") as fh:
            fh.write(render_summary_markdown(block))
    print(f"joined clean_room block into {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
