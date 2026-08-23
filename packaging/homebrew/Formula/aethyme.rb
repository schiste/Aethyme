# frozen_string_literal: true

# The paired Aethyme router and engine daemon.
class Aethyme < Formula
  desc "Local-first flight control for concurrent AI coding agents"
  homepage "https://github.com/schiste/Aethyme"
  version "0.1.5"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/schiste/Aethyme/releases/download/v0.1.5/aethyme-v0.1.5-aarch64-apple-darwin.tar.gz"
      sha256 "6bf7e26bee5c88842de589fecb8c2d5d0aed7aff2c19d15c055ca154341a7811"
    end

    on_intel do
      url "https://github.com/schiste/Aethyme/releases/download/v0.1.5/aethyme-v0.1.5-x86_64-apple-darwin.tar.gz"
      sha256 "2f1881ac2e9d2f731cab66fbc9125da49125a2c6fa560de01ac76b01789ca0eb"
    end
  end

  on_linux do
    depends_on arch: :x86_64

    on_intel do
      url "https://github.com/schiste/Aethyme/releases/download/v0.1.5/aethyme-v0.1.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4e9cc67c632950feb47edd1b6ec85cc998359b31e233a0ee9d8ceff928099cfd"
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
