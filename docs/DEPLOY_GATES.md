# Deploy gates: cost-carrying binaries + spec-carrying artifacts

Two deploy-time gates, both fail-closed (a refusal is exit 1, like a type
error — never a warning that deploys anyway). Implemented 2026-11-18
(55-exec Batch 6: 55-ideas D1/#34 and B10/#21).

## 1. Cost-carrying binaries (`cuni cost`)

The compiler emits, alongside the binary, a per-function cost manifest
measured on reference hardware; the deploy checker compares the manifest
against a budget file and FAILS the deploy on breach. Costs are re-measured
on every rebuild — a manifest is always produced by a fresh profiling run,
never cached.

### Measure

```sh
cuni cost <file.cuni> [--out manifest.json] [--reps N] [--tdp-watts W]
```

* `--reps N` (default 5): profiled runs after 1 discarded warmup run.
* `--tdp-watts W`: model energy as wall time × TDP. Without it, energy is
  `null` (unmodeled) — never invented.
* Default `--out` is `<stem>.cost.json` beside the source.
* A failing program produces NO manifest.

### Manifest format (`cuni-cost-manifest/1`)

```json
{
  "format": "cuni-cost-manifest/1",
  "cuni_version": "0.10.1",
  "source_path": "settle.cuni",
  "source_sha256": "<sha256 of the source at measure time>",
  "measured_at": "2026-11-18T03:00:00Z",
  "measured_seat": "interp",
  "reps": 5,
  "reference": {
    "id": "linux-x86_64-amd-epyc-...", "os": "linux", "arch": "x86_64",
    "cpu_model": "AMD EPYC ...", "cpu_count": 8,
    "mem_total_kib": 6234700, "rustc_version": "rustc 1.99.1 ..."
  },
  "energy_method": "modeled: wall_time x tdp_w (tdp_w=65)",
  "tdp_watts": 65.0,
  "totals": {"wall_us": 12345, "peak_rss_bytes": 0, "io_bytes": 12, "energy_mj_total": 0.8},
  "functions": [
    {"name": "fee", "calls": 10, "exercised": true,
     "time_us": {"mean": 12, "p50": 11, "p95": 18, "max": 25},
     "memory_bytes_peak": 0, "io_bytes": 0,
     "energy_mj": {"mean": 0.001, "p50": 0.001, "p95": 0.001, "max": 0.002},
     "energy_mj_total": 0.01}
  ]
}
```

Per-call statistics are inclusive wall time (callees included),
nearest-rank percentiles over all sampled calls pooled across reps.
Every defined function gets a row, even one the run never called
(`exercised: false`, time/energy null) — the manifest cannot silently omit
a function. Top-level statements are the synthetic `<top>` row.

### What each metric is (honest labels)

| Metric | Method |
|---|---|
| `time_us` | Measured: `Instant` around each call in the reference interpreter. |
| `memory_bytes_peak` | Measured: max RSS delta per call, Linux `/proc/self/statm`, page-granular (a call allocating under one page reads 0). |
| `io_bytes` | Measured: stdout bytes written while the call was innermost. |
| `energy_mj` | MODELLED: wall time × declared TDP. `null` without `--tdp-watts`. |
| `reference` | The machine the run happened on, recorded verbatim. |

The profiling seat is the compiler's own reference interpreter
(`measured_seat: "interp"`). Per-seat costs of emitted artifacts are not
measured in v1; the manifest says so rather than implying otherwise.

### Budget format (`cuni-cost-budget/1`)

```json
{
  "format": "cuni-cost-budget/1",
  "reference": "linux-x86_64-amd-epyc-...",
  "defaults": {"time_us_p95_max": 100000, "time_us_max_max": 1000000,
               "memory_bytes_peak_max": 16777216, "io_bytes_max": 1048576,
               "energy_mj_p95_max": null},
  "per_function": {"settle": {"time_us_p95_max": 50000}},
  "totals": {"wall_us_max": 5000000, "peak_rss_bytes_max": 268435456,
             "io_bytes_max": 1048576, "energy_mj_total_max": null}
}
```

* `reference` (optional): if set, the manifest's `reference.id` must match
  exactly — budgets are anchored to the hardware they were written for.
* A limit applies per function: `per_function[name]` wins, else `defaults`.
  Unset = no limit.
* `cuni cost --init-budget [--out budget.json]` writes a template
  (defaults + totals; `per_function` empty — add entries only for
  functions your program defines, since an unknown budget function FAILs
  the gate by design).

### Deploy gate

```sh
cuni cost --check <manifest> --budget <budget> --source <file.cuni>
```

Exit 0 `cost gate: PASS` / exit 1 `cost gate: FAIL` with each breach on its
own line (`BREACH fn \`burn\`: time p95 123456µs > budget 1000µs`).
Fail-closed cases, all FAIL (not warnings):

* **Stale manifest** — source sha256 no longer matches: re-measure.
* **Unknown budget function** — the budget names a function neither defined
  in `--source` nor in the manifest (a typo'd budget must not silently
  apply to nothing).
* **Unverifiable energy** — the budget limits energy but the manifest has
  no energy measurement (`CANNOT VERIFY ... re-measure with --tdp-watts`).
* **Pinned-reference mismatch** — manifest measured on different hardware
  than the budget pins.
* A function the profiling run never exercised is reported as a note, not
  checked — a deploy that never ran a function cannot smuggle it past the
  gate unnoticed.

### Time-to-detect (measured vs assumed)

Measured 2026-11-18 (test `cost_gate_time_to_detect`,
`tests/cost_manifest.rs`): the deploy gate catches an injected regression
in milliseconds — the wall time of one `cuni cost --check` invocation
(printed by the test). The production-monitoring comparison floor (60s) is
a documented ASSUMPTION — the minimum detection delay of a scrape-interval
+ alerting pipeline — not a measured fact. The test asserts the measured
gate latency sits below that floor.

## 2. Spec-carrying artifacts (`cuni bundle` / `cuni admit`)

The deployable artifact carries its formal spec and proof inside it,
signed; the admission controller re-verifies the proof against live
environment state at deploy time and refuses spec-less artifacts. A spec
edit or binary change invalidates the proof.

### Build a bundle

```sh
cuni audit --gen-key <name>            # once: mints <name>.key (600) + <name>.pub
cuni bundle <file.cuni> --spec spec.json --out DIR [--seat rs]
            [--sign keyfile] [--timeout secs]
```

`--sign` is REQUIRED: an unsigned bundle cannot exist (admission would
refuse it). Default seat `rs`, default timeout 30s. Bundle creation
re-verifies before writing anything: the artifact must run and print
byte-identical stdout to the reference interpreter, the spec's
`behavior.stdout_sha256` (if set) must match, and `max_wall_ms` (if set)
must hold. On any failure the output directory is removed — no half-bundle
can be admitted later. Refuses to overwrite a non-empty output directory.

### Bundle layout

```text
<name>.cunibundle/
  manifest.json   — cuni-bundle/1: name, seat, source hash, sha256 of every other file
  source.cuni     — the source the artifact was built from
  artifact.<ext>  — the emitted deployable artifact (one seat)
  spec.json       — machine-readable spec (cuni-spec/1), strict schema
  proof.json      — cuni-proof/1: hashes binding binary+spec+source,
                    checks run at bundle time, gold stdout hash,
                    Ed25519 signature over the canonical proof bytes
```

### Spec schema (`cuni-spec/1`, strict — unknown keys refused)

```json
{
  "format": "cuni-spec/1",
  "name": "hello-bundle",
  "version": "1.0.0",
  "entry": "double",
  "seats": ["rs"],
  "env_assumptions": {
    "os": "linux", "arch": "x86_64",
    "toolchains": {"rs": ">=1.75"}
  },
  "behavior": {"stdout_sha256": "<64 hex, optional>", "exit_code": 0},
  "resource": {"max_wall_ms": 60000}
}
```

* `entry` must be a defined function or `"<top>"`; `seats` must contain
  the bundle seat.
* Toolchain constraints: `>=x.y`, `>x.y`, `<=x.y`, `<x.y`, `=x.y.z`,
  or bare `x.y` (means `>=`). Only the seats in the toolchain table
  (`src/bundle.rs`, `toolchain_binary`) can be version-checked; naming any
  other seat refuses the spec (admission only admits what it can verify).
* `resource` currently supports `max_wall_ms` only; the wall clock covers
  compile + run of the artifact.

### Proof (`cuni-proof/1`)

Binds `binary_sha256` + `spec_sha256` + `source_sha256`, the bundle-time
checks (source loads, interpreter run, artifact run matches interpreter,
spec behavior hash, spec wall budget), `gold_stdout_sha256`, and the
Ed25519 signature (canonical bytes = proof with `signature` removed,
pubkey included — same pattern as `audit::sign_receipt`).

### Admission

```sh
cuni admit <dir> [--env env.json] [--timeout secs]
```

Exit 0 `ADMIT` / exit 1 `REFUSE` with every reason listed / exit 2 usage
error. Admission re-verifies, in order:

1. `manifest.json` exists, parses, right format.
2. `spec.json` exists — **spec-less artifacts are refused, full stop**.
3. Every manifest-listed file exists with the recorded sha256 (tampered
   binary/spec/source → refuse).
4. Proof hashes match the live files (a spec edit or binary change after
   bundling invalidates the proof).
5. Ed25519 signature present and verifies.
6. Spec validates against the strict schema.
7. Environment assumptions hold live: os/arch match, toolchain binaries
   exist with satisfying versions (read live via `--version`).
8. Live re-verification: the bundled source still loads (parse +
   typecheck), the reference interpreter still prints the gold stdout
   hash, and the artifact — run fresh from a scratch copy — still prints
   the gold stdout hash, matches the interpreter, meets
   `behavior.stdout_sha256` (if set) and `max_wall_ms` (if set).

`--env env.json` (`{"os","arch","toolchains": {"rs": "1.75.0"}}`) supplies
environment facts instead of live detection, for simulation/testing.

### What admission does NOT do (v1 limits, stated)

* One seat per bundle (`--seat`); multi-seat bundles are future work.
* Artifact re-runs use the seat's own toolchain (`rustc`, `python3`,
  `node`, ...); a missing toolchain refuses admission rather than
  skipping the re-run.
* No network or registry checks; the bundle is self-contained.
