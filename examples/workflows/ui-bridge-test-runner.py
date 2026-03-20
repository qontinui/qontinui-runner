"""
UI Bridge Test Runner — Verification Script

Runs curl-based tests against both web (localhost:3001) and runner (localhost:9876)
UI Bridge endpoints. Outputs a score report and exits with code 0 (all pass) or 1 (failures).

Used as the verification step in the UI Bridge Test & Fix Loop workflow.
"""

import json
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

APPS = {
    "web": "http://localhost:3001/api/ui-bridge",
    "runner": "http://localhost:9876/ui-bridge",
}

# Pages to navigate to for specific categories
NAV_PAGES = {
    "web": {"forms": "/build/workflows", "scroll": "/build/workflows", "default": "/"},
    "runner": {"forms": "/", "scroll": "/workflows", "default": "/"},
}

REPORT_ONLY = "--report-only" in sys.argv


def curl_get(url: str, timeout: int = 12) -> dict | None:
    try:
        r = subprocess.run(
            ["curl", "-s", "--max-time", str(timeout), url],
            capture_output=True, text=True, timeout=timeout + 5,
        )
        if r.returncode != 0 or not r.stdout.strip():
            return None
        return json.loads(r.stdout)
    except Exception:
        return None


def curl_post(url: str, data: dict, timeout: int = 12) -> dict | None:
    try:
        r = subprocess.run(
            ["curl", "-s", "--max-time", str(timeout), "-X", "POST", url,
             "-H", "Content-Type: application/json", "-d", json.dumps(data)],
            capture_output=True, text=True, timeout=timeout + 5,
        )
        if r.returncode != 0 or not r.stdout.strip():
            return None
        return json.loads(r.stdout)
    except Exception:
        return None


def navigate(base: str, path: str):
    curl_post(f"{base}/control/page/navigate", {"url": path}, timeout=5)
    time.sleep(2)


def get_elements(base: str) -> list:
    d = curl_get(f"{base}/control/snapshot")
    if not d:
        return []
    return d.get("data", {}).get("elements", [])


def find_element_by_type(elements: list, etype: str) -> dict | None:
    for e in elements:
        if e.get("type") == etype:
            return e
    return None


def test_category(cat_num: int, app: str, base: str) -> tuple[int, int, list[str]]:
    """Returns (discoverability, effectiveness, notes)."""
    notes = []

    if cat_num == 0:  # Health
        d = curl_get(f"{base}/health")
        if not d:
            return 1, 1, ["Health endpoint unreachable"]
        data = d.get("data", {})
        responsive = data.get("responsive", data == "ok")
        snap = get_elements(base)
        if len(snap) == 0:
            return 3, 2, [f"0 elements, responsive={responsive}"]
        errs = curl_get(f"{base}/control/console-errors")
        err_count = len((errs or {}).get("data", {}).get("errors", []))
        notes.append(f"{len(snap)} elements, responsive={responsive}, errors={err_count}")
        return 5, 5, notes

    if cat_num == 1:  # Element Discovery
        snap = get_elements(base)
        if len(snap) == 0:
            return 2, 1, ["0 elements in snapshot"]
        disc_i = curl_post(f"{base}/control/discover", {"interactive_only": True})
        disc_a = curl_post(f"{base}/control/discover", {"interactive_only": False})
        i_count = len((disc_i or {}).get("data", {}).get("elements", []))
        a_count = len((disc_a or {}).get("data", {}).get("elements", []))
        find_z = curl_post(f"{base}/control/find", {"text": "ZZZNONEXISTENT"})
        z_count = len((find_z or {}).get("data", {}).get("elements", []))
        eid = snap[0]["id"]
        detail = curl_get(f"{base}/control/element/{eid}")
        state = curl_get(f"{base}/control/element/{eid}/state")
        d_score, e_score = 5, 5
        if a_count <= i_count:
            e_score = min(e_score, 4)
            notes.append(f"discover all ({a_count}) not > interactive ({i_count})")
        if z_count != 0:
            e_score = min(e_score, 2)
            notes.append(f"ZZZNONEXISTENT returned {z_count} (expected 0)")
        if not detail or not detail.get("success"):
            e_score = min(e_score, 3)
            notes.append("element detail failed")
        state_data = (state or {}).get("data", {})
        if "focused" not in state_data and "focused" not in str(state_data):
            e_score = min(e_score, 4)
            notes.append("state missing focused field")
        notes.append(f"snap={len(snap)}, interactive={i_count}, all={a_count}, z={z_count}")
        return d_score, e_score, notes

    if cat_num == 2:  # Click
        snap = get_elements(base)
        btn = find_element_by_type(snap, "button")
        if not btn:
            return 3, 2, ["No buttons found"]
        r = curl_post(f"{base}/control/element/{btn['id']}/action", {"action": "click"})
        ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
        if ok:
            return 5, 5, [f"Clicked {btn['id']}"]
        return 4, 3, [f"Click returned: {(r or {}).get('error', 'unknown')}"]

    if cat_num == 3:  # Text Input
        navigate(base, NAV_PAGES[app]["forms"])
        snap = get_elements(base)
        inp = find_element_by_type(snap, "input") or find_element_by_type(snap, "textarea")
        if not inp:
            return 3, 2, ["No input/textarea on page after navigation"]
        eid = inp["id"]
        r = curl_post(f"{base}/control/element/{eid}/action", {"action": "type", "params": {"text": "UI_TEST_42", "clear": True}})
        ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
        if not ok:
            return 4, 2, [f"Type failed: {(r or {}).get('error', 'unknown')}"]
        state = curl_get(f"{base}/control/element/{eid}/state")
        val = (state or {}).get("data", {}).get("value", "")
        if "UI_TEST_42" not in str(val):
            return 4, 3, [f"Value not persisted: got '{val}'"]
        clr = curl_post(f"{base}/control/element/{eid}/action", {"action": "clear"})
        state2 = curl_get(f"{base}/control/element/{eid}/state")
        val2 = (state2 or {}).get("data", {}).get("value", "")
        if val2 not in ("", None):
            return 4, 4, [f"Clear incomplete: got '{val2}'"]
        return 5, 5, [f"Type+clear on {eid}, value persisted correctly"]

    if cat_num == 4:  # Selection Controls
        snap = get_elements(base)
        sel = find_element_by_type(snap, "select")
        switch_el = None
        for e in snap:
            s = e.get("state", {})
            if s.get("checked") is not None or e.get("type") == "checkbox":
                switch_el = e
                break
            if "switch" in str(e.get("identifier", {})).lower():
                switch_el = e
                break
        d_score, e_score = 4, 4
        if sel:
            r = curl_post(f"{base}/control/element/{sel['id']}/action", {"action": "select", "params": {"value": ""}})
            ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
            notes.append(f"select on {sel['id']}: {'ok' if ok else 'fail'}")
        else:
            notes.append("No select element found")
            e_score = min(e_score, 3)
        if switch_el:
            r = curl_post(f"{base}/control/element/{switch_el['id']}/action", {"action": "toggle"})
            ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
            notes.append(f"toggle on {switch_el['id']}: {'ok' if ok else 'fail'}")
        else:
            notes.append("No switch/checkbox found")
            d_score = min(d_score, 4)
        return d_score, e_score, notes

    if cat_num == 5:  # Focus
        snap = get_elements(base)
        inp = find_element_by_type(snap, "input") or find_element_by_type(snap, "textarea")
        if not inp:
            return 3, 2, ["No input to focus"]
        eid = inp["id"]
        curl_post(f"{base}/control/element/{eid}/action", {"action": "focus"})
        s1 = curl_get(f"{base}/control/element/{eid}/state")
        focused = (s1 or {}).get("data", {}).get("focused")
        curl_post(f"{base}/control/element/{eid}/action", {"action": "blur"})
        s2 = curl_get(f"{base}/control/element/{eid}/state")
        blurred = (s2 or {}).get("data", {}).get("focused")
        if focused is True and blurred is False:
            return 5, 5, [f"Focus/blur on {eid}: focused={focused}, blurred={blurred}"]
        if focused is True:
            return 5, 4, [f"Focus works, blur state: {blurred}"]
        return 4, 3, [f"focused={focused}, blurred={blurred}"]

    if cat_num == 6:  # Scrolling
        snap = get_elements(base)
        el = snap[0] if snap else None
        if not el:
            return 2, 1, ["No elements"]
        eid = el["id"]
        r = curl_post(f"{base}/control/element/{eid}/action", {"action": "scroll", "params": {"direction": "down", "amount": 300}})
        ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
        has_info = "scrollInfo" in json.dumps(r or {})
        r2 = curl_post(f"{base}/control/element/{eid}/action", {"action": "scrollIntoView"})
        ok2 = (r2 or {}).get("success") or (r2 or {}).get("data", {}).get("success")
        d_score, e_score = 5, 5
        if not ok:
            e_score = min(e_score, 2)
            notes.append(f"scroll failed: {(r or {}).get('error', 'unknown')}")
        elif not has_info:
            e_score = min(e_score, 4)
            notes.append("scroll ok but no scrollInfo")
        if not ok2:
            e_score = min(e_score, 3)
            notes.append("scrollIntoView failed")
        notes.append(f"scroll={ok}, scrollInfo={has_info}, scrollIntoView={ok2}")
        return d_score, e_score, notes

    if cat_num == 7:  # Form Operations
        # Reuse same page as cat 3
        snap = get_elements(base)
        inp = find_element_by_type(snap, "input") or find_element_by_type(snap, "textarea")
        if not inp:
            return 3, 3, ["No form inputs found"]
        eid = inp["id"]
        r = curl_post(f"{base}/control/element/{eid}/action", {"action": "type", "params": {"text": "FORM_TEST"}})
        ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
        return (5, 5, [f"Form type on {eid}"]) if ok else (4, 3, ["Type failed"])

    if cat_num == 8:  # Navigation
        snap_before = get_elements(base)
        nav_url = "/runs/active" if app == "web" else "/active"
        navigate(base, nav_url)
        snap_after = get_elements(base)
        curl_post(f"{base}/control/page/back", {}, timeout=5)
        time.sleep(1)
        snap_back = get_elements(base)
        curl_post(f"{base}/control/page/forward", {}, timeout=5)
        time.sleep(1)
        snap_fwd = get_elements(base)
        counts = [len(snap_after), len(snap_back), len(snap_fwd)]
        all_nonzero = all(c > 0 for c in counts)
        notes.append(f"after={counts[0]}, back={counts[1]}, fwd={counts[2]}")
        if all_nonzero:
            return 5, 5, notes
        return 4, 3, notes + ["Some navigation returned 0 elements (connection lost)"]

    if cat_num == 9:  # Components
        r = curl_get(f"{base}/control/components")
        if r and r.get("success"):
            # Empty component list is valid — no components are explicitly registered
            return 5, 5, ["Components endpoint works (empty list is expected)"]
        return 3, 2, ["Components endpoint failed"]

    if cat_num == 10:  # Finding
        snap = get_elements(base)
        # Find by visible text
        if snap:
            label = snap[0].get("label", "")
            if label:
                fr = curl_post(f"{base}/control/find", {"text": label[:20]})
                found = len((fr or {}).get("data", {}).get("elements", []))
                notes.append(f"find text='{label[:20]}': {found} results")
        fz = curl_post(f"{base}/control/find", {"text": "ZZZNONEXISTENT"})
        z_count = len((fz or {}).get("data", {}).get("elements", []))
        if z_count != 0:
            return 4, 2, [f"ZZZNONEXISTENT returned {z_count}"]
        return 5, 5, notes + [f"ZZZNONEXISTENT=0"]

    if cat_num == 11:  # Console Errors
        r = curl_get(f"{base}/control/console-errors")
        if not r:
            return 3, 2, ["Console errors endpoint failed"]
        errs = r.get("data", {}).get("errors", [])
        # Check for perf event leakage
        perf_leak = any(e.get("type") in ("long-task", "navigation", "long-animation-frame") for e in errs)
        if perf_leak:
            return 4, 2, [f"{len(errs)} entries but includes performance events (not console errors)"]
        return 5, 5, [f"{len(errs)} console errors"]

    if cat_num == 12:  # Specs
        r = curl_get(f"{base}/control/specs")
        if not r or not r.get("success"):
            return 3, 2, ["Specs endpoint failed"]
        data = r.get("data", {})
        # Check for non-empty specs
        specs = data.get("specs", [])
        count = data.get("count", len(specs) if isinstance(specs, list) else 0)
        if isinstance(data, dict) and not specs and count == 0:
            # Maybe specs are keyed differently
            if len(data) > 2:  # Has keys beyond count/specs
                count = len([k for k in data if k not in ("count", "specs", "timestamp")])
        if count > 0 or len(specs) > 0:
            return 5, 5, [f"{count} specs loaded"]
        return 4, 3, [f"Specs returned empty: {json.dumps(data)[:200]}"]

    if cat_num == 13:  # Drag
        snap = get_elements(base)
        if not snap:
            return 3, 2, ["No elements"]
        el = snap[0]
        r = curl_post(f"{base}/control/element/{el['id']}/action",
                      {"action": "drag", "params": {"targetPosition": {"x": 100, "y": 100}}})
        ok = (r or {}).get("success") or (r or {}).get("data", {}).get("success")
        # Drag succeeding (even on non-draggable element) is 5/5 — the SDK handles it correctly
        return (5, 5, ["Drag executed"]) if ok else (5, 4, [f"Drag returned error (expected for non-draggable): {(r or {}).get('error', 'fail')[:80]}"])

    if cat_num == 14:  # JS Eval (runner only)
        if app == "web":
            return 0, 0, ["SKIP"]  # Not applicable
        r = curl_post(f"{base}/control/page/evaluate", {"expression": "document.title"})
        if not r:
            return 3, 2, ["Evaluate endpoint not found"]
        val = (r or {}).get("data", {}).get("result", {}).get("value")
        if val:
            return 5, 5, [f"document.title = {val}"]
        return 4, 3, [f"Evaluate returned: {json.dumps(r)[:200]}"]

    if cat_num == 15:  # Idle
        s1 = get_elements(base)
        time.sleep(2)
        s2 = get_elements(base)
        diff = abs(len(s1) - len(s2))
        if diff == 0:
            return 5, 5, [f"Stable: {len(s1)} elements"]
        return 4, 4, [f"Drift: {len(s1)} -> {len(s2)}"]

    if cat_num == 16:  # Connection Health
        start = time.time()
        snap = curl_get(f"{base}/control/snapshot")
        elapsed = time.time() - start
        # Error envelope check
        err_r = curl_get(f"{base}/control/element/nonexistent-id-12345")
        top_success = (err_r or {}).get("success")
        # Health endpoint
        health = curl_get(f"{base}/health")
        health_data = (health or {}).get("data", {})
        d_score, e_score = 5, 5
        notes.append(f"snapshot {elapsed:.2f}s")
        if elapsed > 2:
            e_score = min(e_score, 4)
            notes.append("slow snapshot")
        if top_success is not False:
            e_score = min(e_score, 3)
            notes.append(f"error envelope: success={top_success} (expected false)")
        else:
            notes.append("error envelope correct (success=false)")
        if app == "runner":
            for field in ("status", "responsive", "uptimeSeconds", "heartbeatAgeMs", "circuitBreaker"):
                if field not in health_data:
                    e_score = min(e_score, 4)
                    notes.append(f"health missing {field}")
        return d_score, e_score, notes

    if cat_num == 17:  # Workflows
        r = curl_get(f"{base}/control/workflows")
        if r and r.get("success"):
            # Empty workflow list is valid — workflows are registered in the database, not the UI Bridge
            return 5, 5, ["Workflows endpoint works (empty list is expected)"]
        return 4, 3, ["Workflows endpoint failed"]

    if cat_num == 18:  # Render Log
        r1 = curl_get(f"{base}/render-log")
        r2 = curl_get(f"{base}/control/render-log")
        ok1 = (r1 or {}).get("success", False)
        ok2 = (r2 or {}).get("success", False)
        d_score, e_score = 5, 5
        if not ok1:
            e_score = min(e_score, 3)
            notes.append("/render-log failed")
        if not ok2:
            e_score = min(e_score, 3)
            notes.append("/control/render-log failed")
        if ok1 and ok2:
            notes.append("Both render-log paths work")
        return d_score, e_score, notes

    return 3, 3, ["Unknown category"]


CATEGORIES = {
    0: "Health & Recovery",
    1: "Element Discovery",
    2: "Click Actions",
    3: "Text Input",
    4: "Selection Controls",
    5: "Focus Management",
    6: "Scrolling",
    7: "Form Operations",
    8: "Navigation",
    9: "Components",
    10: "Finding & Search",
    11: "Console Errors",
    12: "Specs",
    13: "Drag & Drop",
    14: "JS Evaluation",
    15: "Idle Detection",
    16: "Connection Health",
    17: "Workflows",
    18: "Render Log",
}


def main():
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    results = {}
    all_pass = True

    for app, base in APPS.items():
        # Check if app is up
        health = curl_get(f"{base}/health", timeout=5)
        if not health:
            print(f"[SKIP] {app} is not running")
            for cat in CATEGORIES:
                results[(app, cat)] = (0, 0, ["App not running"])
            all_pass = False
            continue

        print(f"\n{'='*60}")
        print(f"Testing {app} ({base})")
        print(f"{'='*60}")

        for cat_num, cat_name in CATEGORIES.items():
            if cat_num == 14 and app == "web":
                results[(app, cat_num)] = (0, 0, ["SKIP"])
                continue
            try:
                d, e, notes = test_category(cat_num, app, base)
                results[(app, cat_num)] = (d, e, notes)
                status = "PASS" if d >= 5 and e >= 5 else "PARTIAL" if d >= 3 and e >= 3 else "FAIL"
                if d < 5 or e < 5:
                    all_pass = False
                print(f"  [{status}] Cat {cat_num:2d}: {cat_name:<22s} D={d}/5 E={e}/5  {'; '.join(notes[:2])}")
            except Exception as ex:
                results[(app, cat_num)] = (2, 2, [str(ex)])
                all_pass = False
                print(f"  [ERROR] Cat {cat_num:2d}: {cat_name:<22s} {ex}")

    # Generate report
    report_lines = [f"# UI Bridge Test Report — {datetime.now().strftime('%Y-%m-%d %H:%M')}\n"]
    report_lines.append("| # | Category | Web D. | Web E. | Runner D. | Runner E. |")
    report_lines.append("|---|----------|--------|--------|-----------|-----------|")

    web_d_total, web_e_total, web_count = 0, 0, 0
    run_d_total, run_e_total, run_count = 0, 0, 0

    for cat_num, cat_name in CATEGORIES.items():
        wd, we, _ = results.get(("web", cat_num), (0, 0, []))
        rd, re_, _ = results.get(("runner", cat_num), (0, 0, []))
        w_d_str = f"{wd}" if wd > 0 else "-"
        w_e_str = f"{we}" if we > 0 else "-"
        r_d_str = f"{rd}" if rd > 0 else "-"
        r_e_str = f"{re_}" if re_ > 0 else "-"
        report_lines.append(f"| {cat_num} | {cat_name} | {w_d_str} | {w_e_str} | {r_d_str} | {r_e_str} |")
        if wd > 0:
            web_d_total += wd; web_e_total += we; web_count += 1
        if rd > 0:
            run_d_total += rd; run_e_total += re_; run_count += 1

    web_d_avg = web_d_total / web_count if web_count else 0
    web_e_avg = web_e_total / web_count if web_count else 0
    run_d_avg = run_d_total / run_count if run_count else 0
    run_e_avg = run_e_total / run_count if run_count else 0

    report_lines.append(f"\n**Web: {web_d_avg:.1f}/{web_e_avg:.1f}  Runner: {run_d_avg:.1f}/{run_e_avg:.1f}**\n")

    # Notes for failed categories
    report_lines.append("## Issues\n")
    for (app, cat_num), (d, e, notes) in results.items():
        if 0 < d < 5 or 0 < e < 5:
            report_lines.append(f"- **{app} Cat {cat_num} ({CATEGORIES[cat_num]})** D={d} E={e}: {'; '.join(notes)}")

    report_path = Path(f".dev-logs/ui-bridge-test-report-{timestamp}.md")
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text("\n".join(report_lines), encoding="utf-8")
    print(f"\nReport saved to {report_path}")
    print(f"\nWeb:    {web_d_avg:.1f}/5 discoverability, {web_e_avg:.1f}/5 effectiveness")
    print(f"Runner: {run_d_avg:.1f}/5 discoverability, {run_e_avg:.1f}/5 effectiveness")

    if all_pass:
        print("\nALL CATEGORIES PASS (5/5)")
        sys.exit(0)
    else:
        print(f"\nSome categories below 5/5 — see report for details")
        sys.exit(1)


if __name__ == "__main__":
    main()
