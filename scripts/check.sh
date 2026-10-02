#!/bin/sh
# Runs locally what CI runs remotely, in the same order, and stops at the first
# failure. The pre-commit hook calls this, so a commit that would turn CI red
# never gets made.
set -eu

cd "$(dirname "$0")/.."

# A warning that nobody reads is a warning that does not exist.
export RUSTFLAGS="-D warnings"

echo "==> Formatting"
cargo fmt --all -- --check

echo "==> Lints"
cargo clippy --workspace --all-targets --all-features --quiet

echo "==> Documentation"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --quiet

echo "==> Tests"
cargo test --workspace --all-features --quiet

echo "==> Dependencies"
if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check
else
    # Not fatal, so that a fresh clone can commit before installing it, but
    # said out loud, so that nobody believes the audit ran.
    echo "cargo-deny is not installed, skipping (cargo install --locked cargo-deny)"
fi

echo "All checks passed."
