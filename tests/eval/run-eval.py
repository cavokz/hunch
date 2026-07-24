#!/usr/bin/env python3
"""Run foray eval scenarios against models via opencode.

Usage: run-eval.py <model> [model ...]
Example: run-eval.py github-copilot/claude-sonnet-4.6 github-copilot/claude-haiku-4.5
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime
from pathlib import Path

import checker

SCENARIOS_DIR = Path(__file__).resolve().parent / "scenarios"

_print_lock = threading.Lock()


def _print(*args, **kwargs):
    with _print_lock:
        print(*args, **kwargs)


def load_scenario(path: Path) -> dict:
    try:
        import tomllib
    except ModuleNotFoundError:
        import tomli as tomllib
    with open(path, "rb") as f:
        return tomllib.load(f)


def format_prompt(scenario: dict) -> str:
    return f"Journal: {scenario['journal']}\nPrompt: {scenario['prompt']}"


def run_opencode(model: str, prompt: str, env: dict) -> subprocess.CompletedProcess:
    result = subprocess.run(
        ["opencode", "run", "--pure", "--model", model, "--format", "json", prompt],
        capture_output=True,
        text=True,
        env=env,
    )
    if result.returncode == -signal.SIGINT:
        raise KeyboardInterrupt
    return result


def run_model(model: str, timestamp: str, env: dict) -> tuple[str, list[tuple[str, bool]]]:
    model_slug = model.replace("/", "-")
    results_file = Path(f"eval-results-{timestamp}-{model_slug}.txt")

    results: list[tuple[str, bool]] = []

    for scenario_path in sorted(SCENARIOS_DIR.glob("*.toml")):
        scenario = load_scenario(scenario_path)
        name = scenario_path.stem

        result = run_opencode(model, format_prompt(scenario), env)

        passed, failures, tool_calls, _ = checker.run_checks(
            result.stdout,
            name,
            scenario,
            result.returncode,
        )

        with open(results_file, "a") as rf:
            rf.write(f"---\n{name}\n{json.dumps(scenario, indent=2)}\n")
            rf.write(f"checks: {'PASS' if passed else 'FAIL'}\n")
            for f in failures:
                rf.write(f"  - {f}\n")
            rf.write(f"tool_calls: {len(tool_calls)}\n")
            rf.write(result.stdout + result.stderr + "\n")

        symbol = "\033[32m\u2713\033[0m" if passed else "\033[31m\u2717\033[0m"
        detail = ""
        if failures:
            detail = f" — {failures[0]}"
        _print(f" {symbol} {model} / {name}  ({len(tool_calls)} calls){detail}", flush=True)
        results.append((name, passed))

    passed_count = sum(1 for _, p in results if p)
    total = len(results)
    _print(f"  {model}  {passed_count}/{total} passed  ({results_file})", flush=True)

    return model, results


def main() -> int:
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <model> [model ...]", file=sys.stderr)
        print(f"Example: {sys.argv[0]} github-copilot/claude-sonnet-4.6", file=sys.stderr)
        return 1

    models = sys.argv[1:]
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")

    env = os.environ.copy()
    env["OPENCODE_CONFIG_CONTENT"] = json.dumps(
        {
            "mcp": {
                "foray": {"enabled": False},
                "foray-dev": {"enabled": False},
                "foray-eval": {"enabled": True},
            },
        }
    )

    tmp_data_dir = tempfile.mkdtemp(prefix="opencode-eval-")
    env["XDG_DATA_HOME"] = tmp_data_dir

    all_results: list[tuple[str, list[tuple[str, bool]]]] = []

    try:
        with ThreadPoolExecutor(max_workers=len(models)) as pool:
            futures = {pool.submit(run_model, m, timestamp, env): m for m in models}
            for future in as_completed(futures):
                model, results = future.result()
                all_results.append((model, results))

        if len(models) > 1:
            _print("\n=== SUMMARY ===", flush=True)
            for model, results in all_results:
                passed_count = sum(1 for _, p in results if p)
                total = len(results)
                _print(f"  {model}: {passed_count}/{total}", flush=True)

        any_fail = any(not p for _, results in all_results for _, p in results)
        return 1 if any_fail else 0
    finally:
        shutil.rmtree(tmp_data_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
