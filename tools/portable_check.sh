#!/usr/bin/env bash
# The portability gate (docs/decisions.md, "Platforms"). Run it before each merge.
#
#   tools/portable_check.sh          # the source scan, then `cargo check` for Windows and macOS
#   tools/portable_check.sh --scan   # the source scan only
#
# 1. Source scan: OS code (`cfg(unix|windows|target_os)`, `/proc/`, `/etc/`, `/dev/`, `/usr/` paths, `extern "C"`,
#    `std::os::*`) may be only in the OS layer. Unit test files (`*_tests.rs`) and `tests/` are not scanned.
# 2. `cargo check --workspace --all-targets` for x86_64-pc-windows-msvc (through `cargo xwin`) and aarch64-apple-darwin (with clang
#    for the C parts of `psm`; there is no macOS SDK here). A check proves that the code builds, not that it runs there.
set -euo pipefail
cd "$(dirname "$0")/.."

pat='cfg\((all\(|not\()?(unix|windows|target_os)|"/(proc|etc|dev|usr)/|extern "C"|std::os::(unix|linux|windows|macos)'
# The OS layer: these files may call the OS.
layer='^crates/window/src/(wayland|none|lib)\.rs$'
# OS code that the next topic moves into the OS layer. Remove a file when it is clean. Do not add files.
known='^crates/(bare/src/main|bench/src/main|drive/src/main|drive/src/run|js/src/eval_intl|js/src/eval_numeric|net/src/dns|net/src/parallel|net/src/tcp|tls/src/random|tls/src/trust_store)\.rs$'

bad=$(grep -rlE "$pat" crates/*/src --include='*.rs' | grep -vE '_tests\.rs$' | grep -vE "$layer" | grep -vE "$known" || true)
if [ -n "$bad" ]; then
    echo "portable_check: OS code outside the OS layer:" >&2
    for f in $bad; do grep -nE "$pat" "$f" | sed "s|^|  $f:|" >&2; done
    exit 1
fi
stale=$(echo "$known" | grep -oE '[a-z]+/src/[a-z_]+' | while read -r f; do grep -qE "$pat" "crates/$f.rs" || echo "crates/$f.rs"; done)
if [ -n "$stale" ]; then
    echo "portable_check: these files have no OS code now; remove them from 'known':" $stale >&2
    exit 1
fi
echo "portable_check: scan ok"
[ "${1:-}" = "--scan" ] && exit 0

cargo xwin check --quiet --target x86_64-pc-windows-msvc --workspace --all-targets
echo "portable_check: windows ok"
CC_aarch64_apple_darwin=clang CFLAGS_aarch64_apple_darwin=--target=aarch64-apple-darwin AR_aarch64_apple_darwin=llvm-ar \
    cargo check --quiet --target aarch64-apple-darwin --workspace --all-targets
echo "portable_check: macos ok"
