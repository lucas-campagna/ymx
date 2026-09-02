## Testing

Tests are **first-class**: every scenario lives in `tests/cases/rule-NN/<scenario>/` as a real YMX project (one directory = project root), and assertions are written inside the YAML itself via the `_test` meta key (see [Project metadata](06-project-metadata.md)) — no hand-written expected-output files and no external snapshot tooling. Both success expectations (`Expected::Value`) and compile-time diagnostic expectations (`Expected::Error`) are expressed in `_test`. The test harness is a small Rust integration test that, for each scenario directory:

1. `ymx_lib::load_project(scenario_dir)` → `Project` (collecting raw `_ymx`/`_test` meta).
2. `ymx_config::extract_options(&project, &CliOverrides::default_for_tests())` → `Options` (front-matter defaults override the engine defaults; the harness sets no CLI overrides unless the scenario requires them).
3. `ymx_test::run_tests(&project, &opts)` → `Vec<TestResult>`; the harness asserts every test `passed`, printing the failing target + expected/actual diff on failure.

**Scenario layout.**

```
tests/cases/rule-NN/<scenario>.yml   # one file = one scenario = one project root
```

- Every scenario must define at least one `_test` entry. A scenario asserts either a value (`Expected::Value`) or a diagnostic (`Expected::Error`); the `error` variant may assert codes that arise **after** a successful `load_project` — option-resolution (`E009`, the unknown/invalid-`_ymx`-field part of `E010`) and target-compilation (`E002`, `E003`, `E005`, `E006`, `E008`, the call-site / string-escape / math-identifier / mixed-shape-chain parts of `E010`, `E011`, `E012`, `E013`). Load-time codes (`E001`, `E004`, `E007`, `E015`) are not `_test`-driveable because `load_project` is all-or-nothing (see [Reach of the error variant](06-project-metadata.md#_test--inline-tests)).
- The `_test._build_error: <code>` key asserts that `load_project` **or** `extract_options` fails with the given diagnostic code — a matching diagnostic is a PASS, a mismatch or unexpected success is a FAIL. This makes `E009` (entry not found), the invalid-`_ymx`-field part of `E010`, and all other load/option-time codes `_test`-driveable without silently skipping them. The shape mirrors `Expected::Error` from regular `_test` assertions; when `_build_error` is set, no `result`/`error` assertions may appear in the same `_test` block.
- The only diagnostics that are **not** `_test`-driveable by construction are produced by parsing the `_test` block itself (the malformed-`_test`-block part of `E010`) and YAML-parse failures (`E001`) of the document that hosts the `_test` block — both yield an unreadable `_test`. Together with the other load-time codes (`E004`, `E007`, `E015`) they are exercised by ordinary crate `#[test]` unit tests with inline YAML snippets. The test crate `ymx-test` exposes enough of `parse_tests`/`run_tests` to drive these where convenient.
- `_ymx` in a scenario's entry document sets non-default flags the rule needs (e.g. `max_depth` for an `E008` case, a custom `from_keyword` for rule 6 keyword-override scenarios, or `plain: template` / `plain: true` for namespace-promotion scenarios).
- Multi-file / namespace / file-scope scenarios are no longer supported in the flat layout; scenarios that require multiple documents use `_use` within the single file or are tested via crate `#[test]` unit tests with inline YAML.
- Scenarios that exercise `_use` transitive re-export use the subdirectory layout (e.g. `use/transitive/`) with an entry file, intermediate file, and leaf file — the entry's `_use` names an intermediate file's `_use`-imported component, verifying the re-export chain.
- Rule-21 (IPC) scenarios use `cat` or coreutils as the backend process (always available, deterministic). Python or other interpreter scenarios should be gated on availability and must not be required for a green CI run. IPC scenarios must provide an `IpcHost` implementation via `Options.ipc`; scenarios without a host skip with `E018`.
