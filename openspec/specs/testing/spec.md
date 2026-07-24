# Testing Specification

## Purpose

Defines foray's testing infrastructure: the static journal fixture files used in
integration and model-eval tests, the model evaluation harness that runs behavioral
scenarios against a live model using the MCP tools, and the isolation mechanism that
prevents test runs from affecting real journal stores. Together these provide
repeatable, observable verification of both the foray binary and the companion skill's
behavioral compliance.

## Requirements

### Requirement: Static journal fixtures cover all pagination and migration edge cases

A set of static JSON journal files SHALL be committed under `tests/fixtures/journals/`.
These files SHALL NOT be mutated in-place; tests that require a writable copy SHALL
set `FORAY_HOME` to a temporary directory containing a copy of the fixture tree.
The fixture set SHALL include at minimum:

| File | Purpose |
|------|---------|
| `stats-empty.json` | 0 items — `list_journals` must omit `avg_item_size` and `std_item_size` |
| `stats-single.json` | 1 item — `avg_item_size` present, `std_item_size` is 0 |
| `stats-uniform.json` | 100 identical items — `std_item_size` near zero |
| `stats-high-variance.json` | 100 items alternating ~50 B / ~2 KB — large `std_item_size` |
| `stats-realistic.json` | 200 items across multiple templates — real-world size distribution |
| `schema-v0.json` | 5 items in pre-v1 format — migration must be transparent |
| `correction-trail.json` | 10 items with an append-only correction — no item may be deleted |
| `cross-reference.json` | 10 items with `foray:` cross-journal refs in `meta.ref` |
| `archive/stats-high-variance-archived.json` | Same content as `stats-high-variance`; in archived location |

#### Scenario: Fixtures are read-only in tests
- **WHEN** a test reads from `tests/fixtures/journals/`
- **THEN** the fixture file on disk is unchanged after the test completes

#### Scenario: Writable test copies use FORAY_HOME
- **WHEN** a test needs to write to a fixture journal
- **THEN** `FORAY_HOME` is set to a temporary directory containing a copy of the fixture

### Requirement: stats-empty fixture produces absent size statistics

The `stats-empty.json` fixture SHALL contain a valid journal with zero items. When
`list_journals` is called against this fixture, the entry for that journal SHALL omit
both `avg_item_size` and `std_item_size`.

#### Scenario: list_journals for empty fixture omits size fields
- **WHEN** `list_journals` is called with `FORAY_HOME` pointing at the fixture directory
- **THEN** the entry for `stats-empty` has no `avg_item_size` or `std_item_size`

### Requirement: stats-single fixture produces zero standard deviation

The `stats-single.json` fixture SHALL contain exactly one item. When `list_journals`
is called, the entry SHALL include `avg_item_size` (the size of that one item) and
`std_item_size` SHALL be 0.

#### Scenario: list_journals for single-item fixture has std of zero
- **WHEN** `list_journals` is called against `stats-single`
- **THEN** `avg_item_size` is present and `std_item_size` is 0

### Requirement: schema-v0 fixture is readable after transparent migration

The `schema-v0.json` fixture SHALL have no `schema` field and SHALL contain items with
a top-level `ref` field (pre-v1 format). Reading it via `sync_journal` SHALL succeed
and SHALL return items with `meta.ref` set correctly. The file on disk SHALL remain
unmodified until a write occurs.

#### Scenario: schema-v0 fixture is read without error
- **WHEN** `sync_journal` is called against the `schema-v0` fixture
- **THEN** items are returned successfully and `meta.ref` contains the migrated reference

### Requirement: archived fixture requires archived:true on sync_journal

The `archive/stats-high-variance-archived.json` fixture SHALL be in the archived
location. Any `sync_journal` call against it MUST pass `archived: true`; calls with
`archived: false` SHALL return a not-found error.

#### Scenario: Archived fixture requires archived:true
- **WHEN** `sync_journal` is called with `archived: false` against the archived fixture
- **THEN** a not-found error is returned

#### Scenario: Archived fixture is readable with archived:true
- **WHEN** `sync_journal` is called with `archived: true` against the archived fixture
- **THEN** items are returned successfully

### Requirement: Model eval scenarios are declared in TOML files under tests/eval/scenarios/

Each evaluation scenario SHALL be a TOML file under `tests/eval/scenarios/`. Each file
SHALL declare: `journal` (the fixture journal name), `archived` (whether the journal is
archived), `prompt` (the prompt given to the model), `[checks]` (mechanical boolean or
integer checks), and `[[expected_behaviors]]` (plain-English observable behaviors for
manual review). Universal checks SHALL always run: no tool errors, correct `hello`→
`list_journals` call order, correct `archived` flag on all `sync_journal` calls.

The eval suite SHALL include at minimum these scenarios:

| Scenario | Journal | What it tests |
|----------|---------|---------------|
| `pagination-uniform` | `stats-uniform` | Page size from formula when std ≈ 0 |
| `pagination-high-variance` | `stats-high-variance` | Conservative page size when std is large |
| `pagination-realistic` | `stats-realistic` | Offset tracking across 200-item multi-page read |
| `archived-read` | `stats-high-variance-archived` | `archived: true` on all sync calls |
| `cross-reference` | `cross-reference` | Follow `foray:` refs to open referenced journals |
| `schema-migration` | `schema-v0` | Pre-v1 journal is readable; migration is transparent |
| `empty-journal` | `stats-empty` | Absent stats → model uses safe default size of 5 |
| `single-item` | `stats-single` | `std_item_size` of 0 → size = floor(budget / avg) |

#### Scenario: Scenario file is valid TOML with required fields
- **WHEN** a scenario file is loaded by the eval runner
- **THEN** it contains `journal`, `archived`, `prompt`, `[checks]`, and at least one `[[expected_behaviors]]` entry

#### Scenario: Protocol checks run on every scenario
- **WHEN** any scenario is evaluated
- **THEN** the checker verifies: no tool errors, `hello` called before `list_journals`, `list_journals` called before `sync_journal`, correct `archived` flag on all `sync_journal` calls

### Requirement: The eval runner invokes one or more models and captures tool-call traces

The eval runner (`tests/eval/run-eval.py`) accepts one or more model names as positional
arguments and runs all scenarios against each model, using `ThreadPoolExecutor` for
parallel execution. For each model it SHALL invoke
`opencode run --pure --model <model> --format json` with each scenario's prompt,
capturing the structured event stream. The runner SHALL set `OPENCODE_CONFIG_CONTENT`
to activate only the `foray-eval` MCP server (which points `FORAY_HOME` at
`tests/fixtures`) and disable all other foray servers (`foray` and `foray-dev`),
preventing tool-name collisions. The runner SHALL set `XDG_DATA_HOME` to a fresh
temporary directory so each eval run gets a clean opencode session database; the
directory SHALL be removed in a `finally` block after the run completes. `--pure`
disables external terminal plugins that would interleave non-JSON output in the event
stream. Results for each model are written to a timestamped file
`eval-results-{timestamp}-{model_slug}.txt`.

#### Scenario: Only foray-eval MCP server is active during eval
- **WHEN** a scenario is run by the eval runner
- **THEN** only the `foray-eval` server is reachable; `foray` and `foray-dev` servers are inactive

#### Scenario: Runner captures a structured event stream
- **WHEN** `opencode run --pure --model <model> --format json` completes
- **THEN** the runner receives a parseable JSON event stream containing tool call records

#### Scenario: Results written per model
- **WHEN** the runner completes scenarios for a given model
- **THEN** a timestamped result file for that model is written to the working directory

### Requirement: The checker mechanically verifies tool-call traces against declared checks

The checker (`tests/eval/checker.py`) SHALL parse the event stream produced by the eval
runner and verify each declared check in the scenario's `[checks]` section. Boolean
checks verify presence or absence of a condition. Integer checks verify that a count
meets a threshold (e.g. `min_sync_calls`, `max_sync_calls`). The checker SHALL report
pass/fail per check and an overall pass/fail for the scenario.

#### Scenario: Failing check produces a clear report
- **WHEN** the model makes more `sync_journal` calls than `max_sync_calls` allows
- **THEN** the checker reports the `max_sync_calls` check as failed with the actual count

#### Scenario: All checks passing produces overall pass
- **WHEN** all declared checks pass for a scenario
- **THEN** the checker reports the scenario as passed
