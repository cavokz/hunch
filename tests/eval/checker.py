"""Mechanical verifier for foray eval scenarios.

Parses the JSON event stream from `opencode run --format json` and runs
checks declared in each scenario's [checks] TOML section against the
tool-call trace.
"""

from __future__ import annotations

import json


def parse_events(stdout: str) -> tuple[list[dict], list[str], list[str]]:
    """Parse JSON event stream. Returns (tool_calls, text_responses, bad_lines)."""
    tool_calls = []
    texts = []
    bad_lines = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            bad_lines.append(line)
            continue
        if ev.get("type") == "tool_use":
            part = ev.get("part", {})
            if part.get("type") == "tool":
                tool = part.get("tool")
                if not tool:
                    continue
                state = part.get("state", {})
                if state.get("status") == "completed":
                    inp = state.get("input", {})
                    out = state.get("output", "")
                    if isinstance(inp, str):
                        try:
                            inp = json.loads(inp)
                        except (json.JSONDecodeError, TypeError):
                            pass
                    if isinstance(out, str):
                        try:
                            out = json.loads(out)
                        except (json.JSONDecodeError, TypeError):
                            pass
                    tool_calls.append(
                        {
                            "tool": tool,
                            "input": inp if isinstance(inp, dict) else {},
                            "output": out if isinstance(out, dict) else {},
                        }
                    )
        elif ev.get("type") == "text":
            text = ev.get("text", "")
            if text.strip():
                texts.append(text.strip())
    return tool_calls, texts, bad_lines


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _tool_name(name: str) -> str:
    """Strip foray-eval_ prefix (e.g. foray-eval_sync_journal → sync_journal)."""
    if name.startswith("foray-eval_"):
        return name[len("foray-eval_") :]
    return name


def _sync_calls(calls: list[dict], journal: str | None = None) -> list[dict]:
    """Return sync_journal calls, optionally filtered by journal name."""
    return [
        c
        for c in calls
        if _tool_name(c["tool"]) == "sync_journal" and (journal is None or c["input"].get("name") == journal)
    ]


# ---------------------------------------------------------------------------
# Check functions — each returns a (possibly empty) list of failure strings
# ---------------------------------------------------------------------------


def check_tool_errors(calls: list[dict]) -> list[str]:
    """Detect tool calls that returned an error in their output."""
    return [
        f"Tool '{c['tool']}' returned error: {c['output']['error']}"
        for c in calls
        if isinstance(c.get("output"), dict) and c["output"].get("error")
    ]


def check_protocol_order(calls: list[dict]) -> list[str]:
    """Protocol checks: hello before list_journals, list_journals before sync_journal."""
    fails = []
    hello_indices = [i for i, c in enumerate(calls) if _tool_name(c["tool"]) == "hello"]
    list_indices = [i for i, c in enumerate(calls) if _tool_name(c["tool"]) == "list_journals"]
    sync_indices = [i for i, c in enumerate(calls) if _tool_name(c["tool"]) == "sync_journal"]

    if list_indices:
        if not hello_indices:
            fails.append("hello never called before list_journals")
        elif list_indices[0] < hello_indices[0]:
            fails.append("list_journals called before hello")

    if sync_indices:
        if not list_indices:
            fails.append("list_journals never called before sync_journal")
        elif sync_indices[0] < list_indices[0]:
            fails.append("sync_journal called before list_journals")

    return fails


def check_all_syncs_use_journal(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify every sync_journal call targets the expected journal."""
    if not value:
        return []
    journal = scenario["journal"]
    fails = []
    for i, c in enumerate(calls):
        if _tool_name(c["tool"]) == "sync_journal":
            name = c["input"].get("name", "")
            if name != journal:
                fails.append(f"sync_journal[{i}] name='{name}', expected '{journal}'")
    return fails


def check_first_sync_uses_journal(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify the first sync_journal call targets the expected journal."""
    if not value:
        return []
    journal = scenario["journal"]
    for c in calls:
        if _tool_name(c["tool"]) == "sync_journal":
            name = c["input"].get("name", "")
            if name != journal:
                return [f"First sync_journal name='{name}', expected '{journal}'"]
            return []
    return ["No sync_journal calls found"]


def check_no_writes(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify no tool call creates or writes to journals."""
    if not value:
        return []
    fails = []
    for c in calls:
        tn = _tool_name(c["tool"])
        if tn == "create_journal":
            fails.append("unexpected create_journal call")
        if tn == "sync_journal" and c["input"].get("items"):
            fails.append("sync_journal called with items (write)")
    return fails


def check_archived_flag(calls: list[dict], expected: bool) -> list[str]:
    """Check every sync_journal call passes the correct archived flag."""
    fails = []
    for i, c in enumerate(calls):
        if _tool_name(c["tool"]) == "sync_journal":
            if "archived" not in c["input"]:
                fails.append(f"sync_journal[{i}] missing required archived parameter")
            elif c["input"]["archived"] != expected:
                fails.append(f"sync_journal[{i}] archived={c['input']['archived']}, expected {expected}")
    return fails


def check_from_tracking(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify from offsets advance by the number of items returned each page."""
    if not value:
        return []
    journal = scenario["journal"]
    fails = []
    syncs = _sync_calls(calls, journal)
    expected = 0
    for i, s in enumerate(syncs):
        actual = s["input"].get("from", -1)
        if actual != expected:
            fails.append(f"sync_journal[{i}] from={actual}, expected {expected}")
        expected += len(s["output"].get("items", []))
    return fails


def check_size_not_fallback(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify size is not the fallback 5 when avg_item_size is available.

    The companion skill says: if avg_item_size is absent, use size:5.
    If present, compute size = floor(budget / (avg + 2*std)).
    """
    if not value:
        return []
    journal = scenario["journal"]
    has_avg = any(
        j.get("name") == journal and "avg_item_size" in j
        for c in calls
        if _tool_name(c["tool"]) == "list_journals"
        for j in c["output"].get("journals", [])
    )
    if not has_avg:
        return []
    return [
        f"sync_journal[{i}] size=5 (fallback) but avg_item_size was available"
        for i, s in enumerate(_sync_calls(calls, journal))
        if s["input"].get("size") == 5
    ]


def check_min_sync_calls(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify at least `value` sync_journal calls were made."""
    if isinstance(value, bool):
        return ["Expected integer for min_sync_calls, got boolean"]
    journal = scenario["journal"]
    minimum = int(value)
    n = len(_sync_calls(calls, journal))
    if n < minimum:
        return [f"Expected >= {minimum} sync_journal calls, got {n}"]
    return []


def check_max_sync_calls(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify at most `value` sync_journal calls were made."""
    if isinstance(value, bool):
        return ["Expected integer for max_sync_calls, got boolean"]
    journal = scenario["journal"]
    maximum = int(value)
    n = len(_sync_calls(calls, journal))
    if n > maximum:
        return [f"Expected <= {maximum} sync_journal calls, got {n}"]
    return []


def check_exact_item_count(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify total items retrieved across all pages matches expected."""
    if isinstance(value, bool):
        return ["Expected integer for exact_item_count, got boolean"]
    journal = scenario["journal"]
    expected = int(value)
    syncs = _sync_calls(calls, journal)
    if not syncs:
        if expected == 0:
            return []
        return [f"No sync_journal calls for '{journal}'"]
    total = sum(len(s["output"].get("items", [])) for s in syncs)
    if total != expected:
        return [f"Retrieved {total} items, expected {expected}"]
    return []


def check_multiple_journals_read(calls: list[dict], scenario: dict, value: object) -> list[str]:
    """Verify at least `minimum` distinct journal names were read."""
    if isinstance(value, bool):
        return ["Expected integer for min_journals_read, got boolean"]
    minimum = int(value)
    names = {c["input"].get("name", "") for c in calls if _tool_name(c["tool"]) == "sync_journal"}
    names.discard("")
    if len(names) < minimum:
        return [f"Read {len(names)} distinct journal(s), expected >= {minimum}"]
    return []


# ---------------------------------------------------------------------------
# Check dispatch — maps [checks] TOML keys to (label, function)
# ---------------------------------------------------------------------------

_CHECKS: dict[str, tuple[str, callable]] = {
    "all_syncs_use_journal": ("all syncs use journal", check_all_syncs_use_journal),
    "first_sync_uses_journal": (
        "first sync uses journal",
        check_first_sync_uses_journal,
    ),
    "from_tracking": ("from offset tracking", check_from_tracking),
    "size_not_fallback": ("size not fallback", check_size_not_fallback),
    "no_writes": ("no writes", check_no_writes),
    "min_sync_calls": ("min sync calls", check_min_sync_calls),
    "max_sync_calls": ("max sync calls", check_max_sync_calls),
    "exact_item_count": ("exact item count", check_exact_item_count),
    "min_journals_read": ("min journals read", check_multiple_journals_read),
}


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def run_checks(
    stdout: str, scenario_name: str, scenario: dict, returncode: int = 0
) -> tuple[bool, list[str], list[dict], list[str]]:
    """Run all checks for a scenario.

    Checks are declared in scenario["checks"]. Protocol checks
    (tool_errors, call ordering, archived flag) always run.

    Returns (passed, failures, tool_calls, texts).
    """
    tool_calls, texts, bad_lines = parse_events(stdout)
    failures: list[str] = []

    if bad_lines:
        failures.append(f"{len(bad_lines)} non-JSON line(s) in event stream")

    # Protocol checks — always run
    failures.extend(check_tool_errors(tool_calls))
    failures.extend(check_protocol_order(tool_calls))
    failures.extend(check_archived_flag(tool_calls, scenario.get("archived", False)))

    if returncode != 0:
        failures.append(f"opencode run exited with code {returncode}")

    # Scenario-specific checks from [checks] TOML table
    checks = scenario.get("checks", {})

    for key, val in checks.items():
        if key not in _CHECKS:
            failures.append(f"Unknown check: {key}")
            continue

        label, fn = _CHECKS[key]
        try:
            failures.extend(fn(tool_calls, scenario, val))
        except Exception as e:
            failures.append(f"{label}: check raised {e!r}")

    return len(failures) == 0, failures, tool_calls, texts
