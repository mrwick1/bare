"""Split the samples of `perf script -F tid,period,ip,sym` output by browser subsystem. Use it through subsystem_profile.sh.

  classify.py SCRIPT.txt [SCRIPT.txt ...]    one column for each file, then the mean and the range (min to max) of the shares

A sample counts once. The first matching rule from the leaf gives its owner (RULES). The tables:
  SUBSYSTEMS  the plan's split. Collector: any frame of the sample is `Interp::collect` or `Heap::collect` (design 3.1: the
              work in `free`, `Vec` and shape lookups under the collector is collector work). Parsing: HTML parse, and JS lex,
              parse and compile. JS: the rest of the JS engine. Style includes CSS parse. The rows DOM, images and driver are
              the rest of the browser.
  SELF        the table of 2026-11-18: owner by first own frame from the leaf, JS split by kind. Its "garbage collect" row counts
              only the first collector-like frame, so it is smaller than the Collector row above.
  INCLUSIVE   the sample has a frame of the subsystem (shares add up to more than 100%).
"""
import collections, functools, re, sys

RULES = [  # (subsystem, regex on the symbol): the first match wins
    ('JS engine', r'^<?bare_js2?::'),
    ('DOM + Web APIs (bindings)', r'^<?bare::browser_(?!style|computed|used|fonts|face|fontface|img|fetch|xhr|loader|cookie)|^<?bare::(page|agent|control|force|shims)'),
    ('Style (CSS parse, match, cascade)', r'^<?bare_css::|^<?bare::browser_(style|computed|used|cssom|css_ns)'),
    ('Layout + text', r'^<?bare_layout::|^<?bare_unicode::|^<?bare::browser_(fonts|face|fontface)|rustybuzz|ttf_parser|swash|harfrust|skrifa|read_fonts'),
    ('Paint + raster', r'^<?bare_paint::|^<?bare_window::|tiny_skia|fontdue|zeno'),
    ('Images', r'^<?bare_image::|^<?bare::(images|browser_img)|png|zune|jpeg|gif'),
    ('HTML parse', r'^<?bare_html::'),
    ('Network + TLS + HTTP', r'^<?bare_(net|tls|url|os)::|^<?bare::(resources|browser_fetch|browser_xhr|browser_loader|browser_cookie)|rustls|ring::|aws_lc|brotli|flate|miniz|zstd|^<?http|h2::'),
    ('Driver (bare-drive)', r'^<?bare_drive::|^<?bare_observe::'),
]
RX = [(n, re.compile(p)) for n, p in RULES]
COLLECT = re.compile(r'^<?bare_js::\w+::(Interp|Heap)::collect$|^<?bare_js2::gc::collect\b')
JS_PARSE = re.compile(r'bare_js::(parse|lex|ast|parse_cache|parse_ahead|bc_gen)|bare_js2::(parse|lex|ast|parse_ahead|compile)')
JS_GC = re.compile(r'(collect|gc_|mark|sweep|trace_)')
SUBSYSTEM_OF = {  # owner to row of the SUBSYSTEMS table
    'Network + TLS + HTTP': 'Network + TLS + HTTP', 'HTML parse': 'Parsing (HTML, JS parse and compile)',
    'Style (CSS parse, match, cascade)': 'Style', 'Layout + text': 'Layout + text', 'Paint + raster': 'Paint + raster',
    'DOM + Web APIs (bindings)': 'DOM + Web APIs', 'Images': 'Images', 'Driver (bare-drive)': 'Driver',
}


@functools.lru_cache(maxsize=None)  # a symbol repeats in many samples
def facts(sym):
    """Gives (owner, is the collector, is parse or compile, is collector-like) of a symbol."""
    own = next((n for n, r in RX if r.search(sym)), None)
    return own, bool(COLLECT.search(sym)), bool(JS_PARSE.search(sym)), bool(JS_GC.search(sym) and 'bare_js' in sym)


def samples(path):
    """Yield the frames of each sample, leaf first, with offsets and `(dso)` removed. Kernel frames end with ' [k]'."""
    frames = None
    for line in open(path, errors='replace'):
        if not line.strip():
            if frames is not None:
                yield frames
            frames = None
        elif not line.startswith((' ', '\t')):
            frames = []
        elif frames is not None:
            p = line.strip().split(None, 1)
            s = re.sub(r'\+0x[0-9a-f]+', '', p[1] if len(p) > 1 else '?')
            frames.append(s.replace(' ([kernel.kallsyms])', '') + ' [k]' if '([kernel.kallsyms])' in s else re.sub(r' \(.*\)$', '', s))
    if frames is not None:
        yield frames


def split(path):
    """Gives {table: {row: percent}} for one file."""
    sub, selfc, incl, jsub = (collections.Counter() for _ in range(4))
    total = 0
    for syms in samples(path):
        total += 1
        fs = [facts(s) for s in syms]
        classes = [f[0] for f in fs]
        own = next((c for c in classes if c), None)
        collector = any(f[1] for f in fs)
        js_parse = own == 'JS engine' and any(f[2] for f in fs)
        if own == 'JS engine':  # the 2026-11-18 JS split: the first frame that is parse or collector-like decides
            kind = ' / execute (interpreter + builtins)'
            for f in fs:
                if f[2]:
                    kind = ' / parse + compile'
                    break
                if f[3]:
                    kind = ' / garbage collect'
                    break
            jsub['JS' + kind] += 1
        sub['Collector (any frame on the stack)' if collector else 'Parsing (HTML, JS parse and compile)' if js_parse
            else 'JS engine (run, without parse and collector)' if own == 'JS engine' else SUBSYSTEM_OF.get(own or '', 'Other')] += 1
        leaf_kernel = bool(syms) and syms[0].endswith('[k]')
        selfc[(own or 'other') + (' [kernel]' if leaf_kernel else '')] += 1
        for c in set(filter(None, classes)):
            incl[c] += 1
    pct = lambda c: {k: 100 * v / max(total, 1) for k, v in c.items()}
    return {'SUBSYSTEMS': pct(sub), 'SELF': pct(selfc), 'JS split (2026-11-18 rules)': pct(jsub), 'INCLUSIVE': pct(incl)}, total


def main(paths):
    runs = [split(p) for p in paths]
    print('samples: ' + ', '.join(str(t) for _, t in runs))
    if min(t for _, t in runs) < 5000:
        print('warning: under 5,000 samples in a run; the shares are rough (more runs, or a longer journey)')
    for table in runs[0][0]:
        rows = collections.defaultdict(list)
        for r, _ in runs:
            for k in set().union(*(x[0][table].keys() for x in runs)):
                rows[k].append(r[table].get(k, 0.0))
        print(f'\n{table} (percent of samples)')
        print(f'  {"":48}' + ''.join(f'{"run" + str(i + 1):>8}' for i in range(len(runs))) + f'{"mean":>8}  range')
        for k, v in sorted(rows.items(), key=lambda kv: -sum(kv[1])):
            print(f'  {k:48}' + ''.join(f'{x:8.2f}' for x in v) + f'{sum(v) / len(v):8.2f}  {min(v):.2f} to {max(v):.2f}')


if __name__ == '__main__':
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
