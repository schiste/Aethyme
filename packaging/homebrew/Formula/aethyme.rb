# frozen_string_literal: true

# The paired Aethyme router and engine daemon.
class Aethyme < Formula
  desc "Local-first flight control for concurrent AI coding agents"
  homepage "https://github.com/schiste/Aethyme"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/schiste/Aethyme/releases/download/v0.2.0/aethyme-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "aae4347e88c13ff37d33f718520a9af1d8335c02ff75da1cfcb448deffdece73"
    end

    on_intel do
      url "https://github.com/schiste/Aethyme/releases/download/v0.2.0/aethyme-v0.2.0-x86_64-apple-darwin.tar.gz"
      sha256 "aa76b871679a74af8aa0e3973a54775b5a9dc20d5ee9396faeb79c0f4dbe87ef"
    end
  end

  on_linux do
    depends_on arch: :x86_64

    on_intel do
      url "https://github.com/schiste/Aethyme/releases/download/v0.2.0/aethyme-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cf9b2a0c946308e5d8989f610286d3c4b19a4747681be0fb1a201caf15f8e033"
    end
  end

  def install
    bin.install "aethyme", "aethyme-engine-cli"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/aethyme --version")
    assert_match version.to_s, shell_output("#{bin}/aethyme-engine-cli --version")
    system bin/"aethyme", "broker", "quick-test"
  end
end
