#!/usr/bin/env bash
set -euo pipefail

targets=(
  aarch64-apple-darwin
  x86_64-unknown-linux-gnu
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-unknown-freebsd
)

for target in "${targets[@]}"; do
  echo
  echo "==> Checking ${target}"
  cargo check \
    --workspace \
    --all-targets \
    --target "${target}"
done
