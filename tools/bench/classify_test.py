"""js2 frames in the subsystem profile: python3 -I tools/bench/classify_test.py."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f'{name}.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


SAMPLES = ''.join(f'bare-drive 1 1:\n\t1 {leaf}+0x10 (bare)\n\t2 bare::browser_fetch::run_streams+0x10 (bare)\n\n' for leaf in (
    'bare_vex::interp::run', 'bare_vex::gc::collect', 'bare_vex::parse::Parser::expr'))


class Js2Frames(unittest.TestCase):
    def setUp(self):
        f = tempfile.NamedTemporaryFile('w', suffix='.txt', delete=False)
        f.write(SAMPLES)
        f.close()
        self.path = f.name
        self.addCleanup(Path(f.name).unlink)

    def test_classify_puts_js2_in_js_rows_not_network(self):
        sub = load('classify').split(self.path)[0]['SUBSYSTEMS']
        self.assertNotIn('Network + TLS + HTTP', sub)
        for row in ('JS engine (run, without parse and collector)', 'Parsing (HTML, JS parse and compile)', 'Collector (any frame on the stack)'):
            self.assertAlmostEqual(sub[row], 100 / 3, places=3)

    def test_jscat_sees_the_js2_collector_and_parser(self):
        out = load('jscat').split(self.path)[0]
        self.assertAlmostEqual(out['Collector (any frame on the stack)'], 100 / 3, places=3)
        self.assertAlmostEqual(out['Parse and compile'], 100 / 3, places=3)


if __name__ == '__main__':
    unittest.main()
