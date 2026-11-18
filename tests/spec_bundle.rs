//! Black-box tests for `cuni bundle` / `cuni admit` — spec-carrying
//! artifacts (55-ideas B10 / #21).
//!
//! Every test really bundles and really admits: the artifact is emitted and
//! run with the real `rustc`, the proof is Ed25519-signed with a freshly
//! minted keypair, and admission re-runs the artifact against live state.
//! Tamper cases mutate the bundle on disk after creation, the way a stale
//! or hostile artifact would arrive at admission.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cuni_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cuni"))
}

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
        "cuni_bundle_{}_{}_{}_{:?}",
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

fn write_file(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// sha256 of a file via the system `sha256sum` (tests have no sha2 dep).
fn sha256_hex_file(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum must exist");
    assert!(out.status.success(), "sha256sum failed on {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// Scratch dir with hello.cuni + a spec, plus a freshly minted keypair.
/// Returns (dir, source, spec, bundle_dir, keyfile).
fn setup(spec_json: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = scratch_dir("setup");
    let src_text = std::fs::read_to_string("tests/fixtures-bundle/hello.cuni")
        .expect("missing fixture tests/fixtures-bundle/hello.cuni");
    let src = dir.join("hello.cuni");
    write_file(&src, &src_text);
    let spec = dir.join("spec.json");
    write_file(&spec, spec_json);
    let (ok, _, stderr) = run_cuni(&["audit", "--gen-key", "bundletest"], &dir);
    assert!(ok, "keygen must succeed: {stderr}");
    let key = dir.join("bundletest.key");
    assert!(key.exists(), "key file must exist");
    let bundle = dir.join("hello.cunibundle");
    (dir, src, spec, bundle, key)
}

const SPEC: &str = r#"{
  "format": "cuni-spec/1",
  "name": "hello-bundle",
  "version": "1.0.0",
  "entry": "double",
  "seats": ["rs"],
  "env_assumptions": {"os": "linux", "arch": "x86_64", "toolchains": {"rs": ">=1.75"}},
  "behavior": {"exit_code": 0},
  "resource": {"max_wall_ms": 60000}
}"#;

fn bundle(dir: &Path, src: &Path, spec: &Path, out: &Path, key: &Path) -> (bool, String, String) {
    run_cuni(
        &[
            "bundle",
            src.to_str().unwrap(),
            "--spec",
            spec.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--seat",
            "rs",
            "--sign",
            key.to_str().unwrap(),
        ],
        dir,
    )
}

fn admit(dir: &Path, bundle: &Path) -> (bool, String, String) {
    run_cuni(&["admit", bundle.to_str().unwrap()], dir)
}

/// A valid bundle passes admission: proof verifies, artifact re-runs,
/// stdout matches the gold hash.
#[test]
fn bundle_valid_admits() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, stdout, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed:\nstdout: {stdout}\nstderr: {stderr}");
    for f in ["manifest.json", "source.cuni", "artifact.rs", "spec.json", "proof.json"] {
        assert!(out.join(f).exists(), "bundle must contain {f}");
    }
    let proof: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("proof.json")).unwrap()).unwrap();
    assert_eq!(proof["format"], "cuni-proof/1");
    assert!(proof["signature"].as_str().unwrap().len() == 128, "proof must be signed");
    assert_eq!(proof["gold_exit_code"], 0);
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(ok, "valid bundle must be admitted:\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("ADMIT"), "admission must print ADMIT: {stdout}");
}

/// A spec edit after bundling invalidates the proof: admission catches the
/// stale spec at the manifest hash check, it does not silently admit the
/// new spec.
#[test]
fn bundle_stale_spec_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let mut text = std::fs::read_to_string(out.join("spec.json")).unwrap();
    text = text.replace("\"1.0.0\"", "\"1.0.1\"");
    assert_ne!(text, std::fs::read_to_string(out.join("spec.json")).unwrap());
    std::fs::write(out.join("spec.json"), text).unwrap();
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "stale spec must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("file `spec.json` hash mismatch"),
        "admission must blame the spec hash:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The proof binding is a second layer: even if the manifest is re-hashed
/// to match an edited spec, the proof still invalidates it.
#[test]
fn bundle_stale_proof_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let mut text = std::fs::read_to_string(out.join("spec.json")).unwrap();
    text = text.replace("\"1.0.0\"", "\"1.0.1\"");
    std::fs::write(out.join("spec.json"), text).unwrap();
    // Attacker re-hashes the manifest to match the edited spec...
    let new_hash = sha256_hex_file(&out.join("spec.json"));
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap())
            .unwrap();
    manifest["files"]["spec.json"] = serde_json::Value::String(new_hash);
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    // ...but the proof still binds the ORIGINAL spec hash.
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "re-hashed stale spec must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("spec.json no longer matches proof.spec_sha256"),
        "admission must blame the proof binding:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A spec-less artifact is refused at admission, full stop.
#[test]
fn bundle_specless_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    std::fs::remove_file(out.join("spec.json")).unwrap();
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "spec-less artifact must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("spec-less artifact"),
        "admission must say spec-less:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A binary change after bundling invalidates the proof: admission catches
/// the tampered artifact at the manifest hash check.
#[test]
fn bundle_binary_tamper_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let mut text = std::fs::read_to_string(out.join("artifact.rs")).unwrap();
    text.push_str("\n// tampered after bundling\n");
    std::fs::write(out.join("artifact.rs"), text).unwrap();
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "tampered binary must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("file `artifact.rs` hash mismatch"),
        "admission must blame the binary hash:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The proof binding is a second layer for the binary too: even if the
/// manifest is re-hashed to match a tampered artifact, the proof still
/// invalidates it.
#[test]
fn bundle_binary_proof_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let mut text = std::fs::read_to_string(out.join("artifact.rs")).unwrap();
    text.push_str("\n// tampered after bundling\n");
    std::fs::write(out.join("artifact.rs"), text).unwrap();
    let new_hash = sha256_hex_file(&out.join("artifact.rs"));
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap())
            .unwrap();
    manifest["files"]["artifact.rs"] = serde_json::Value::String(new_hash);
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "re-hashed tampered binary must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}")
            .contains("artifact bytes no longer match proof.binary_sha256"),
        "admission must blame the proof binding:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A tampered signature is refused, not ignored.
#[test]
fn bundle_tampered_signature_refused() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let mut proof: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("proof.json")).unwrap()).unwrap();
    let sig = proof["signature"].as_str().unwrap().to_string();
    let flipped = format!("0{}", &sig[1..]);
    assert_ne!(flipped, sig);
    proof["signature"] = serde_json::Value::String(flipped);
    std::fs::write(
        out.join("proof.json"),
        serde_json::to_string_pretty(&proof).unwrap(),
    )
    .unwrap();
    // Re-hash the manifest so admission reaches the signature check itself
    // (otherwise the manifest hash mismatch fires first — also a refusal,
    // but it would not exercise signature verification).
    let new_hash = sha256_hex_file(&out.join("proof.json"));
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap())
            .unwrap();
    manifest["files"]["proof.json"] = serde_json::Value::String(new_hash);
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "tampered signature must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("signature"),
        "admission must blame the signature:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A spec whose environment assumptions do not hold live is refused — the
/// admission controller checks assumptions against live state, not CI's.
#[test]
fn bundle_env_violation_refused() {
    let spec_win = SPEC.replace("\"linux\"", "\"windows\"");
    let (dir, src, spec, out, key) = setup(&spec_win);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed (env is checked at admission, not build): {stderr}");
    let (ok, stdout, stderr) = admit(&dir, &out);
    assert!(!ok, "windows-only spec must be refused on linux:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("requires os `windows`"),
        "admission must blame the os assumption:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Unsigned bundles cannot be created: --sign is required.
#[test]
fn bundle_requires_signature() {
    let (dir, src, spec, out, _key) = setup(SPEC);
    let (ok, stdout, stderr) = run_cuni(
        &[
            "bundle",
            src.to_str().unwrap(),
            "--spec",
            spec.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--seat",
            "rs",
        ],
        &dir,
    );
    assert!(!ok, "unsigned bundle must be refused:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("--sign"),
        "refusal must name --sign:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!out.join("proof.json").exists(), "no half-bundle may be left behind");
}

/// A spec that pins the expected stdout hash is enforced at bundle time
/// (wrong hash → no bundle) and at admission (right hash → ADMIT).
#[test]
fn bundle_spec_expected_stdout_enforced() {
    let (dir, src, spec, out, key) = setup(SPEC);
    let (ok, _, stderr) = bundle(&dir, &src, &spec, &out, &key);
    assert!(ok, "bundle must succeed: {stderr}");
    let proof: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("proof.json")).unwrap()).unwrap();
    let gold = proof["gold_stdout_sha256"].as_str().unwrap().to_string();
    // Right hash: a second bundle with the pinned hash admits.
    let spec_pinned = SPEC.replace(
        "\"behavior\": {\"exit_code\": 0}",
        &format!("\"behavior\": {{\"stdout_sha256\": \"{gold}\", \"exit_code\": 0}}"),
    );
    let spec2 = dir.join("spec-pinned.json");
    write_file(&spec2, &spec_pinned);
    let out2 = dir.join("hello-pinned.cunibundle");
    let (ok, stdout, stderr) = bundle(&dir, &src, &spec2, &out2, &key);
    assert!(ok, "pinned-hash bundle must succeed:\nstdout: {stdout}\nstderr: {stderr}");
    let (ok, stdout, stderr) = admit(&dir, &out2);
    assert!(ok, "pinned-hash bundle must be admitted:\nstdout: {stdout}\nstderr: {stderr}");
    // Wrong hash: bundle creation itself refuses.
    let spec_wrong = SPEC.replace(
        "\"behavior\": {\"exit_code\": 0}",
        "\"behavior\": {\"stdout_sha256\": \"0000000000000000000000000000000000000000000000000000000000000000\", \"exit_code\": 0}",
    );
    let spec3 = dir.join("spec-wrong.json");
    write_file(&spec3, &spec_wrong);
    let out3 = dir.join("hello-wrong.cunibundle");
    let (ok, stdout, stderr) = bundle(&dir, &src, &spec3, &out3, &key);
    assert!(!ok, "wrong-hash spec must fail bundle creation:\nstdout: {stdout}");
    assert!(
        format!("{stdout}\n{stderr}").contains("stdout_sha256"),
        "refusal must name stdout_sha256:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!out3.exists(), "no half-bundle may be left behind");
}
