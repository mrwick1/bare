#!/bin/sh
# Reproducible 1x / 10x / 100x run of the crypto stress cases. One process per case,
# a time limit and a memory limit (ulimit -v, 512 MiB) for each. Run from the repo root.
#   sh crates/tls/examples/run_scales.sh
set -e
cargo build --release -q -p bare-tls --example crypto_stress
E=target/release/examples/crypto_stress
ulimit -v 524288
MIB=1048576
for x in 1 10 100; do
  for c in sha hmac; do timeout 300 $E $c $((64 * MIB * x)); done
  timeout 300 $E hkdf $((2000 * x))
  timeout 300 $E small $((100000 * x))
done
# Chunk size: many tiny updates against one big one (64 MiB total).
for chunk in 1 7 64 1000 65536; do timeout 300 $E sha $((64 * MIB)) $chunk; done
