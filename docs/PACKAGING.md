# CuNi packaging (v0.10.2)

**Goal**: install `cuni` without cloning the repo.
**Status**: **v0.10.2**. The tag, `Cargo.toml`, crates.io, and the GitHub release all say 0.10.2. The release has prebuilt binaries for Linux and macOS on x86_64 and aarch64, plus `CHECKSUMS.txt`.

## One-command install

```bash
cargo install cuni
# or from git:
cargo install --git https://github.com/Slid-Phi-Labs/cuni --tag v0.10.2
```

Needs a Rust toolchain. After install, `cuni version` prints `cuni 0.10.2`.

## Prebuilt binaries (v0.10.2)

GitHub release [v0.10.2](https://github.com/Slid-Phi-Labs/cuni/releases/tag/v0.10.2) ships one tarball per target, each with a single `cuni` binary at the root:

| Target triple | Asset name |
|---------------|------------|
| `x86_64-unknown-linux-gnu` | `cuni-0.10.2-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-unknown-linux-gnu` | `cuni-0.10.2-aarch64-unknown-linux-gnu.tar.gz` |
| `x86_64-apple-darwin` | `cuni-0.10.2-x86_64-apple-darwin.tar.gz` |
| `aarch64-apple-darwin` | `cuni-0.10.2-aarch64-apple-darwin.tar.gz` |
| | `CHECKSUMS.txt` (sha256 for all four) |

```bash
curl -sL https://github.com/Slid-Phi-Labs/cuni/releases/download/v0.10.2/cuni-0.10.2-x86_64-unknown-linux-gnu.tar.gz \
  | tar -xz && ./cuni version
```

The release workflow (`.github/workflows/release.yml`) builds these on every `v*` tag.

## Local release binary

```bash
git clone https://github.com/Slid-Phi-Labs/cuni.git && cd cuni
cargo build --release
# → target/release/cuni
```

## cargo-binstall

`Cargo.toml` includes `[package.metadata.binstall]` pointing at:

```text
{repo}/releases/download/v{version}/cuni-{version}-{target}.tar.gz
```

```bash
cargo binstall --git https://github.com/Slid-Phi-Labs/cuni --version 0.10.2
```

## Homebrew

Formula: [`Formula/cuni.rb`](../Formula/cuni.rb) (copy in [`packaging/homebrew/cuni.rb`](../packaging/homebrew/cuni.rb)). It installs the prebuilt v0.10.2 binary, with sha256 values taken from the release's `CHECKSUMS.txt`. There is no tap yet; the formula is ready to move into one.
