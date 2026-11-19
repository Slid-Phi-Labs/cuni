//! One version, one count.
//!
//! The binary is the source of truth: `cuni version` gives the version and
//! `cuni --list-langs` gives the catalog. This test fails if the public
//! surfaces (README, Studio, protocol files, install files) say something
//! different, or if an old count (144 / 119 / 113) creeps back in.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cuni(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cuni"))
        .args(args)
        .output()
        .expect("run cuni");
    assert!(out.status.success(), "cuni {:?} failed", args);
    String::from_utf8(out.stdout).expect("utf8")
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn binary_version() -> String {
    let v = cuni(&["version"]);
    v.trim().strip_prefix("cuni ").expect("`cuni version` prints `cuni X.Y.Z`").to_string()
}

fn catalog_count() -> usize {
    cuni(&["--list-langs"]).lines().filter(|l| !l.trim().is_empty()).count()
}

#[test]
fn binary_matches_cargo_and_package() {
    let v = binary_version();
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
    let pkg = read("package.json");
    assert!(pkg.contains(&format!("\"version\": \"{v}\"")), "package.json version != {v}");
    let cff = read("CITATION.cff");
    assert!(cff.contains(&format!("version: {v}\n")), "CITATION.cff version != {v}");
}

/// Files that must name the current version.
const VERSIONED: &[&str] = &[
    "README.md",
    "Formula/cuni.rb",
    "packaging/homebrew/cuni.rb",
    "docs/PACKAGING.md",
    "playground/index.html",
    "playground/llms.txt",
    "skills/cuni/SKILL.md",
];

#[test]
fn public_files_name_the_current_version() {
    let v = binary_version();
    for f in VERSIONED {
        assert!(read(f).contains(&v), "{f} does not mention version {v}");
    }
    for f in ["Formula/cuni.rb", "packaging/homebrew/cuni.rb"] {
        assert!(
            read(f).contains(&format!("version \"{v}\"")),
            "{f}: formula version is not {v}"
        );
    }
    for f in ["protocol.json", "playground/protocol.json", "playground/.well-known/cuni-protocol.json"] {
        assert!(
            read(f).contains(&format!("\"public_install_tag\": \"v{v}\"")),
            "{f}: public_install_tag is not v{v}"
        );
    }
}

#[test]
fn protocol_files_carry_the_real_catalog_count() {
    let n = catalog_count();
    assert_eq!(n, 53, "catalog changed size; update this test and the public copy together");
    for f in ["protocol.json", "playground/protocol.json", "playground/.well-known/cuni-protocol.json"] {
        assert!(
            read(f).contains(&format!("\"catalog_languages\": {n}")),
            "{f}: catalog_languages is not {n}"
        );
    }
    assert_eq!(
        read("playground/protocol.json"),
        read("playground/.well-known/cuni-protocol.json"),
        "the two Studio protocol copies differ"
    );
}

/// Old counts that must not come back on current-facing surfaces.
/// History files (CHANGELOG, REMOVED_FAT, milestone and release notes) may
/// still quote them, so they are not scanned.
const STALE: &[&str] = &[
    "144 lang", "144-lang", "144 catalog", "144 target", "144 seat",
    "119 lang", "119-lang", "119 catalog", "119 target",
    "113 lang", "113 catalog", "113 target",
];

const CURRENT: &[&str] = &[
    "README.md",
    "llms.txt",
    "protocol.json",
    "playground/index.html",
    "playground/llms.txt",
    "playground/agents.json",
    "playground/agents.txt",
    "playground/protocol.json",
    "playground/protocol.html",
    "playground/.well-known/cuni-protocol.json",
    "playground/server.py",
    "skills/cuni/SKILL.md",
    "docs/STATUS.md",
    "docs/PACKAGING.md",
    "examples/demo-30s.sh",
    "scripts/run-compressors.sh",
    "scripts/render_demo_30s.py",
];

fn walk_examples(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_examples(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("cuni") {
            out.push(p);
        }
    }
}

#[test]
fn no_stale_counts_on_current_surfaces() {
    let mut files: Vec<PathBuf> = CURRENT.iter().map(|f| root().join(f)).collect();
    walk_examples(&root().join("examples"), &mut files);
    let mut bad = Vec::new();
    for p in files {
        let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        let lower = text.to_lowercase();
        for s in STALE {
            if lower.contains(s) {
                bad.push(format!("{}: \"{s}\"", p.strip_prefix(root()).unwrap().display()));
            }
        }
    }
    assert!(bad.is_empty(), "old catalog counts still on public surfaces:\n{}", bad.join("\n"));
}
