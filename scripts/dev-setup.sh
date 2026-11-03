#!/usr/bin/env bash
# Sets up Ubuntu or Debian (including WSL2 on Windows) to build and test Strata, then runs the
# same checks as CI. Safe to run again: it skips what is already installed.
#
#   scripts/dev-setup.sh            # install, then fmt + clippy + tests
#   scripts/dev-setup.sh --no-test  # install only
set -euo pipefail
cd "$(dirname "$0")/.."
step() { printf '\n== %s\n' "$*"; }

case "$PWD" in
  /mnt/*)
    echo "This checkout is on a Windows drive ($PWD). Clone it into your Linux home instead:" >&2
    echo "  cd ~ && git clone https://github.com/C01-Dev/Strata && cd Strata && scripts/dev-setup.sh" >&2
    echo "Windows drives are slow from WSL and do not behave like Linux disks." >&2
    exit 1 ;;
esac
if ! command -v apt-get >/dev/null; then
  echo "This script uses apt (Ubuntu or Debian). Elsewhere, install git, a C compiler, curl, xz," >&2
  echo "rustup and Node.js 20 or later yourself, then run: npm ci --prefix packages/tsbridge" >&2
  exit 1
fi

step "System packages (asks for your password)"
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential git curl xz-utils perl

step "Rust"
if ! command -v rustup >/dev/null && [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup show >/dev/null # installs the toolchain rust-toolchain.toml pins, with rustfmt and clippy
rustc --version

step "Node.js (20 or later)"
major=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)
if [ "$major" -lt 20 ]; then
  curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
  sudo apt-get install -y -qq nodejs
fi
node --version

step "TypeScript for the shared checker (packages/tsbridge)"
npm ci --prefix packages/tsbridge --silent --no-audit --no-fund

if grep -qi microsoft /proc/version 2>/dev/null && [ "$(ps -p 1 -o comm= 2>/dev/null)" != "systemd" ]; then
  step "WSL note"
  echo "systemd is off in this WSL distro, so 'strata init' cannot install its background service."
  echo "Use 'strata init --no-service', or turn systemd on: add these two lines to /etc/wsl.conf,"
  echo "then run 'wsl --shutdown' in PowerShell and reopen Ubuntu:"
  printf '  [boot]\n  systemd=true\n'
fi

if [ "${1:-}" = "--no-test" ]; then
  echo; echo "Installed. Run the checks any time with: cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test"
  exit 0
fi

step "The same checks as CI (the first build takes a few minutes)"
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
echo; echo "All set. See CONTRIBUTING.md for what to work on and how to send it."
