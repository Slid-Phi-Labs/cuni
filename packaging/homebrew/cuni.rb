# frozen_string_literal: true

# CuNi — prebuilt binary formula (no Rust toolchain needed).
#
# Install:
#   brew tap ceedot-rock/cuni && brew install cuni
#
# Per-release checklist (maintainer):
#   1. Tag vX.Y.Z on ceedot-rock/cuni → release.yml publishes
#      cuni-X.Y.Z-{aarch64,x86_64}-{apple-darwin,unknown-linux-gnu}.tar.gz + CHECKSUMS.txt
#   2. Bump `version` below and paste the four sha256 lines from CHECKSUMS.txt
#   3. Commit + push here.

class Cuni < Formula
  desc "CuNi — write once, print many languages; Python/Go/JS must match"
  homepage "https://spl-cuni-studio.fly.dev/"
  version "0.10.2"
  license "AGPL-3.0-or-later"

  on_macos do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-aarch64-apple-darwin.tar.gz"
      sha256 "2407a58430a34b1886a5f01fb05ca264cae54084bab6d1e6c759ac615ae9b40d"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-x86_64-apple-darwin.tar.gz"
      sha256 "022eb305fe4100bbf21305e6fc6957f78fe3b7a4d6f2db77a0a90f072e836a15"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "52a26cbcc0f88820327b6938b0e97ed3da9eddf9c3b8fd2c2563ffce875bd468"
    end
    on_intel do
      url "https://github.com/ceedot-rock/cuni/releases/download/v0.10.2/cuni-0.10.2-x86_64-unknown-linux-gnu.tar.gz"
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
