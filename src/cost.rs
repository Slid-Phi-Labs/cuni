//! Cost-carrying binaries (55-ideas D1 / #34).
//!
//! The compiler emits, alongside the binary, a per-function cost manifest
//! measured on reference hardware — and the deploy pipeline refuses a release
//! whose measured costs exceed the declared budget, the way a type error
//! fails a build. Costs are re-measured on every rebuild: a manifest is
//! always produced by a fresh profiling run, never cached, and the deploy
//! checker refuses a manifest whose source hash no longer matches the source
//! it was measured from.
//!
//! # What is measured, honestly
//!
//! The profiling seat is the compiler's own reference interpreter
//! (`interp::run_profiled`); `measured_seat` says so in the manifest.
//! Per call, per function (inclusive wall time, callees included):
//!
//! * `time_us` — wall time, mean / p50 / p95 / max over all sampled calls
//!   (nearest-rank percentiles). Real measurement.
//! * `memory_bytes_peak` — max RSS delta observed across the call (Linux
//!   `/proc/self/statm`, page-granular: a call allocating under one page
//!   reads 0). Real measurement, documented granularity.
//! * `io_bytes` — stdout bytes written while the call was innermost. Real
//!   measurement.
//! * `energy_mj` — MODELLED, never claimed as measured: wall time × a
//!   declared TDP (`--tdp-watts`). `null` unless a TDP is supplied, and the
//!   manifest records the TDP and the method so the number is auditable.
//!   A budget that limits energy against a manifest with no energy
//!   measurement FAILS the gate ("cannot verify") instead of passing
//!   silently.
//!
//! # Commands
//!
//! * `cuni cost <file.cuni> [--out manifest.json] [--reps N]
//!   [--tdp-watts W]` — measure and write the manifest.
//! * `cuni cost --check <manifest> --budget <budget> --source <file.cuni>`
//!   — deploy gate: exit 0 PASS, exit 1 FAIL (breach / stale / unverifiable),
//!   exit 2 usage error.
//! * `cuni cost --init-budget [--out budget.json]` — write a template budget.
//!
//! # Budget semantics (fail-closed)
//!
//! A limit applies per function: `per_function[name]` wins, else `defaults`.
//! A budget naming a function that is neither defined in `--source` nor in
//! the manifest is a FAIL (a typo'd budget must not silently apply to
//! nothing). A function the profiling run never exercised is reported, not
//! checked — and the manifest says `exercised: false`, so a deploy that
//! never ran a function cannot smuggle it past the gate unnoticed. A budget
//! that pins `reference` requires the manifest's reference id to match
//! exactly (budgets are anchored to the hardware they were written for).

use crate::audit::{sha256_hex, utc_timestamp};
use crate::check;
use crate::interp::{run_profiled, FnSample, Profile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

pub const MANIFEST_FORMAT: &str = "cuni-cost-manifest/1";
pub const BUDGET_FORMAT: &str = "cuni-cost-budget/1";

/// Nearest-rank percentile over a sorted slice of u64 samples.
fn percentile(sorted: &[u64], pct: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[idx])
}

fn mean_u64(xs: &[u64]) -> Option<u64> {
    if xs.is_empty() {
        return None;
    }
    let sum: u128 = xs.iter().map(|&x| x as u128).sum();
    Some((sum / xs.len() as u128) as u64)
}

fn mean_f64(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    Some(xs.iter().sum::<f64>() / xs.len() as f64)
}

fn percentile_f64(sorted: &[f64], pct: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[idx])
}

/// Per-call time statistics (µs, inclusive wall time).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TimeStats {
    pub mean: Option<u64>,
    pub p50: Option<u64>,
    pub p95: Option<u64>,
    pub max: Option<u64>,
}

/// Per-call energy statistics (mJ, modelled — see module docs).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct EnergyStats {
    pub mean: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FnCost {
    pub name: String,
    /// Calls observed across all reps. 0 → `exercised: false`, time/energy null.
    pub calls: u64,
    pub exercised: bool,
    pub time_us: TimeStats,
    pub memory_bytes_peak: u64,
    pub io_bytes: u64,
    pub energy_mj: EnergyStats,
    /// Total modelled energy over all calls (mJ); null without a TDP.
    pub energy_mj_total: Option<f64>,
}

/// The machine the costs were measured on. `id` is the stable reference
/// anchor a budget pins (`{os}-{arch}-{cpu-model-slug}`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReferenceHw {
    pub id: String,
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_count: usize,
    pub mem_total_kib: Option<u64>,
    pub rustc_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CostTotals {
    pub wall_us: u64,
    pub peak_rss_bytes: u64,
    pub io_bytes: u64,
    pub energy_mj_total: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CostManifest {
    pub format: String,
    pub cuni_version: String,
    pub source_path: String,
    pub source_sha256: String,
    pub measured_at: String,
    pub measured_seat: String,
    pub reps: u32,
    pub reference: ReferenceHw,
    /// "measured: interp wall time" / "modeled: wall_time × tdp_w" / "unmodeled: no TDP supplied".
    pub energy_method: String,
    pub tdp_watts: Option<f64>,
    pub totals: CostTotals,
    pub functions: Vec<FnCost>,
}

/// One function's budget limits. Every field is optional: unset = no limit.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FnLimits {
    pub time_us_p95_max: Option<u64>,
    pub time_us_max_max: Option<u64>,
    pub memory_bytes_peak_max: Option<u64>,
    pub io_bytes_max: Option<u64>,
    pub energy_mj_p95_max: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TotalLimits {
    pub wall_us_max: Option<u64>,
    pub peak_rss_bytes_max: Option<u64>,
    pub io_bytes_max: Option<u64>,
    pub energy_mj_total_max: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CostBudget {
    pub format: String,
    /// If set, the manifest's `reference.id` must equal this exactly.
    pub reference: Option<String>,
    pub defaults: Option<FnLimits>,
    #[serde(default)]
    pub per_function: BTreeMap<String, FnLimits>,
    pub totals: Option<TotalLimits>,
}

fn cpu_model() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(info) = fs::read_to_string("/proc/cpuinfo") {
            for line in info.lines() {
                if let Some(rest) = line.strip_prefix("model name") {
                    if let Some(v) = rest.split(':').nth(1) {
                        return v.trim().to_string();
                    }
                }
            }
        }
    }
    "unknown".to_string()
}

fn mem_total_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(info) = fs::read_to_string("/proc/meminfo") {
            for line in info.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    return rest
                        .trim()
                        .split_whitespace()
                        .next()
                        .and_then(|t| t.parse::<u64>().ok());
                }
            }
        }
    }
    None
}

fn rustc_version() -> Option<String> {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

fn reference_hw() -> ReferenceHw {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let model = cpu_model();
    let slug: String = model
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug.chars().take(40).collect::<String>();
    ReferenceHw {
        id: format!("{os}-{arch}-{slug}"),
        os,
        arch,
        cpu_model: model,
        cpu_count: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        mem_total_kib: mem_total_kib(),
        rustc_version: rustc_version(),
    }
}

/// Aggregate pooled per-call samples into a per-function cost row.
fn aggregate_fn(name: &str, samples: &[&FnSample], tdp_watts: Option<f64>) -> FnCost {
    let mut durs: Vec<u64> = samples.iter().map(|s| s.dur_ns / 1000).collect();
    durs.sort_unstable();
    let energy: Vec<f64> = match tdp_watts {
        Some(w) => samples
            .iter()
            .map(|s| s.dur_ns as f64 / 1e9 * w * 1000.0)
            .collect(),
        None => Vec::new(),
    };
    let mut energy_sorted = energy.clone();
    energy_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let energy_total: Option<f64> = match tdp_watts {
        Some(_) => Some(energy.iter().sum()),
        None => None,
    };
    FnCost {
        name: name.to_string(),
        calls: samples.len() as u64,
        exercised: !samples.is_empty(),
        time_us: TimeStats {
            mean: mean_u64(&durs),
            p50: percentile(&durs, 50.0),
            p95: percentile(&durs, 95.0),
            max: durs.last().copied(),
        },
        memory_bytes_peak: samples
            .iter()
            .map(|s| s.rss_delta_bytes.max(0) as u64)
            .max()
            .unwrap_or(0),
        io_bytes: samples.iter().map(|s| s.io_bytes).sum(),
        energy_mj: EnergyStats {
            mean: mean_f64(&energy_sorted),
            p50: percentile_f64(&energy_sorted, 50.0),
            p95: percentile_f64(&energy_sorted, 95.0),
            max: energy_sorted.last().copied(),
        },
        energy_mj_total: energy_total,
    }
}

/// Measure `source` — one warmup run (discarded) plus `reps` profiled runs —
/// and build the manifest. Fails if the program does not run cleanly: no
/// manifest is ever written for a failing program.
pub fn measure(
    source: &Path,
    reps: u32,
    tdp_watts: Option<f64>,
) -> Result<CostManifest, String> {
    if reps == 0 {
        return Err("--reps must be >= 1".to_string());
    }
    if let Some(w) = tdp_watts {
        if !(w > 0.0) {
            return Err("--tdp-watts must be > 0".to_string());
        }
    }
    let program = check::load_program(source)?;
    let source_bytes =
        fs::read(source).map_err(|e| format!("couldn't read {}: {}", source.display(), e))?;
    let source_sha256 = sha256_hex(&source_bytes);
    // Warmup (discarded): page faults, allocator warmup, first-run costs.
    run_profiled(&program).map_err(|e| format!("warmup run failed: {e}"))?;
    let mut pooled: Vec<FnSample> = Vec::new();
    let mut wall_us: u64 = 0;
    let mut peak_rss: u64 = 0;
    let mut io_total: u64 = 0;
    for _ in 0..reps {
        let (_out, prof): (String, Profile) =
            run_profiled(&program).map_err(|e| format!("profiling run failed: {e}"))?;
        wall_us += prof.wall_ns / 1000;
        peak_rss = peak_rss.max(prof.peak_rss_bytes);
        io_total += prof.samples.iter().map(|s| s.io_bytes).sum::<u64>();
        pooled.extend(prof.samples);
    }
    // Every defined function gets a row — even one the run never called
    // (exercised: false), so the manifest cannot silently omit a function.
    let mut by_name: BTreeMap<String, Vec<&FnSample>> = BTreeMap::new();
    for s in &pooled {
        by_name.entry(s.name.clone()).or_default().push(s);
    }
    let mut functions: Vec<FnCost> = Vec::new();
    for item in &program.items {
        if let crate::ast::Item::Def(f) = item {
            let samples: &[&FnSample] = by_name
                .get(&f.name)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            functions.push(aggregate_fn(&f.name, samples, tdp_watts));
        }
    }
    // `<top>` is a synthetic row: keep it, it is real cost.
    let top_samples: &[&FnSample] = by_name.get("<top>").map(|v| v.as_slice()).unwrap_or(&[]);
    functions.push(aggregate_fn("<top>", top_samples, tdp_watts));
    functions.sort_by(|a, b| a.name.cmp(&b.name));
    let energy_total: Option<f64> = match tdp_watts {
        Some(w) => Some(wall_us as f64 / 1e6 * w * 1000.0),
        None => None,
    };
    let energy_method = match tdp_watts {
        Some(w) => format!("modeled: wall_time x tdp_w (tdp_w={w})"),
        None => "unmodeled: no TDP supplied (pass --tdp-watts to model energy)".to_string(),
    };
    Ok(CostManifest {
        format: MANIFEST_FORMAT.to_string(),
        cuni_version: env!("CARGO_PKG_VERSION").to_string(),
        source_path: source.display().to_string(),
        source_sha256,
        measured_at: utc_timestamp(),
        measured_seat: "interp".to_string(),
        reps,
        reference: reference_hw(),
        energy_method,
        tdp_watts,
        totals: CostTotals {
            wall_us,
            peak_rss_bytes: peak_rss,
            io_bytes: io_total,
            energy_mj_total: energy_total,
        },
        functions,
    })
}

/// Outcome of the deploy gate.
#[derive(Clone, Debug)]
pub struct GateOutcome {
    pub passed: bool,
    pub breaches: Vec<String>,
    pub notes: Vec<String>,
    pub checked: usize,
}

/// Compare a manifest against a budget, with the source as the authority on
/// which functions exist. Fail-closed: stale manifest, pinned-reference
/// mismatch, unknown budget function, or an energy limit the manifest cannot
/// verify are all FAILs, not warnings.
pub fn check_gate(
    manifest: &CostManifest,
    budget: &CostBudget,
    defined_fns: &[String],
) -> GateOutcome {
    let mut breaches: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut checked: usize = 0;

    if manifest.format != MANIFEST_FORMAT {
        breaches.push(format!(
            "manifest format is `{}`, expected `{MANIFEST_FORMAT}`",
            manifest.format
        ));
        return GateOutcome {
            passed: false,
            breaches,
            notes,
            checked,
        };
    }
    if budget.format != BUDGET_FORMAT {
        breaches.push(format!(
            "budget format is `{}`, expected `{BUDGET_FORMAT}`",
            budget.format
        ));
        return GateOutcome {
            passed: false,
            breaches,
            notes,
            checked,
        };
    }
    if let Some(pinned) = &budget.reference {
        if &manifest.reference.id != pinned {
            breaches.push(format!(
                "reference mismatch: manifest measured on `{}`, budget pins `{pinned}` (budgets are anchored to the hardware they were written for)",
                manifest.reference.id
            ));
        }
    }
    // A budget naming a function that exists nowhere is a typo, not a
    // vacuous pass.
    for name in budget.per_function.keys() {
        if !defined_fns.contains(name) && !manifest.functions.iter().any(|f| &f.name == name) {
            breaches.push(format!(
                "budget names unknown function `{name}` (not defined in source, not in manifest)"
            ));
        }
    }

    let limits_for = |name: &str| -> FnLimits {
        let mut lim = budget.defaults.clone().unwrap_or_default();
        if let Some(per) = budget.per_function.get(name) {
            if per.time_us_p95_max.is_some() {
                lim.time_us_p95_max = per.time_us_p95_max;
            }
            if per.time_us_max_max.is_some() {
                lim.time_us_max_max = per.time_us_max_max;
            }
            if per.memory_bytes_peak_max.is_some() {
                lim.memory_bytes_peak_max = per.memory_bytes_peak_max;
            }
            if per.io_bytes_max.is_some() {
                lim.io_bytes_max = per.io_bytes_max;
            }
            if per.energy_mj_p95_max.is_some() {
                lim.energy_mj_p95_max = per.energy_mj_p95_max;
            }
        }
        lim
    };

    for f in &manifest.functions {
        if !f.exercised {
            notes.push(format!(
                "function `{}` was not exercised by the profiling run (no calls observed); budget limits not checked for it",
                f.name
            ));
            continue;
        }
        let lim = limits_for(&f.name);
        if let Some(max) = lim.time_us_p95_max {
            checked += 1;
            if let Some(p95) = f.time_us.p95 {
                if p95 > max {
                    breaches.push(format!(
                        "BREACH fn `{}`: time p95 {p95}µs > budget {max}µs",
                        f.name
                    ));
                }
            }
        }
        if let Some(max) = lim.time_us_max_max {
            checked += 1;
            if let Some(mx) = f.time_us.max {
                if mx > max {
                    breaches.push(format!(
                        "BREACH fn `{}`: time max {mx}µs > budget {max}µs",
                        f.name
                    ));
                }
            }
        }
        if let Some(max) = lim.memory_bytes_peak_max {
            checked += 1;
            if f.memory_bytes_peak > max {
                breaches.push(format!(
                    "BREACH fn `{}`: memory peak {}B > budget {max}B",
                    f.name, f.memory_bytes_peak
                ));
            }
        }
        if let Some(max) = lim.io_bytes_max {
            checked += 1;
            if f.io_bytes > max {
                breaches.push(format!(
                    "BREACH fn `{}`: io {}B > budget {max}B",
                    f.name, f.io_bytes
                ));
            }
        }
        if let Some(max) = lim.energy_mj_p95_max {
            checked += 1;
            match f.energy_mj.p95 {
                Some(p95) => {
                    if p95 > max {
                        breaches.push(format!(
                            "BREACH fn `{}`: energy p95 {p95:.3}mJ > budget {max:.3}mJ",
                            f.name
                        ));
                    }
                }
                None => breaches.push(format!(
                    "CANNOT VERIFY fn `{}`: budget limits energy (p95 <= {max:.3}mJ) but the manifest has no energy measurement (re-measure with --tdp-watts)",
                    f.name
                )),
            }
        }
    }

    if let Some(tot) = &budget.totals {
        if let Some(max) = tot.wall_us_max {
            checked += 1;
            if manifest.totals.wall_us > max {
                breaches.push(format!(
                    "BREACH totals: wall {}µs > budget {max}µs",
                    manifest.totals.wall_us
                ));
            }
        }
        if let Some(max) = tot.peak_rss_bytes_max {
            checked += 1;
            if manifest.totals.peak_rss_bytes > max {
                breaches.push(format!(
                    "BREACH totals: peak RSS {}B > budget {max}B",
                    manifest.totals.peak_rss_bytes
                ));
            }
        }
        if let Some(max) = tot.io_bytes_max {
            checked += 1;
            if manifest.totals.io_bytes > max {
                breaches.push(format!(
                    "BREACH totals: io {}B > budget {max}B",
                    manifest.totals.io_bytes
                ));
            }
        }
        if let Some(max) = tot.energy_mj_total_max {
            checked += 1;
            match manifest.totals.energy_mj_total {
                Some(total) => {
                    if total > max {
                        breaches.push(format!(
                            "BREACH totals: energy {total:.3}mJ > budget {max:.3}mJ"
                        ));
                    }
                }
                None => breaches.push(format!(
                    "CANNOT VERIFY totals: budget limits energy (<= {max:.3}mJ) but the manifest has no energy measurement (re-measure with --tdp-watts)"
                )),
            }
        }
    }

    GateOutcome {
        passed: breaches.is_empty(),
        breaches,
        notes,
        checked,
    }
}

/// Defined function names in a source file (`<top>` included: top-level
/// statements are a cost row too).
pub fn defined_fns(source: &Path) -> Result<Vec<String>, String> {
    let program = check::load_program(source)?;
    let mut names: Vec<String> = program
        .items
        .iter()
        .filter_map(|i| match i {
            crate::ast::Item::Def(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    names.push("<top>".to_string());
    Ok(names)
}

pub fn template_budget() -> CostBudget {
    // NOTE: `per_function` is deliberately empty. An example entry naming
    // a function the program does not define would make the template FAIL
    // the gate by design (unknown budget function = fail-closed), so the
    // template ships with defaults + totals only; add per-function entries
    // for functions your program actually defines.
    CostBudget {
        format: BUDGET_FORMAT.to_string(),
        reference: None,
        defaults: Some(FnLimits {
            time_us_p95_max: Some(100_000),
            time_us_max_max: Some(1_000_000),
            memory_bytes_peak_max: Some(16_777_216),
            io_bytes_max: Some(1_048_576),
            energy_mj_p95_max: None,
        }),
        per_function: BTreeMap::new(),
        totals: Some(TotalLimits {
            wall_us_max: Some(5_000_000),
            peak_rss_bytes_max: Some(268_435_456),
            io_bytes_max: Some(1_048_576),
            energy_mj_total_max: None,
        }),
    }
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("cuni cost: {msg} (try `cuni cost --help`)");
    ExitCode::from(2)
}

fn print_cost_help() {
    eprintln!(
        "\\
cuni cost — cost-carrying binaries (55-ideas D1 / #34).

  cuni cost <file.cuni> [--out manifest.json] [--reps N] [--tdp-watts W]
      Measure the program (1 warmup + N profiled runs on the reference
      interpreter seat) and write a per-function cost manifest
      (cuni-cost-manifest/1) next to the binary. Costs are re-measured on
      every run — never cached. Default --out is <stem>.cost.json beside
      the source; default --reps is 5. --tdp-watts models energy as
      wall_time x TDP; without it energy is null (unmodeled, auditable).
  cuni cost --check <manifest> --budget <budget> --source <file.cuni>
      Deploy gate: compare the manifest against the budget and FAIL the
      deploy on breach, like a type error. Also refuses a stale manifest
      (source hash mismatch), a budget naming an unknown function, and an
      energy limit the manifest cannot verify. Exit 0 PASS, 1 FAIL.
  cuni cost --init-budget [--out budget.json]
      Write a template cost budget (cuni-cost-budget/1).
"
    );
}

/// `cuni cost ...` — measure a manifest, or run the deploy gate.
pub fn cmd_cost(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_cost_help();
        return ExitCode::SUCCESS;
    }
    let mut check_manifest: Option<String> = None;
    let mut budget_path: Option<String> = None;
    let mut source_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut reps: u32 = 5;
    let mut tdp: Option<f64> = None;
    let mut init_budget = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--check" => {
                i += 1;
                check_manifest = args.get(i).cloned();
            }
            "--budget" => {
                i += 1;
                budget_path = args.get(i).cloned();
            }
            "--source" => {
                i += 1;
                source_path = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out_path = args.get(i).cloned();
            }
            "--reps" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<u32>().ok()) {
                    Some(n) => reps = n,
                    None => return usage_err("--reps requires a positive integer"),
                }
            }
            "--tdp-watts" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<f64>().ok()) {
                    Some(w) => tdp = Some(w),
                    None => return usage_err("--tdp-watts requires a number"),
                }
            }
            "--init-budget" => init_budget = true,
            _ if a.starts_with('-') => {
                return usage_err(&format!("unknown flag `{a}`"));
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }

    if init_budget {
        let out = match out_path {
            Some(p) => PathBuf::from(p),
            None => PathBuf::from("budget.json"),
        };
        let json = serde_json::to_string_pretty(&template_budget())
            .expect("template budget serialization cannot fail");
        if let Err(e) = fs::write(&out, format!("{json}\n")) {
            eprintln!("cuni cost: write {}: {e}", out.display());
            return ExitCode::FAILURE;
        }
        println!("cuni cost: wrote template budget {}", out.display());
        return ExitCode::SUCCESS;
    }

    if let Some(mpath) = check_manifest {
        // ---- Deploy gate ----
        let bpath = match budget_path {
            Some(b) => b,
            None => return usage_err("--check requires --budget <budget.json>"),
        };
        let spath = match source_path {
            Some(s) => s,
            None => return usage_err("--check requires --source <file.cuni>"),
        };
        let t0 = Instant::now();
        let manifest_text = match fs::read_to_string(&mpath) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("cuni cost: FAIL — cannot read manifest {mpath}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let manifest: CostManifest = match serde_json::from_str(&manifest_text) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("cuni cost: FAIL — manifest {mpath} is not valid JSON: {e}");
                return ExitCode::FAILURE;
            }
        };
        let budget_text = match fs::read_to_string(&bpath) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("cuni cost: FAIL — cannot read budget {bpath}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let budget: CostBudget = match serde_json::from_str(&budget_text) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cuni cost: FAIL — budget {bpath} is not valid JSON: {e}");
                return ExitCode::FAILURE;
            }
        };
        // Stale manifest: the source changed since measurement. Re-measure;
        // never deploy on a manifest that describes different code.
        let source_bytes = match fs::read(&spath) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cuni cost: FAIL — cannot read source {spath}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let outcome: GateOutcome;
        if sha256_hex(&source_bytes) != manifest.source_sha256 {
            outcome = GateOutcome {
                passed: false,
                breaches: vec![format!(
                    "STALE manifest: source {spath} no longer matches the source the manifest was measured from (sha256 mismatch) — re-run `cuni cost` to re-measure"
                )],
                notes: vec![],
                checked: 0,
            };
        } else {
            let defined = match defined_fns(Path::new(&spath)) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("cuni cost: FAIL — source {spath} does not load: {e}");
                    return ExitCode::FAILURE;
                }
            };
            outcome = check_gate(&manifest, &budget, &defined);
        }
        let ms = t0.elapsed().as_millis();
        if outcome.passed {
            println!(
                "cost gate: PASS ({} limit(s) checked, {} function(s), gate latency {}ms)",
                outcome.checked,
                manifest.functions.len(),
                ms
            );
            for n in &outcome.notes {
                println!("  note: {n}");
            }
            return ExitCode::SUCCESS;
        }
        eprintln!("cost gate: FAIL (gate latency {ms}ms)");
        for b in &outcome.breaches {
            eprintln!("  {b}");
        }
        for n in &outcome.notes {
            eprintln!("  note: {n}");
        }
        return ExitCode::FAILURE;
    }

    if positional.is_empty() {
        return usage_err("missing <file.cuni> (or --check <manifest>)");
    }
    if positional.len() > 1 {
        return usage_err("one source file at a time");
    }
    let source = PathBuf::from(&positional[0]);
    let out = match out_path {
        Some(p) => PathBuf::from(p),
        None => source.with_extension("cost.json"),
    };
    let t0 = Instant::now();
    match measure(&source, reps, tdp) {
        Ok(manifest) => {
            let json = serde_json::to_string_pretty(&manifest)
                .expect("manifest serialization cannot fail");
            if let Err(e) = fs::write(&out, format!("{json}\n")) {
                eprintln!("cuni cost: write {}: {e}", out.display());
                return ExitCode::FAILURE;
            }
            let wall_ms = t0.elapsed().as_millis();
            println!(
                "cuni cost: measured {} ({} reps, {} functions, wall {}µs) -> {} in {wall_ms}ms",
                source.display(),
                reps,
                manifest.functions.len(),
                manifest.totals.wall_us,
                out.display()
            );
            for f in &manifest.functions {
                let p95 = f
                    .time_us
                    .p95
                    .map(|v| format!("{v}µs"))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {:<16} calls={:<6} p95={:<10} mem_peak={:<10} io={}",
                    f.name, f.calls, p95, f.memory_bytes_peak, f.io_bytes
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cuni cost: measurement failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let mut xs = vec![10u64, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        xs.sort_unstable();
        assert_eq!(percentile(&xs, 50.0), Some(50));
        assert_eq!(percentile(&xs, 95.0), Some(100));
        assert_eq!(percentile(&xs, 100.0), Some(100));
        assert_eq!(percentile(&[], 95.0), None);
        assert_eq!(percentile(&[7u64], 95.0), Some(7));
    }

    #[test]
    fn mean_u64_rounds_down() {
        assert_eq!(mean_u64(&[1, 2]), Some(1));
        assert_eq!(mean_u64(&[]), None);
    }

    fn manifest_with_fn(name: &str, p95: Option<u64>, exercised: bool) -> CostManifest {
        CostManifest {
            format: MANIFEST_FORMAT.to_string(),
            cuni_version: "0.10.1".to_string(),
            source_path: "x.cuni".to_string(),
            source_sha256: "00".repeat(32),
            measured_at: "2026-11-18T00:00:00Z".to_string(),
            measured_seat: "interp".to_string(),
            reps: 5,
            reference: ReferenceHw {
                id: "linux-x86_64-test".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                cpu_model: "test".to_string(),
                cpu_count: 1,
                mem_total_kib: None,
                rustc_version: None,
            },
            energy_method: "unmodeled".to_string(),
            tdp_watts: None,
            totals: CostTotals {
                wall_us: 1000,
                peak_rss_bytes: 0,
                io_bytes: 0,
                energy_mj_total: None,
            },
            functions: vec![FnCost {
                name: name.to_string(),
                calls: if exercised { 10 } else { 0 },
                exercised,
                time_us: TimeStats {
                    mean: p95,
                    p50: p95,
                    p95,
                    max: p95,
                },
                memory_bytes_peak: 0,
                io_bytes: 0,
                energy_mj: EnergyStats::default(),
                energy_mj_total: None,
            }],
        }
    }

    #[test]
    fn gate_passes_within_budget() {
        let m = manifest_with_fn("f", Some(100), true);
        let mut b = template_budget();
        b.reference = None;
        b.defaults = Some(FnLimits {
            time_us_p95_max: Some(1000),
            ..Default::default()
        });
        b.per_function.clear();
        b.totals = None;
        let out = check_gate(&m, &b, &["f".to_string(), "<top>".to_string()]);
        assert!(out.passed, "{:?}", out.breaches);
        assert_eq!(out.checked, 1);
    }

    #[test]
    fn gate_fails_on_p95_breach() {
        let m = manifest_with_fn("slow", Some(5000), true);
        let mut b = template_budget();
        b.reference = None;
        b.defaults = Some(FnLimits {
            time_us_p95_max: Some(1000),
            ..Default::default()
        });
        b.per_function.clear();
        b.totals = None;
        let out = check_gate(&m, &b, &["slow".to_string(), "<top>".to_string()]);
        assert!(!out.passed);
        assert!(out.breaches.iter().any(|x| x.contains("slow") && x.contains("5000")));
    }

    #[test]
    fn gate_fails_on_unknown_budget_function() {
        let m = manifest_with_fn("f", Some(100), true);
        let mut b = template_budget();
        b.reference = None;
        b.defaults = None;
        b.per_function.clear();
        b.per_function.insert(
            "typo_name".to_string(),
            FnLimits {
                time_us_p95_max: Some(1),
                ..Default::default()
            },
        );
        b.totals = None;
        let out = check_gate(&m, &b, &["f".to_string(), "<top>".to_string()]);
        assert!(!out.passed);
        assert!(out
            .breaches
            .iter()
            .any(|x| x.contains("unknown function `typo_name`")));
    }

    #[test]
    fn gate_fails_on_reference_pin_mismatch() {
        let m = manifest_with_fn("f", Some(100), true);
        let mut b = template_budget();
        b.reference = Some("linux-arm64-other".to_string());
        b.defaults = None;
        b.per_function.clear();
        b.totals = None;
        let out = check_gate(&m, &b, &["f".to_string(), "<top>".to_string()]);
        assert!(!out.passed);
        assert!(out.breaches.iter().any(|x| x.contains("reference mismatch")));
    }

    #[test]
    fn gate_cannot_verify_energy_without_measurement() {
        let m = manifest_with_fn("f", Some(100), true);
        let mut b = template_budget();
        b.reference = None;
        b.defaults = Some(FnLimits {
            energy_mj_p95_max: Some(1.0),
            ..Default::default()
        });
        b.per_function.clear();
        b.totals = None;
        let out = check_gate(&m, &b, &["f".to_string(), "<top>".to_string()]);
        assert!(!out.passed);
        assert!(out
            .breaches
            .iter()
            .any(|x| x.contains("CANNOT VERIFY") && x.contains("energy")));
    }

    #[test]
    fn gate_notes_unexercised_function_instead_of_checking() {
        let m = manifest_with_fn("never_called", None, false);
        let mut b = template_budget();
        b.reference = None;
        b.defaults = Some(FnLimits {
            time_us_p95_max: Some(1),
            ..Default::default()
        });
        b.per_function.clear();
        b.totals = None;
        let out = check_gate(&m, &b, &["never_called".to_string(), "<top>".to_string()]);
        assert!(out.passed, "{:?}", out.breaches);
        assert!(out
            .notes
            .iter()
            .any(|x| x.contains("never_called") && x.contains("not exercised")));
    }

    #[test]
    fn aggregate_fn_pools_samples() {
        let s: Vec<FnSample> = (1..=10)
            .map(|i| FnSample {
                name: "f".to_string(),
                dur_ns: i * 1000,
                io_bytes: 0,
                rss_delta_bytes: 0,
            })
            .collect();
        let refs: Vec<&FnSample> = s.iter().collect();
        let row = aggregate_fn("f", &refs, Some(65.0));
        assert_eq!(row.calls, 10);
        assert!(row.exercised);
        assert_eq!(row.time_us.p50, Some(5));
        assert_eq!(row.time_us.p95, Some(10));
        assert_eq!(row.time_us.max, Some(10));
        assert!(row.energy_mj_total.is_some());
        // 10 calls × (1..10 µs) × 65W → 55µs × 65W = 3.575 mJ total.
        let total = row.energy_mj_total.unwrap();
        assert!((total - 3.575).abs() < 1e-9, "{total}");
    }
}
