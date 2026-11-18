//! Spec-carrying artifacts (55-ideas B10 / #21).
//!
//! The deployable artifact carries its formal spec and proof inside it,
//! signed; the admission controller re-verifies the proof against live
//! environment state at deploy time and refuses spec-less artifacts. A spec
//! edit or binary change invalidates the proof.
//!
//! # Bundle layout
//!
//! ```text
//! <name>.cunibundle/
//!   manifest.json   — cuni-bundle/1: name, seat, source hash, sha256 of every other file
//!   source.cuni     — the source the artifact was built from
//!   artifact.<ext>  — the emitted deployable artifact (one seat)
//!   spec.json       — machine-readable spec (cuni-spec/1), strict schema
//!   proof.json      — cuni-proof/1: hashes binding binary+spec+source,
//!                     the checks run at bundle time, gold stdout hash,
//!                     Ed25519 signature over the canonical proof bytes
//! ```
//!
//! # Commands
//!
//! * `cuni bundle <file.cuni> --spec spec.json --out DIR [--seat rs]
//!   [--sign keyfile] [--timeout secs]` — build a bundle. `--sign` is
//!   REQUIRED: an unsigned bundle cannot exist (admission would refuse it).
//!   Bundle creation itself re-verifies: the artifact must run and print
//!   byte-identical stdout to the reference interpreter, the spec's
//!   `behavior.stdout_sha256` (if set) must match, and `max_wall_ms` (if
//!   set) must hold — otherwise no bundle is written (the output directory
//!   is removed on failure, so no half-bundle can be admitted later).
//! * `cuni admit <dir> [--env env.json] [--timeout secs]` — admission
//!   controller. Exit 0 ADMIT, exit 1 REFUSE with every reason listed,
//!   exit 2 usage error. Refuses: missing/unreadable manifest, spec-less
//!   artifact, any file hash mismatch (tampered binary/spec/source),
//!   proof hash mismatch, missing/invalid signature, spec schema
//!   violation, live environment assumption violated, live re-run
//!   divergence (stdout hash, exit code, wall budget).
//!
//! # Spec schema (strict — unknown keys are refused, not ignored)
//!
//! `format: "cuni-spec/1"`, `name`, `version`, `entry` (a defined function
//! or `"<top>"`), `seats` (must contain the bundle seat),
//! `env_assumptions: {os?, arch?, toolchains: {seat: ">=x.y"}}`,
//! `behavior: {stdout_sha256?, exit_code?}`,
//! `resource: {max_wall_ms?}`. Only the toolchain seats in the table below
//! can be version-checked; naming any other seat refuses admission with the
//! seat named (fail-closed: the controller only admits what it can verify).

use crate::audit::{hex_of, sha256_hex, utc_timestamp};
use crate::check;
use crate::emit;
use crate::interp;
use crate::langs;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub const BUNDLE_FORMAT: &str = "cuni-bundle/1";
pub const SPEC_FORMAT: &str = "cuni-spec/1";
pub const PROOF_FORMAT: &str = "cuni-proof/1";

// ---------------------------------------------------------------- spec ---

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvAssumptions {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub toolchains: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Behavior {
    #[serde(default)]
    pub stdout_sha256: Option<String>,
    #[serde(default)]
    pub exit_code: Option<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    #[serde(default)]
    pub max_wall_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub format: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub seats: Vec<String>,
    #[serde(default)]
    pub env_assumptions: EnvAssumptions,
    #[serde(default)]
    pub behavior: Behavior,
    #[serde(default)]
    pub resource: Resource,
}

/// Validate a parsed spec structurally (not against any environment).
/// Returns refusal reasons; empty = valid.
pub fn validate_spec(spec: &Spec, seat: &str) -> Vec<String> {
    let mut reasons = Vec::new();
    if spec.format != SPEC_FORMAT {
        reasons.push(format!(
            "spec format is `{}`, expected `{SPEC_FORMAT}`",
            spec.format
        ));
    }
    if spec.name.trim().is_empty() {
        reasons.push("spec.name must not be empty".to_string());
    }
    if spec.version.trim().is_empty() {
        reasons.push("spec.version must not be empty".to_string());
    }
    if spec.entry.trim().is_empty() {
        reasons.push("spec.entry must not be empty".to_string());
    }
    if spec.seats.is_empty() {
        reasons.push("spec.seats must list at least one seat".to_string());
    } else if !spec.seats.iter().any(|s| s == seat) {
        reasons.push(format!(
            "spec.seats {:?} does not contain the bundle seat `{seat}`",
            spec.seats
        ));
    }
    for (seat_id, constraint) in &spec.env_assumptions.toolchains {
        if toolchain_binary(seat_id).is_none() {
            reasons.push(format!(
                "spec names unknown toolchain seat `{seat_id}` (admission cannot version-check it)"
            ));
        } else if parse_constraint(constraint).is_none() {
            reasons.push(format!(
                "spec toolchain constraint for `{seat_id}` is not parseable: `{constraint}` (expected e.g. `>=1.75`)"
            ));
        }
    }
    if let Some(h) = &spec.behavior.stdout_sha256 {
        if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            reasons.push("spec.behavior.stdout_sha256 must be 64 lowercase hex chars".to_string());
        }
    }
    reasons
}

/// Runner binary for a toolchain seat (used for `--version` checks).
/// Seats not listed here cannot be version-checked, so a spec naming one
/// is refused at admission.
pub fn toolchain_binary(seat: &str) -> Option<&'static str> {
    Some(match seat {
        "py" => "python3",
        "rs" => "rustc",
        "js" | "ts" => "node",
        "go" => "go",
        "c" => "gcc",
        "cpp" => "g++",
        "java" => "java",
        "cs" | "fs" => "dotnet",
        "kt" => "kotlinc",
        "scala" => "scala",
        "rb" => "ruby",
        "lua" => "lua5.4",
        "php" => "php",
        "pl" => "perl",
        "r" => "Rscript",
        "jl" => "julia",
        "ex" => "elixir",
        "hs" => "ghc",
        "ml" => "ocaml",
        "clj" => "clojure",
        "dart" => "dart",
        "zig" => "zig",
        "nim" => "nim",
        "cr" => "crystal",
        "d" => "dmd",
        "v" => "v",
        "ada" => "gnat",
        "pas" => "fpc",
        "f90" => "gfortran",
        "cob" => "cobc",
        "pro" => "swipl",
        "sh" => "bash",
        "ps1" => "pwsh",
        "awk" => "awk",
        "tcl" => "tclsh",
        "hx" => "haxe",
        "groovy" => "groovy",
        "m-objc" => "gcc",
        "sol" => "solc",
        "sql" => "sqlite3",
        _ => return None,
    })
}

// ------------------------------------------------- version constraints ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

fn parse_version(text: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = text.trim().split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut out = Vec::new();
    for p in parts {
        // Tolerate a trailing non-numeric suffix ("1.75.0-nightly" → 1.75.0).
        let num: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        if num.is_empty() {
            return None;
        }
        out.push(num.parse::<u64>().ok()?);
    }
    Some(out)
}

/// Parse `>=1.75`, `>2`, `=1.75.0`, `==1.75`, or bare `1.75` (means `>=`).
fn parse_constraint(text: &str) -> Option<(Op, Vec<u64>)> {
    let t = text.trim();
    for (prefix, op) in [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("==", Op::Eq),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return parse_version(rest).map(|v| (op, v));
        }
    }
    parse_version(t).map(|v| (Op::Ge, v))
}

fn cmp_version(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

fn constraint_holds(installed: &[u64], op: Op, required: &[u64]) -> bool {
    match (op, cmp_version(installed, required)) {
        (Op::Ge, std::cmp::Ordering::Less) => false,
        (Op::Gt, std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => false,
        (Op::Le, std::cmp::Ordering::Greater) => false,
        (Op::Lt, std::cmp::Ordering::Greater | std::cmp::Ordering::Equal) => false,
        (Op::Eq, o) => o == std::cmp::Ordering::Equal,
        _ => true,
    }
}

/// First `x[.y[.z]]` token in `--version` output (stdout, then stderr —
/// `java -version` writes to stderr). `None` = binary missing/unparseable.
fn installed_version(binary: &str) -> Option<Vec<u64>> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .or_else(|| {
            std::process::Command::new(binary)
                .arg("-version")
                .output()
                .ok()
        })?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let t = token.trim_matches('.');
        if t.contains('.') || t.len() >= 2 {
            if let Some(v) = parse_version(t) {
                return Some(v);
            }
        }
    }
    None
}

// ---------------------------------------------------------------- proof ---

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProofCheck {
    pub name: String,
    pub result: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proof {
    pub format: String,
    pub binary_sha256: String,
    pub spec_sha256: String,
    pub source_sha256: String,
    pub cuni_version: String,
    pub issued_at: String,
    pub checks: Vec<ProofCheck>,
    pub gold_stdout_sha256: String,
    pub gold_exit_code: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_pubkey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl Proof {
    /// Canonical bytes the signature covers: the proof with the signature
    /// field removed (pubkey included, mirroring `audit::sign_receipt`).
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        serde_json::to_string(&unsigned)
            .expect("proof serialization cannot fail")
            .into_bytes()
    }
}

pub fn sign_proof(proof: &mut Proof, signing: &SigningKey) {
    proof.signer_pubkey = Some(hex_of(signing.verifying_key().as_bytes()));
    let msg = proof.canonical_bytes();
    let sig: Signature = signing.sign(&msg);
    proof.signature = Some(hex_of(&sig.to_bytes()));
}

/// Verify a parsed proof's signature. `Ok(true)` = signature present and
/// valid; `Ok(false)` = unsigned (admission treats this as a refusal, but
/// the distinction is useful at bundle time); `Err` = present but invalid.
pub fn verify_proof(proof: &Proof) -> Result<bool, String> {
    match (&proof.signer_pubkey, &proof.signature) {
        (None, None) => Ok(false),
        (Some(_), None) => Err("proof has signer_pubkey but no signature".to_string()),
        (None, Some(_)) => Err("proof has signature but no signer_pubkey".to_string()),
        (Some(pk_hex), Some(sig_hex)) => {
            let pk_bytes = crate::audit::unhex(pk_hex)
                .map_err(|e| format!("bad signer_pubkey: {e}"))?;
            let sig_bytes =
                crate::audit::unhex(sig_hex).map_err(|e| format!("bad signature: {e}"))?;
            if pk_bytes.len() != 32 {
                return Err("signer_pubkey must be 32 bytes (64 hex chars)".to_string());
            }
            if sig_bytes.len() != 64 {
                return Err("signature must be 64 bytes (128 hex chars)".to_string());
            }
            let mut pk_arr = [0u8; 32];
            pk_arr.copy_from_slice(&pk_bytes);
            let mut sig_arr = [0u8; 64];
            sig_arr.copy_from_slice(&sig_bytes);
            let vk = VerifyingKey::from_bytes(&pk_arr)
                .map_err(|e| format!("bad signer_pubkey: {e}"))?;
            let sig = Signature::from_bytes(&sig_arr);
            vk.verify(&proof.canonical_bytes(), &sig)
                .map(|_| true)
                .map_err(|_| "proof signature does not verify (proof or key tampered)".to_string())
        }
    }
}

// -------------------------------------------------------------- manifest ---

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BundleManifest {
    pub format: String,
    pub name: String,
    pub cuni_version: String,
    pub created_at: String,
    pub seat: String,
    pub source_sha256: String,
    /// filename → sha256 hex. Covers source.cuni, artifact.<ext>, spec.json,
    /// proof.json. manifest.json itself is never listed (self-hash).
    pub files: BTreeMap<String, String>,
}

// ------------------------------------------------------------- env file ---

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvFacts {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub toolchains: BTreeMap<String, String>,
}

fn live_env_facts() -> EnvFacts {
    EnvFacts {
        os: Some(std::env::consts::OS.to_string()),
        arch: Some(std::env::consts::ARCH.to_string()),
        toolchains: BTreeMap::new(),
    }
}

/// Check the spec's environment assumptions against facts. Toolchain
/// versions are read live from the named binaries (or from `--env` supplied
/// versions, which override live detection for simulation).
fn check_env_assumptions(spec: &Spec, facts: &EnvFacts) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(want_os) = &spec.env_assumptions.os {
        match &facts.os {
            Some(got) if got == want_os => {}
            Some(got) => reasons.push(format!(
                "env assumption violated: spec requires os `{want_os}`, live os is `{got}`"
            )),
            None => reasons.push(format!(
                "env assumption unverifiable: spec requires os `{want_os}`, no os fact available"
            )),
        }
    }
    if let Some(want_arch) = &spec.env_assumptions.arch {
        match &facts.arch {
            Some(got) if got == want_arch => {}
            Some(got) => reasons.push(format!(
                "env assumption violated: spec requires arch `{want_arch}`, live arch is `{got}`"
            )),
            None => reasons.push(format!(
                "env assumption unverifiable: spec requires arch `{want_arch}`, no arch fact available"
            )),
        }
    }
    for (seat, constraint) in &spec.env_assumptions.toolchains {
        let (op, required) = match parse_constraint(constraint) {
            Some(c) => c,
            None => {
                reasons.push(format!(
                    "env assumption unparseable: toolchain `{seat}` constraint `{constraint}`"
                ));
                continue;
            }
        };
        let installed: Option<Vec<u64>> = match facts.toolchains.get(seat) {
            Some(v) => parse_version(v),
            None => toolchain_binary(seat).and_then(installed_version),
        };
        match installed {
            Some(v) => {
                if !constraint_holds(&v, op, &required) {
                    reasons.push(format!(
                        "env assumption violated: toolchain `{seat}` is {}.{}, spec requires `{constraint}`",
                        v.first().copied().unwrap_or(0),
                        v.get(1).copied().unwrap_or(0)
                    ));
                }
            }
            None => reasons.push(format!(
                "env assumption unverifiable: toolchain `{seat}` not found or version unreadable (spec requires `{constraint}`)"
            )),
        }
    }
    reasons
}

// ------------------------------------------------------------ artifact run ---

/// Run an artifact file in a scratch copy (compiled seats write a `.bin`
/// next to the artifact — never inside the bundle). Returns stdout.
fn run_artifact_copy(
    artifact: &Path,
    lang_id: &str,
    timeout: Duration,
) -> Result<(String, u64), check::RunFail> {
    let lang = langs::LANGS
        .iter()
        .find(|l| l.id == lang_id)
        .ok_or_else(|| check::RunFail {
            message: format!("unknown language `{lang_id}`"),
            toolchain_missing: false,
        })?;
    let scratch = std::env::temp_dir().join(format!(
        "cuni-admit-{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&scratch).map_err(|e| check::RunFail {
        message: format!("scratch dir: {e}"),
        toolchain_missing: false,
    })?;
    let copy = scratch.join(format!("artifact.{}", lang.ext));
    if let Err(e) = fs::copy(artifact, &copy) {
        let _ = fs::remove_dir_all(&scratch);
        return Err(check::RunFail {
            message: format!("copy artifact: {e}"),
            toolchain_missing: false,
        });
    }
    let t0 = Instant::now();
    let out = check::run_target(lang, &copy, timeout);
    let wall_ms = t0.elapsed().as_millis() as u64;
    let _ = fs::remove_dir_all(&scratch);
    out.map(|stdout| (stdout, wall_ms))
}

// ------------------------------------------------------------------ bundle ---

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("cuni bundle/admit: {msg} (try `cuni bundle --help` / `cuni admit --help`)");
    ExitCode::from(2)
}

fn print_bundle_help() {
    eprintln!(
        "\\
cuni bundle — spec-carrying artifacts (55-ideas B10 / #21).

  cuni bundle <file.cuni> --spec spec.json --out DIR [--seat rs]
            [--sign keyfile] [--timeout secs]
      Build a bundle: source + emitted artifact + spec + signed proof.
      --sign is REQUIRED (keypair from `cuni audit --gen-key`). Default
      seat is rs; default timeout 30s. Bundle creation re-verifies the
      artifact against the reference interpreter and the spec; on any
      failure no bundle is left behind. Refuses to overwrite a non-empty
      output directory.
  cuni admit <dir> [--env env.json] [--timeout secs]
      Admission controller: re-verify the bundle against live environment
      state. Refuses spec-less artifacts, tampered files, invalid
      signatures, stale proofs, violated environment assumptions, and
      live re-run divergence. Exit 0 ADMIT, 1 REFUSE, 2 usage error.
      --env supplies environment facts instead of live detection
      (os/arch/toolchain versions) for simulation.
"
    );
}

pub fn cmd_bundle(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_bundle_help();
        return ExitCode::SUCCESS;
    }
    let mut spec_path: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut seat = "rs".to_string();
    let mut sign_key: Option<String> = None;
    let mut timeout = Duration::from_secs(30);
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--spec" => {
                i += 1;
                spec_path = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out_dir = args.get(i).cloned();
            }
            "--seat" => {
                i += 1;
                seat = match args.get(i) {
                    Some(s) => s.clone(),
                    None => return usage_err("--seat requires an id (e.g. rs, py)"),
                };
            }
            "--sign" => {
                i += 1;
                sign_key = args.get(i).cloned();
            }
            "--timeout" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<u64>().ok()) {
                    Some(s) => timeout = Duration::from_secs(s),
                    None => return usage_err("--timeout requires seconds"),
                }
            }
            _ if a.starts_with('-') => {
                return usage_err(&format!("unknown flag `{a}`"));
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }
    if positional.is_empty() {
        return usage_err("missing <file.cuni>");
    }
    if positional.len() > 1 {
        return usage_err("one source file at a time");
    }
    let spec_path = match spec_path {
        Some(s) => s,
        None => return usage_err("--spec <spec.json> is required"),
    };
    let out_dir = match out_dir {
        Some(s) => PathBuf::from(s),
        None => return usage_err("--out <dir> is required"),
    };
    let sign_key = match sign_key {
        Some(s) => s,
        None => {
            return usage_err("--sign <keyfile> is required (unsigned bundles cannot be admitted; mint a keypair with `cuni audit --gen-key`)")
        }
    };
    let source_path = PathBuf::from(&positional[0]);

    // Load + validate everything before writing anything.
    let program = match check::load_program(&source_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cuni bundle: REFUSE — source does not load: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source_bytes = match fs::read(&source_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cuni bundle: REFUSE — cannot read {}: {e}", source_path.display());
            return ExitCode::FAILURE;
        }
    };
    let source_sha256 = sha256_hex(&source_bytes);
    let spec_text = match fs::read_to_string(&spec_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cuni bundle: REFUSE — cannot read spec {spec_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let spec: Spec = match serde_json::from_str(&spec_text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cuni bundle: REFUSE — spec {spec_path} is not valid JSON or violates the schema: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut reasons = validate_spec(&spec, &seat);
    // `entry` must be a defined function (or the top level).
    if spec.entry != "<top>" {
        let defined = program.items.iter().any(|i| match i {
            crate::ast::Item::Def(f) => f.name == spec.entry,
            _ => false,
        });
        if !defined {
            reasons.push(format!(
                "spec.entry `{}` is not a defined function (and not `<top>`)",
                spec.entry
            ));
        }
    }
    if !reasons.is_empty() {
        eprintln!("cuni bundle: REFUSE — spec invalid:");
        for r in &reasons {
            eprintln!("  {r}");
        }
        return ExitCode::FAILURE;
    }
    let lang = match langs::LANGS.iter().find(|l| l.id == seat) {
        Some(l) => *l,
        None => {
            eprintln!("cuni bundle: REFUSE — unknown seat `{seat}`");
            return ExitCode::FAILURE;
        }
    };
    let signing = match crate::audit::load_signing_key(Path::new(&sign_key)) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("cuni bundle: REFUSE — cannot load signing key: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Refuse to overwrite a non-empty output directory.
    if out_dir.exists() {
        match fs::read_dir(&out_dir) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    eprintln!(
                        "cuni bundle: REFUSE — output directory {} is not empty",
                        out_dir.display()
                    );
                    return ExitCode::FAILURE;
                }
            }
            Err(e) => {
                eprintln!("cuni bundle: REFUSE — cannot read {}: {e}", out_dir.display());
                return ExitCode::FAILURE;
            }
        }
    }
    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("cuni bundle: REFUSE — cannot create {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }
    // From here on, any failure removes the half-written bundle.
    let fail = |msg: &str| -> ExitCode {
        eprintln!("cuni bundle: REFUSE — {msg}");
        let _ = fs::remove_dir_all(&out_dir);
        ExitCode::FAILURE
    };

    // Emit the artifact.
    let artifact_name = format!("artifact.{}", lang.ext);
    let artifact_text = match emit::generate_exact(&program, &lang) {
        Ok(t) => t,
        Err(e) => return fail(&format!("seat `{seat}` refused to emit: {e}")),
    };
    if let Err(e) = fs::write(out_dir.join(&artifact_name), &artifact_text) {
        return fail(&format!("write artifact: {e}"));
    }
    if let Err(e) = fs::write(out_dir.join("source.cuni"), &source_bytes) {
        return fail(&format!("write source.cuni: {e}"));
    }
    let spec_json = serde_json::to_string_pretty(&spec).expect("spec serialization cannot fail");
    if let Err(e) = fs::write(out_dir.join("spec.json"), format!("{spec_json}\n")) {
        return fail(&format!("write spec.json: {e}"));
    }

    // Bundle-time proof: the artifact must behave exactly like the
    // reference interpreter, exit 0, and meet the spec's behavior + wall
    // budget — otherwise no bundle exists.
    let mut checks: Vec<ProofCheck> = vec![ProofCheck {
        name: "source loads (parse + typecheck)".to_string(),
        result: "pass".to_string(),
        detail: "check::load_program accepted the source".to_string(),
    }];
    let interp_out = match interp::run(&program) {
        Ok(o) => o,
        Err(e) => return fail(&format!("reference interpreter run failed: {e}")),
    };
    let interp_hash = sha256_hex(interp_out.as_bytes());
    checks.push(ProofCheck {
        name: "reference interpreter run".to_string(),
        result: "pass".to_string(),
        detail: format!("stdout sha256 {interp_hash}"),
    });
    let (artifact_out, wall_ms) =
        match run_artifact_copy(&out_dir.join(&artifact_name), &seat, timeout) {
            Ok(v) => v,
            Err(f) => {
                return fail(&format!(
                    "artifact run failed ({}): {}",
                    if f.toolchain_missing {
                        "toolchain missing"
                    } else {
                        "run error"
                    },
                    f.message
                ))
            }
        };
    let artifact_hash = sha256_hex(artifact_out.as_bytes());
    if artifact_out != interp_out {
        return fail(&format!(
            "artifact stdout diverges from the reference interpreter (artifact sha256 {artifact_hash} != interp sha256 {interp_hash})"
        ));
    }
    checks.push(ProofCheck {
        name: "artifact run matches reference interpreter".to_string(),
        result: "pass".to_string(),
        detail: format!("stdout sha256 {artifact_hash}, wall {wall_ms}ms"),
    });
    if let Some(want) = &spec.behavior.stdout_sha256 {
        if want != &artifact_hash {
            return fail(&format!(
                "spec.behavior.stdout_sha256 is {want} but the artifact prints {artifact_hash}"
            ));
        }
        checks.push(ProofCheck {
            name: "spec behavior stdout hash".to_string(),
            result: "pass".to_string(),
            detail: format!("artifact stdout matches spec {want}"),
        });
    }
    if let Some(max_ms) = spec.resource.max_wall_ms {
        if wall_ms > max_ms {
            return fail(&format!(
                "spec.resource.max_wall_ms is {max_ms}ms but the artifact run took {wall_ms}ms"
            ));
        }
        checks.push(ProofCheck {
            name: "spec resource wall budget".to_string(),
            result: "pass".to_string(),
            detail: format!("artifact run {wall_ms}ms <= {max_ms}ms"),
        });
    }

    let artifact_bytes = fs::read(out_dir.join(&artifact_name)).expect("artifact just written");
    let mut proof = Proof {
        format: PROOF_FORMAT.to_string(),
        binary_sha256: sha256_hex(&artifact_bytes),
        spec_sha256: sha256_hex(format!("{spec_json}\n").as_bytes()),
        source_sha256: source_sha256.clone(),
        cuni_version: env!("CARGO_PKG_VERSION").to_string(),
        issued_at: utc_timestamp(),
        checks,
        gold_stdout_sha256: artifact_hash,
        gold_exit_code: 0,
        signer_pubkey: None,
        signature: None,
    };
    sign_proof(&mut proof, &signing);
    let proof_json = serde_json::to_string_pretty(&proof).expect("proof serialization cannot fail");
    if let Err(e) = fs::write(out_dir.join("proof.json"), format!("{proof_json}\n")) {
        return fail(&format!("write proof.json: {e}"));
    }

    let mut files = BTreeMap::new();
    for name in ["source.cuni", artifact_name.as_str(), "spec.json", "proof.json"] {
        let bytes = match fs::read(out_dir.join(name)) {
            Ok(b) => b,
            Err(e) => return fail(&format!("hash {name}: {e}")),
        };
        files.insert(name.to_string(), sha256_hex(&bytes));
    }
    let manifest = BundleManifest {
        format: BUNDLE_FORMAT.to_string(),
        name: spec.name.clone(),
        cuni_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: utc_timestamp(),
        seat: seat.clone(),
        source_sha256,
        files,
    };
    let manifest_json =
        serde_json::to_string_pretty(&manifest).expect("manifest serialization cannot fail");
    if let Err(e) = fs::write(out_dir.join("manifest.json"), format!("{manifest_json}\n")) {
        return fail(&format!("write manifest.json: {e}"));
    }
    println!(
        "cuni bundle: wrote {} (seat {seat}, artifact {artifact_name}, {} proof checks, signed)",
        out_dir.display(),
        proof.checks.len()
    );
    ExitCode::SUCCESS
}

pub fn cmd_admit(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_bundle_help();
        return ExitCode::SUCCESS;
    }
    let mut env_path: Option<String> = None;
    let mut timeout = Duration::from_secs(30);
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--env" => {
                i += 1;
                env_path = args.get(i).cloned();
            }
            "--timeout" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<u64>().ok()) {
                    Some(s) => timeout = Duration::from_secs(s),
                    None => return usage_err("--timeout requires seconds"),
                }
            }
            _ if a.starts_with('-') => {
                return usage_err(&format!("unknown flag `{a}`"));
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }
    if positional.is_empty() {
        return usage_err("missing <bundle dir>");
    }
    if positional.len() > 1 {
        return usage_err("one bundle dir at a time");
    }
    let dir = PathBuf::from(&positional[0]);
    let mut reasons: Vec<String> = Vec::new();

    let refuse = |reasons: &[String]| -> ExitCode {
        eprintln!("cuni admit: REFUSE {} ({} reason(s)):", dir.display(), reasons.len());
        for r in reasons {
            eprintln!("  - {r}");
        }
        ExitCode::FAILURE
    };

    // 1. Manifest.
    let manifest_text = match fs::read_to_string(dir.join("manifest.json")) {
        Ok(t) => t,
        Err(_) => {
            return refuse(&["no manifest.json — not a CuNi bundle".to_string()]);
        }
    };
    let manifest: BundleManifest = match serde_json::from_str(&manifest_text) {
        Ok(m) => m,
        Err(e) => {
            return refuse(&[format!("manifest.json is not valid JSON: {e}")]);
        }
    };
    if manifest.format != BUNDLE_FORMAT {
        return refuse(&[format!(
            "manifest format is `{}`, expected `{BUNDLE_FORMAT}`",
            manifest.format
        )]);
    }

    // 2. Spec-less artifacts are refused, full stop.
    if !dir.join("spec.json").exists() {
        return refuse(&["spec-less artifact: no spec.json in the bundle".to_string()]);
    }
    if !dir.join("proof.json").exists() {
        return refuse(&["no proof.json in the bundle".to_string()]);
    }
    if !dir.join("source.cuni").exists() {
        return refuse(&["no source.cuni in the bundle".to_string()]);
    }

    // 3. Every listed file must exist with the recorded hash.
    for (name, want) in &manifest.files {
        let bytes = match fs::read(dir.join(name)) {
            Ok(b) => b,
            Err(_) => {
                reasons.push(format!("manifest lists `{name}` but the file is missing"));
                continue;
            }
        };
        if &sha256_hex(&bytes) != want {
            reasons.push(format!(
                "file `{name}` hash mismatch (tampered or stale; manifest says {want})"
            ));
        }
    }
    if !reasons.is_empty() {
        return refuse(&reasons);
    }
    let artifact_name = match manifest.files.keys().find(|k| k.starts_with("artifact.")) {
        Some(k) => k.clone(),
        None => {
            return refuse(&["manifest lists no artifact.* file".to_string()]);
        }
    };

    // 4. Proof binds binary + spec + source.
    let proof_text = match fs::read_to_string(dir.join("proof.json")) {
        Ok(t) => t,
        Err(e) => return refuse(&[format!("cannot read proof.json: {e}")]),
    };
    let proof: Proof = match serde_json::from_str(&proof_text) {
        Ok(p) => p,
        Err(e) => return refuse(&[format!("proof.json is not valid JSON: {e}")]),
    };
    if proof.format != PROOF_FORMAT {
        reasons.push(format!(
            "proof format is `{}`, expected `{PROOF_FORMAT}`",
            proof.format
        ));
    }
    let artifact_bytes = fs::read(dir.join(&artifact_name)).unwrap_or_default();
    if proof.binary_sha256 != sha256_hex(&artifact_bytes) {
        reasons.push(
            "proof invalidated: artifact bytes no longer match proof.binary_sha256 (binary changed after bundling)"
                .to_string(),
        );
    }
    let spec_bytes = fs::read(dir.join("spec.json")).unwrap_or_default();
    if proof.spec_sha256 != sha256_hex(&spec_bytes) {
        reasons.push(
            "proof invalidated: spec.json no longer matches proof.spec_sha256 (spec edited after bundling)"
                .to_string(),
        );
    }
    let source_bytes = fs::read(dir.join("source.cuni")).unwrap_or_default();
    if proof.source_sha256 != sha256_hex(&source_bytes) {
        reasons.push(
            "proof invalidated: source.cuni no longer matches proof.source_sha256 (source changed after bundling)"
                .to_string(),
        );
    }
    if proof.source_sha256 != manifest.source_sha256 {
        reasons.push("proof.source_sha256 disagrees with manifest.source_sha256".to_string());
    }
    if !reasons.is_empty() {
        return refuse(&reasons);
    }

    // 5. Signature.
    match verify_proof(&proof) {
        Ok(true) => {}
        Ok(false) => {
            return refuse(&["proof is unsigned — admission requires a signed proof".to_string()])
        }
        Err(e) => return refuse(&[format!("proof signature invalid: {e}")]),
    }

    // 6. Spec schema (strict — same validation as bundle time).
    let spec: Spec = match serde_json::from_slice(&spec_bytes) {
        Ok(s) => s,
        Err(e) => return refuse(&[format!("spec.json violates the schema: {e}")]),
    };
    for r in validate_spec(&spec, &manifest.seat) {
        reasons.push(format!("spec invalid: {r}"));
    }
    if !reasons.is_empty() {
        return refuse(&reasons);
    }

    // 7. Live environment assumptions.
    let facts: EnvFacts = match &env_path {
        Some(p) => match fs::read_to_string(p) {
            Ok(t) => match serde_json::from_str(&t) {
                Ok(f) => f,
                Err(e) => return refuse(&[format!("--env {p} is not valid JSON: {e}")]),
            },
            Err(e) => return refuse(&[format!("cannot read --env {p}: {e}")]),
        },
        None => live_env_facts(),
    };
    for r in check_env_assumptions(&spec, &facts) {
        reasons.push(r);
    }
    if !reasons.is_empty() {
        return refuse(&reasons);
    }

    // 8. Live re-verification: the source must still load, and the artifact
    // must still behave exactly as the proof's gold stdout says — re-run
    // both now, against live state, not CI's memory of it.
    let program = match check::load_program(&dir.join("source.cuni")) {
        Ok(p) => p,
        Err(e) => return refuse(&[format!("live re-verification: bundled source no longer loads: {e}")]),
    };
    let interp_out = match interp::run(&program) {
        Ok(o) => o,
        Err(e) => return refuse(&[format!("live re-verification: reference interpreter run failed: {e}")]),
    };
    let interp_hash = sha256_hex(interp_out.as_bytes());
    if interp_hash != proof.gold_stdout_sha256 {
        reasons.push(format!(
            "live re-verification: reference interpreter stdout sha256 {interp_hash} != proof gold {}",
            proof.gold_stdout_sha256
        ));
    }
    let (artifact_out, wall_ms) =
        match run_artifact_copy(&dir.join(&artifact_name), &manifest.seat, timeout) {
            Ok(v) => v,
            Err(f) => {
                return refuse(&[format!(
                    "live re-verification: artifact run failed ({}): {}",
                    if f.toolchain_missing {
                        "toolchain missing"
                    } else {
                        "run error"
                    },
                    f.message
                )])
            }
        };
    let artifact_hash = sha256_hex(artifact_out.as_bytes());
    if artifact_hash != proof.gold_stdout_sha256 {
        reasons.push(format!(
            "live re-verification: artifact stdout sha256 {artifact_hash} != proof gold {}",
            proof.gold_stdout_sha256
        ));
    }
    if artifact_out != interp_out {
        reasons.push(
            "live re-verification: artifact stdout diverges from the reference interpreter"
                .to_string(),
        );
    }
    if let Some(want) = &spec.behavior.stdout_sha256 {
        if want != &artifact_hash {
            reasons.push(format!(
                "live re-verification: artifact stdout sha256 {artifact_hash} != spec.behavior.stdout_sha256 {want}"
            ));
        }
    }
    if let Some(max_ms) = spec.resource.max_wall_ms {
        if wall_ms > max_ms {
            reasons.push(format!(
                "live re-verification: artifact run took {wall_ms}ms > spec.resource.max_wall_ms {max_ms}ms"
            ));
        }
    }
    if !reasons.is_empty() {
        return refuse(&reasons);
    }

    println!(
        "cuni admit: ADMIT {} (seat {}, proof verified, {} checks, artifact re-run {}ms, stdout sha256 {})",
        dir.display(),
        manifest.seat,
        proof.checks.len(),
        wall_ms,
        artifact_hash
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_toolchain(seat: &str, constraint: &str) -> Spec {
        let mut toolchains = BTreeMap::new();
        toolchains.insert(seat.to_string(), constraint.to_string());
        Spec {
            format: SPEC_FORMAT.to_string(),
            name: "t".to_string(),
            version: "1".to_string(),
            entry: "<top>".to_string(),
            seats: vec!["rs".to_string()],
            env_assumptions: EnvAssumptions {
                os: None,
                arch: None,
                toolchains,
            },
            behavior: Behavior::default(),
            resource: Resource::default(),
        }
    }

    #[test]
    fn constraint_parsing() {
        assert_eq!(parse_constraint(">=1.75"), Some((Op::Ge, vec![1, 75])));
        assert_eq!(parse_constraint("1.75"), Some((Op::Ge, vec![1, 75])));
        assert_eq!(parse_constraint("=1.75.0"), Some((Op::Eq, vec![1, 75, 0])));
        assert_eq!(parse_constraint("garbage"), None);
        assert_eq!(parse_constraint(">=x"), None);
    }

    #[test]
    fn constraint_holds_cases() {
        assert!(constraint_holds(&[1, 75, 0], Op::Ge, &[1, 75]));
        assert!(!constraint_holds(&[1, 74], Op::Ge, &[1, 75]));
        assert!(constraint_holds(&[2], Op::Gt, &[1, 99]));
        assert!(constraint_holds(&[1, 75, 0], Op::Eq, &[1, 75]));
        assert!(!constraint_holds(&[1, 75, 1], Op::Eq, &[1, 75]));
        assert!(constraint_holds(&[1, 75], Op::Le, &[1, 75, 9]));
    }

    #[test]
    fn spec_validation_rejects_unknown_toolchain_seat() {
        let s = spec_with_toolchain("nosuchseat", ">=1.0");
        let r = validate_spec(&s, "rs");
        assert!(r.iter().any(|x| x.contains("unknown toolchain seat")));
    }

    #[test]
    fn spec_validation_rejects_seat_mismatch() {
        let s = spec_with_toolchain("rs", ">=1.0");
        let mut s2 = s.clone();
        s2.seats = vec!["py".to_string()];
        let r = validate_spec(&s2, "rs");
        assert!(r.iter().any(|x| x.contains("does not contain the bundle seat")));
    }

    #[test]
    fn spec_schema_rejects_unknown_fields() {
        let json = r#"{"format":"cuni-spec/1","name":"t","version":"1","entry":"<top>","seats":["rs"],"surprise":1}"#;
        let err = serde_json::from_str::<Spec>(json).expect_err("unknown field must fail");
        assert!(err.to_string().contains("surprise"), "{err}");
    }

    #[test]
    fn proof_signature_roundtrip_and_tamper() {
        let mut proof = Proof {
            format: PROOF_FORMAT.to_string(),
            binary_sha256: "a".repeat(64),
            spec_sha256: "b".repeat(64),
            source_sha256: "c".repeat(64),
            cuni_version: "0.10.1".to_string(),
            issued_at: "2026-11-18T00:00:00Z".to_string(),
            checks: vec![ProofCheck {
                name: "x".to_string(),
                result: "pass".to_string(),
                detail: "d".to_string(),
            }],
            gold_stdout_sha256: "d".repeat(64),
            gold_exit_code: 0,
            signer_pubkey: None,
            signature: None,
        };
        assert_eq!(verify_proof(&proof), Ok(false));
        let mut secret = [7u8; 32];
        let signing = SigningKey::from_bytes(&secret);
        secret.fill(0);
        sign_proof(&mut proof, &signing);
        assert_eq!(verify_proof(&proof), Ok(true));
        // Tamper with a bound hash: signature must fail.
        proof.binary_sha256 = "e".repeat(64);
        assert!(verify_proof(&proof).is_err());
    }

    #[test]
    fn env_assumption_os_mismatch() {
        let mut s = spec_with_toolchain("rs", ">=1.0");
        s.env_assumptions.os = Some("windows".to_string());
        let facts = live_env_facts();
        let r = check_env_assumptions(&s, &facts);
        assert!(r.iter().any(|x| x.contains("requires os `windows`")));
    }
}
