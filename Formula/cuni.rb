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
  version "0.10.1"
  license "AGPL-3.0-or-later"

  # UPDATE PER RELEASE — see checklist above.
  on_macos do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.1/cuni-0.10.1-aarch64-apple-darwin.tar.gz"
      sha256 "2407a58430a34b1886a5f01fb05ca264cae54084bab6d1e6c759ac615ae9b40d"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.1/cuni-0.10.1-x86_64-apple-darwin.tar.gz"
      sha256 "022eb305fe4100bbf21305e6fc6957f78fe3b7a4d6f2db77a0a90f072e836a15"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.1/cuni-0.10.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "52a26cbcc0f88820327b6938b0e97ed3da9eddf9c3b8fd2c2563ffce875bd468"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.1/cuni-0.10.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "873521d5c8dfd83b45efeca85ead4dceb271763fb65066271cd1cd616a250069"
    end
  end

  def install
    bin.install "cuni"
  end

  test do
    assert_match "cuni #{version}", shell_output("#{bin}/cuni version")
  end
end
