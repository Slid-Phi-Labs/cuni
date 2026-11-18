//! Black-box tests for `cuni cost` — cost-carrying binaries (55-ideas D1 / #34).
//!
//! Every test really measures and really gates: the manifest is produced by
//! a fresh profiling run, the deploy gate compares it against a budget file,
//! and the regression test injects a genuinely slow function.
//!
//! Detection-latency note: the gate's latency is measured (wall time of
//! `cuni cost --check`). The production-monitoring floor it is compared
//! against (60s) is a documented ASSUMPTION — the minimum detection delay
//! of a scrape-interval + alerting pipeline — stated in
//! docs/DEPLOY_GATES.md, not a measured fact. The test asserts the measured
//! gate latency is below that floor.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn cuni_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cuni"))
}

/// `rustc` (used by nothing here — the cost seat is the interpreter) and
/// the general toolchain dirs, for the child process. Mirrors
/// tests/proof_decimal.rs.
fn toolchain_path() -> String {
    let home = std::env::var("HOME").expect("HOME must be set to locate toolchains");
    let extra = [
        format!("{home}/go/bin"),
        format!("{home}/toolchains/go/bin"),
        format!("{home}/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin"),
        format!("{home}/toolchains/bin"),
    ];
    let cur = std::env::var("PATH").unwrap_or_default();
    format!("{}:{cur}", extra.join(":"))
}

fn scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "cuni_cost_{}_{}_{}_{:?}",
        tag,
        std::process::id(),
        n,
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp work dir");
    dir
}

fn run_cuni(args: &[&str], dir: &Path) -> (bool, String, String) {
    let output = Command::new(cuni_bin())
        .args(args)
        .env("PATH", toolchain_path())
        .current_dir(dir)
        .output()
        .expect("failed to spawn cuni binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn fixture(name: &str) -> PathBuf {
    let dir = scratch_dir("fixture");
    let src = std::fs::read_to_string(format!("tests/fixtures-cost/{name}"))
        .unwrap_or_else(|e| panic!("missing fixture tests/fixtures-cost/{name}: {e}"));
    let path = dir.join(name);
    std::fs::write(&path, src).expect("write fixture");
    dir.join(name)
}

fn write_file(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

const GENEROUS_BUDGET: &str = r#"{
  "format": "cuni-cost-budget/1",
  "reference": null,
  "defaults": {"time_us_p95_max": 1000000, "time_us_max_max": 10000000, "memory_bytes_peak_max": 268435456, "io_bytes_max": 1048576, "energy_mj_p95_max": null},
  "per_function": {},
  "totals": {"wall_us_max": 60000000, "peak_rss_bytes_max": 1073741824, "io_bytes_max": 1048576, "energy_mj_total_max": null}
}"#;

const TIGHT_BUDGET: &str = r#"{
  "format": "cuni-cost-budget/1",
  "reference": null,
  "defaults": {"time_us_p95_max": 1000, "time_us_max_max": 10000, "memory_bytes_peak_max": 268435456, "io_bytes_max": 1048576, "energy_mj_p95_max": null},
  "per_function": {},
  "totals": null
}"#;

/// The profiling run really measures: the manifest names the functions,
/// carries the source hash, and records the reference hardware id.
#[test]
fn cost_measure_writes_manifest() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "3",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed:\nstdout: {stdout}\nstderr: {stderr}");
    let text = std::fs::read_to_string(&manifest).expect("manifest written");
    let v: serde_json::Value = serde_json::from_str(&text).expect("manifest is valid JSON");
    assert_eq!(v["format"], "cuni-cost-manifest/1");
    assert_eq!(v["measured_seat"], "interp");
    assert_eq!(v["reps"], 3);
    assert!(v["reference"]["id"].as_str().unwrap().starts_with("linux-"));
    assert_eq!(v["source_sha256"].as_str().unwrap().len(), 64);
    let names: Vec<&str> = v["functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"add"), "manifest must list add: {names:?}");
    assert!(names.contains(&"mul"), "manifest must list mul: {names:?}");
    assert!(names.contains(&"<top>"), "manifest must list <top>: {names:?}");
    let add = v["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "add")
        .unwrap();
    assert_eq!(add["calls"], 6, "add called 2x per rep x 3 reps");
    assert_eq!(add["exercised"], true);
    assert!(add["time_us"]["p95"].as_u64().unwrap() > 0, "p95 must be measured");
    assert_eq!(add["energy_mj_total"], serde_json::Value::Null,
        "no TDP supplied: energy must be null (unmodeled), never invented");
}

/// The `--init-budget` template must itself pass the gate for an ordinary
/// program: it ships defaults + totals only, no per-function entries naming
/// functions the program may not define (an unknown budget function is a
/// fail-closed FAIL by design).
#[test]
fn cost_init_budget_template_passes() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let budget = dir.join("budget.json");
    let (ok, _, stderr) = run_cuni(&["cost", "--init-budget", "--out", budget.to_str().unwrap()], &dir);
    assert!(ok, "--init-budget must succeed: {stderr}");
    let (ok, _, stderr) = run_cuni(
        &["cost", src.to_str().unwrap(), "--out", manifest.to_str().unwrap(), "--reps", "2"],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &["cost", "--check", manifest.to_str().unwrap(), "--budget", budget.to_str().unwrap(), "--source", src.to_str().unwrap()],
        &dir,
    );
    assert!(ok, "template budget must pass for fast.cuni:\nstdout: {stdout}\nstderr: {stderr}");
}

/// A within-budget release passes the deploy gate.
#[test]
fn cost_gate_passes_within_budget() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let budget = dir.join("budget.json");
    write_file(&budget, GENEROUS_BUDGET);
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "3",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(ok, "within-budget release must pass:\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("cost gate: PASS"), "gate must print PASS: {stdout}");
}

/// An injected performance regression fails the deploy gate, naming the
/// function and the breached limit.
#[test]
fn cost_gate_fails_on_injected_regression() {
    let src = fixture("slow.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("slow.cost.json");
    let budget = dir.join("budget.json");
    write_file(&budget, TIGHT_BUDGET);
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!ok, "injected regression must fail the gate:\nstdout: {stdout}");
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("BREACH fn `burn`"),
        "gate must name the regressed function:\n{combined}"
    );
    assert!(
        combined.contains("time p95"),
        "gate must name the breached limit:\n{combined}"
    );
}

/// A manifest measured from different source bytes is stale and refused.
#[test]
fn cost_gate_refuses_stale_manifest() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let budget = dir.join("budget.json");
    write_file(&budget, GENEROUS_BUDGET);
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    // Change the source after measurement (a comment changes the hash).
    let mut text = std::fs::read_to_string(&src).unwrap();
    text.push_str("\n# post-measurement edit\n");
    std::fs::write(&src, text).unwrap();
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!ok, "stale manifest must fail:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("STALE"),
        "gate must say STALE:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A budget naming a function that exists nowhere is a typo, not a pass.
#[test]
fn cost_gate_refuses_unknown_budget_function() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let budget = dir.join("budget.json");
    write_file(
        &budget,
        r#"{"format": "cuni-cost-budget/1", "reference": null, "defaults": null,
            "per_function": {"typo_name": {"time_us_p95_max": 1}}, "totals": null}"#,
    );
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!ok, "unknown budget function must fail:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("unknown function `typo_name`"),
        "gate must name the unknown function:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Energy: without a TDP the manifest carries no energy and a budget that
/// limits energy cannot be verified (FAIL); with a TDP it verifies.
#[test]
fn cost_gate_energy_unmodeled_cannot_verify() {
    let src = fixture("fast.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("fast.cost.json");
    let budget = dir.join("budget.json");
    write_file(
        &budget,
        r#"{"format": "cuni-cost-budget/1", "reference": null,
            "defaults": {"energy_mj_p95_max": 1.0}, "per_function": {}, "totals": null}"#,
    );
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!ok, "unmodeled energy must not verify:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("CANNOT VERIFY"),
        "gate must say CANNOT VERIFY:\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Same program, measured WITH a TDP: the same budget now verifies.
    write_file(
        &budget,
        r#"{"format": "cuni-cost-budget/1", "reference": null,
            "defaults": {"energy_mj_p95_max": 1000.0}, "per_function": {}, "totals": null}"#,
    );
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
            "--tdp-watts",
            "65",
        ],
        &dir,
    );
    assert!(ok, "measure with TDP must succeed: {stderr}");
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(ok, "modeled energy must verify:\nstdout: {stdout}\nstderr: {stderr}");
}

/// Time-to-detect: the deploy gate catches the injected regression in
/// milliseconds, at build time — versus production monitoring, whose
/// detection delay is bounded below by its scrape + alerting pipeline.
/// The 60s floor is a documented assumption (docs/DEPLOY_GATES.md), not a
/// measured fact; the gate latency IS measured, and must sit below it.
#[test]
fn cost_gate_time_to_detect() {
    const PRODUCTION_MONITORING_FLOOR_MS: u128 = 60_000;
    let src = fixture("slow.cuni");
    let dir = src.parent().unwrap().to_path_buf();
    let manifest = dir.join("slow.cost.json");
    let budget = dir.join("budget.json");
    write_file(&budget, TIGHT_BUDGET);
    let (ok, _, stderr) = run_cuni(
        &[
            "cost",
            src.to_str().unwrap(),
            "--out",
            manifest.to_str().unwrap(),
            "--reps",
            "2",
        ],
        &dir,
    );
    assert!(ok, "measure must succeed: {stderr}");
    let t0 = Instant::now();
    let (ok, stdout, stderr) = run_cuni(
        &[
            "cost",
            "--check",
            manifest.to_str().unwrap(),
            "--budget",
            budget.to_str().unwrap(),
            "--source",
            src.to_str().unwrap(),
        ],
        &dir,
    );
    let gate_latency_ms = t0.elapsed().as_millis();
    assert!(!ok, "regression must fail the gate:\nstdout: {stdout}\nstderr: {stderr}");
    println!(
        "time-to-detect: deploy gate caught the injected regression in {gate_latency_ms}ms; production-monitoring floor (assumed) is {PRODUCTION_MONITORING_FLOOR_MS}ms"
    );
    assert!(
        gate_latency_ms < PRODUCTION_MONITORING_FLOOR_MS,
        "gate latency {gate_latency_ms}ms must be below the production-monitoring floor {PRODUCTION_MONITORING_FLOOR_MS}ms"
    );
}
