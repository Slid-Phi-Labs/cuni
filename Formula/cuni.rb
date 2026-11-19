# SPDX-License-Identifier: AGPL-3.0-or-later
# Homebrew formula for CuNi — installs the prebuilt release binary, NOT a
# source/cargo build (agents and CI should not need rustup).
#
# PER-RELEASE CHECKLIST (do this every release):
#   1. Bump `version` below to the new version.
#   2. After the `release` GitHub workflow finishes, fill in each `sha256`
#      from the release's CHECKSUMS.txt
#      (https://github.com/ceedot-rock/cuni/releases/download/v<version>/CHECKSUMS.txt).
#   3. `brew audit --strict cuni` and `brew test cuni` before pushing.
#
# NOTE: this file lives in the main repo for now. Once a tap exists
# (github.com/ceedot-rock/homebrew-tap — NOT created yet), move it there
# and point install docs at `brew install ceedot-rock/tap/cuni`.
class Cuni < Formula
  desc "CuNi (Code:uNiTY) — one source, exact on every target or refuse"
  homepage "https://github.com/ceedot-rock/cuni"
  version "0.10.2"
  license "AGPL-3.0-or-later"

  # UPDATE PER RELEASE — see checklist above.
  on_macos do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-aarch64-apple-darwin.tar.gz"
      sha256 "2888e2e07a6b99aa4e4f5661e7b1df91b41fac3caeab1eb87404f785d3362db8"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-x86_64-apple-darwin.tar.gz"
      sha256 "86e3098bce55d6bc8d0c32af21e039187be74e3df658b5a0ed51c8d8d55a5940"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0b82543b0eda7915e983345e5ed7fbea48953822f5408033fa24f02ac722ec17"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1e4998b5b5852c1f4d79b307ae73e0ed9bce17cf73f3122aa3f9edd5dde77905"
    end
  end

  def install
    bin.install "cuni"
  end

  test do
    assert_match "cuni #{version}", shell_output("#{bin}/cuni version")
  end
end
