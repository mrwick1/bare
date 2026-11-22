#!/usr/bin/env bash
# Build one release executable with its target-specific code order.
set -euo pipefail
cd -- "$(dirname -- "$0")/../.."

target=x86_64-unknown-linux-gnu
plain=false
symbols=false
while [[ $# -gt 1 ]]; do
    case $1 in
        --plain) plain=true ;;
        --symbols) symbols=true ;;
        *) break ;;
    esac
    shift
done
case ${1:-} in
    bare) package=bare; kind=--bin ;;
    bare-drive) package=bare-drive; kind=--bin ;;
    script_run) package=bare-js2; kind=--example ;;
    *) echo "usage: $0 [--plain] [--symbols] {bare|bare-drive|script_run}" >&2; exit 2 ;;
esac
if [[ $# != 1 ]]; then
    exit 2
fi

compiler=$(rustc -vV)
host=$(sed -n 's/^host: //p' <<< "$compiler")
if [[ $host != "$target" ]]; then
    echo "Code order is available for $target. Use Cargo for $host." >&2
    exit 2
fi

args=(-C link-arg=-fuse-ld=lld)
if [[ $symbols == true ]]; then
    # Override stripping only on the executable. A Cargo profile override changes symbol names.
    args+=(-C strip=none)
fi
if [[ $plain == false ]]; then
    order="$PWD/tools/code-order/$1.order"
    test -s "$order"
    read -r version < "$order"
    if [[ $version != "# ${compiler%%$'\n'*}" ]]; then
        echo "warning: $1 code order was generated with ${version#\# }. Refresh for ${compiler%%$'\n'*}." >&2
    fi
    # Cargo does not track linker inputs. A fresh path forces this executable to relink.
    link_order=$(mktemp)
    trap 'rm -f -- "$link_order"' EXIT
    cp -- "$order" "$link_order"
    args+=(
        -C link-arg=-Xlinker -C "link-arg=--symbol-ordering-file=$link_order"
        -C link-arg=-Wl,--no-warn-symbol-ordering
    )
fi
cargo rustc --release --locked --target "$target" -p "$package" "$kind" "$1" -- "${args[@]}"
