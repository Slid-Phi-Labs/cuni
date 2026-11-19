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
