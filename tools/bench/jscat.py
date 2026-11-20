"""Split the JS engine's share of CPU by kind of work (design 3.1). Input: `perf script -F tid,period,ip,sym` output. Use it through subsystem_profile.sh.

  jscat.py SCRIPT.txt [SCRIPT.txt ...]    one column for each file, then the mean and the range of the shares (percent of ALL samples)

A sample is JS when the first frame from the leaf whose symbol starts with `bare` is in `bare_js`, or when it is collector. Collector: any frame of
the sample is `Interp::collect` or `Heap::collect`, so the `free`, `Vec` and shape work under the collector counts there (design 3.1: 9.4% against 4.7%
for the leaf alone). Other kinds: the first of the first 6 frames from the leaf that matches a pattern, in the order of KINDS.
"""
import collections, re, sys

COLLECT = re.compile(r'^<?bare_js::\w+::(Interp|Heap)::collect$')
KINDS = [
    ('Parse and compile', r'bare_js::(lex|parse|ast|parse_cache|parse_ahead)|bc_gen|bc_scope|compile|lazy_function|Lexer|Parser|walk_program'),
    ('Calls (frames, arguments, bind)', r'call_fn|bind_call|call_script|Callee|bc_enter|new_env|env_declare|construct|call_native|select|frame'),
    ('Property get and set (shapes, caches, lookup)', r'ic_|get_prop|set_prop|find|shape|Shape|PropMap|lookup|get<bare_js::heap::Object>|own_|proto|define|get_field|set_field|keyed|slot'),
    ('Variable lookup', r'Binding|binding|resolve|get_name|set_name|scope|Env|hops'),
    ('Strings and atoms', r'Str|str_|intern|atom|rope|flat|utf16|concat'),
    ('Allocation (objects, arrays, malloc, free)', r'alloc|new_object|new_array|push_slot|reserve|malloc|realloc|free|cfree'),
    ('Bytecode loop (dispatch, decode)', r'bc_loop|bc_target|bc_sync|decode'),
    ('Tree walker', r'eval_expr|exec_|link_inner|eval_assign|eval_'),
    ('Built-ins (natives)', r'native|builtin|array_|json|regexp|Promise|promise|Map|Set|number|to_string|to_number|math'),
]
RX = [(n, re.compile(p)) for n, p in KINDS]


def samples(path):
    frames = None
    for line in open(path, errors='replace'):
        if not line.strip():
            if frames is not None:
                yield frames
            frames = None
        elif not line.startswith((' ', '\t')):
            frames = []
        elif frames is not None:
            p = line.split(None, 1)
            frames.append(re.sub(r'\+0x[0-9a-f]+', '', p[1]).split(' (')[0].strip() if len(p) > 1 else '?')
    if frames is not None:
        yield frames


def split(path):
    c, total, js = collections.Counter(), 0, 0
    for fr in samples(path):
        total += 1
        if any(COLLECT.search(f) for f in fr):  # also when a host frame is the leaf, as in classify.py
            js += 1
            c['Collector (any frame on the stack)'] += 1
            continue
        first = next((f for f in fr if re.match(r'^<?bare', f)), None)
        if first is None or 'bare_js' not in first:
            continue
        js += 1
        c[next((n for f in fr[:6] for n, r in RX if r.search(f)), 'Other JS')] += 1
    t = max(total, 1)
    out = {k: 100 * v / t for k, v in c.items()}
    out['JS engine, all kinds'] = 100 * js / t
    return out, total


if __name__ == '__main__':
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    runs = [split(p) for p in sys.argv[1:]]
    print('samples: ' + ', '.join(str(t) for _, t in runs))
    print('\nJS engine by kind of work (percent of all samples)')
    print(f'  {"":48}' + ''.join(f'{"run" + str(i + 1):>8}' for i in range(len(runs))) + f'{"mean":>8}  range')
    keys = set().union(*(r.keys() for r, _ in runs))
    rows = {k: [r.get(k, 0.0) for r, _ in runs] for k in keys}
    for k, v in sorted(rows.items(), key=lambda kv: (kv[0] != 'JS engine, all kinds', -sum(kv[1]))):
        print(f'  {k:48}' + ''.join(f'{x:8.2f}' for x in v) + f'{sum(v) / len(v):8.2f}  {min(v):.2f} to {max(v):.2f}')
