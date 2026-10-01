"""Three-valued step outcomes for the clean-room scenario.

Every step records `{step, outcome, reason, evidence_ref}` with `outcome` one of
`pass | fail | unknown`:

  pass     the product did what the step asserts, and it was observed.
  fail     the step WAS measured and the product did not do it -- one parity
           defect with a reproduction (the evidence_ref).
  unknown  the step could not be measured. The reason is one of UNKNOWN_REASONS,
           a closed set asserted by scripts/tests/test_clean_room.py, so a new
           way of not knowing has to be named here before it can be reported.

UNKNOWN is never folded into pass or fail. `box_not_clean` and
`ui_bridge_unreachable` in particular are UNKNOWN: a clean-room run on a dirty
box, or a UI that could not be driven, measured nothing about the product.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass

OUTCOMES = ("pass", "fail", "unknown")

# The closed set of reasons a step may be UNKNOWN. Order is documentation only.
UNKNOWN_REASONS: dict[str, str] = {
    "box_not_clean": "the preflight found the box is not foreign (a sibling checkout, a dev-stack or supervisor listener, a QONTINUI_* variable, or a runner already on the product port); nothing measured on it says anything about an external operator's machine",
    "preflight_incomplete": "a foreignness probe itself could not run, so the box was neither shown clean nor shown dirty",
    "vocabulary_unavailable": "fleet-nouns.toml could not be read or parsed, so neither foreignness nor fleet-noun leaks can be judged",
    "ui_bridge_unreachable": "the runner answered HTTP but the UI Bridge never completed an IPC round-trip (or its control routes did not answer), so the UI could not be driven",
    "prior_step_not_passed": "a step this one depends on did not pass, so this one could not be attempted meaningfully",
    "ui_element_not_found": "the UI Bridge answered but no element matching the step's selectors is registered; the harness could not locate the control",
    "feature_not_released": "the runner route this step reads is not served by this release (route-level 404), so the capability could not be measured",
    "refusal_not_observed": "the probe was refused, but not with the pairing refusal the step is about (a drain, a bad request, a 404/500), so the pairing envelope was not observed",
    "tenant_scope_not_refused": "the unpaired box CREATED the tenant-scoped session instead of refusing it: the release either does not read a session's tenant (it predates tenant-scoped sessions) or did not enforce it, and the harness cannot tell which from outside, so the pairing refusal envelope was not observed",
    "transport_error": "a request timed out or the connection failed mid-step, after the runner had been reachable",
    "response_unparseable": "the runner answered with a body that is not the JSON envelope the step reads",
    "harness_error": "the harness itself raised while running the step; the message is in evidence",
}


class OutcomeError(ValueError):
    pass


@dataclass(frozen=True)
class StepResult:
    step: str
    outcome: str
    reason: str
    evidence_ref: str | None
    # Free text for a human: the observed value, the selectors tried, the
    # exception. `reason` is the machine-readable half; this never replaces it.
    detail: str = ""

    def __post_init__(self) -> None:
        if self.outcome not in OUTCOMES:
            raise OutcomeError(
                f"step {self.step!r}: outcome {self.outcome!r} not in {OUTCOMES}"
            )
        if self.outcome == "unknown" and self.reason not in UNKNOWN_REASONS:
            raise OutcomeError(
                f"step {self.step!r}: unknown reason {self.reason!r} is not in the enumerated set "
                f"({', '.join(sorted(UNKNOWN_REASONS))})"
            )
        if self.outcome == "fail" and not self.reason:
            raise OutcomeError(f"step {self.step!r}: a fail must say what was observed")
        if self.outcome == "pass" and not self.reason:
            raise OutcomeError(f"step {self.step!r}: a pass must say what was observed")

    def to_json(self) -> dict:
        return asdict(self)


def passed(
    step: str, reason: str, evidence_ref: str | None = None, detail: str = ""
) -> StepResult:
    return StepResult(step, "pass", reason, evidence_ref, detail)


def failed(
    step: str, reason: str, evidence_ref: str | None = None, detail: str = ""
) -> StepResult:
    return StepResult(step, "fail", reason, evidence_ref, detail)


def unknown(
    step: str, reason: str, evidence_ref: str | None = None, detail: str = ""
) -> StepResult:
    return StepResult(step, "unknown", reason, evidence_ref, detail)
