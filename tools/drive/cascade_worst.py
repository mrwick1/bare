#!/usr/bin/env python3
"""Hostile page for the style cascade: N elements, R universal `*:not(.zK)` rules that match every element, and a script that forces
repeated cascades inside one task. MODE `class` flips a class on <body> (a restyle of the whole tree); MODE `sheet` appends a rule
(a full compute). Each read sets data-n on <html> after it finishes, so the count of cascades survives a Steps abort.
  tools/drive/cascade_worst.py OUTDIR [ELEMENTS=60000] [RULES=1500] [MODE=class|sheet] [LOOPS=100]
Then: (cd OUTDIR && python3 -m http.server 8765 &); bare-drive --var URL=http://127.0.0.1:8765/page.html OUTDIR/page.steps"""
import os, sys
out = sys.argv[1]
n, r, mode, loops = (int(sys.argv[2]) if len(sys.argv) > 2 else 60000, int(sys.argv[3]) if len(sys.argv) > 3 else 1500,
                     sys.argv[4] if len(sys.argv) > 4 else "class", int(sys.argv[5]) if len(sys.argv) > 5 else 100)
os.makedirs(out, exist_ok=True)
rules = "".join(f"*:not(.z{k}){{color:#{k % 4096:03x}}}" for k in range(r))
body = "".join("<div><i></i></div>" for _ in range(n // 2 - 1))
flip = "document.body.className = i & 1 ? 'z0' : ''" if mode == "class" else "document.head.appendChild(document.createElement('style')).textContent = '.q' + i + '{margin:0}'"
html = f"""<!doctype html><meta charset=utf-8><style>{rules}</style><body>{body}<script>
var x = document.body.firstChild;
for (var i = 0; i < {loops}; i++) {{ {flip}; getComputedStyle(x).color; document.documentElement.setAttribute('data-n', i + 1); }}
document.documentElement.setAttribute('data-done', '1');
</script><script>console.log('cascades ' + document.documentElement.getAttribute('data-n'));
document.documentElement.setAttribute('data-done', '1');</script>"""
open(os.path.join(out, "page.html"), "w").write(html)
open(os.path.join(out, "page.steps"), "w").write(f"timeout 120000\nopen ${{URL}}\nexpect js \"document.documentElement.getAttribute('data-n') !== null\"\n")
