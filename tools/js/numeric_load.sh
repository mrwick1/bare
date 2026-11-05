#!/bin/sh
# Authored Node checksums, then 16 cases at 1x/10x/100x in capped fresh processes.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo build --release -q -p bare-js --example numeric_load
node - <<'JS' > "$work/cases"
for (const n of [1000, 10000, 100000]) {
  for (const kind of ['empty','random','clz32','log','radix16','radix36frac','radix2sub','pow','sin','atan2','tofixed','toprecision','toexp','parsefloat','encodeuri','escape']) {
    let sum=0;
    for(let i=0;i<n;i++) {
      switch(kind) {
        case 'clz32': sum+=Math.clz32(i); break;
        case 'log': sum+=Math.log(i+1); break;
        case 'radix16': sum+=(i*2654435761).toString(16).length; break;
        case 'radix36frac': sum+=(i/7+0.123).toString(36).length; break;
        case 'radix2sub': sum+=(5e-324*(i+1)).toString(2).length; break;
        case 'pow': sum+=(i%100)**1.5; break;
        case 'sin': sum+=Math.sin(i)+1; break;
        case 'atan2': sum+=Math.atan2(i,7)+2; break;
        case 'tofixed': sum+=(i/7).toFixed(2).length; break;
        case 'toprecision': sum+=(i*1.1).toPrecision(6).length; break;
        case 'toexp': sum+=(i/3).toExponential().length; break;
        case 'parsefloat': sum+=parseFloat(i/7+'x'); break;
        case 'encodeuri': sum+=encodeURI('\u00e9 /?#'+i).length; break;
        case 'escape': sum+=escape('\u00e9 /?#'+i).length; break;
      }
    }
    console.log(kind,n,sum);
  }
}
JS
while read -r kind n expected; do
  (ulimit -v 1048576; timeout 45 target/release/examples/numeric_load "$kind" "$n" "$expected") >> "$work/out"
done < "$work/cases"
test "$(wc -l < "$work/out")" -eq 48
cat "$work/out"
