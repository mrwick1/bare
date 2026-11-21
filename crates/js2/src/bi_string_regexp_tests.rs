// The `RegExp` forms of `match`, `matchAll`, `search`, `replace`, `replaceAll` and `split` (`bi_string_regexp.rs`), each run normally and
// under constant collection. The patterns are those of the three journeys that failed on js2 (`rust-book-search`: the elasticlunr
// tokenizer and Porter stemmer; `python-docs-search-results` and `python-org-docs-link`: searchtools.js).
use super::tests::check;

#[test]
fn match_with_and_without_g() {
    check(&[
        ("var m = 'running'.match(/^(.+?)(ing)$/); var r = m.length + '|' + m[0] + '|' + m[1] + '|' + m[2] + '|' + m.index;", "r", "3|running|runn|ing|0"),
        ("var r = 'abc'.match(/x/);", "r", "null"),
        ("var r = 'a1b22c333'.match(/\\d+/g).join(',');", "r", "1,22,333"),
        ("var r = 'abc'.match(/x/g);", "r", "null"),
        ("var g = /a/g; g.lastIndex = 2; var r = 'aaa'.match(g).length + ',' + g.lastIndex;", "r", "3,0"),
        // An empty match moves on by one unit (and by a code point with `u`).
        ("var r = 'ab'.match(/x*/g).length;", "r", "3"),
        ("var r = '\\u{1F600}'.match(/(?:)/gu).length + ',' + '\\u{1F600}'.match(/(?:)/g).length;", "r", "2,3"),
        // A string argument is the source of a new regexp.
        ("var r = 'a.b'.match('.')[0] + ',' + 'a.b'.match('\\\\.')[0] + ',' + 'ab'.match()[0].length;", "r", "a,.,0"),
    ]);
}

#[test]
fn match_all_and_search() {
    check(&[
        ("var r = [...'a1b22'.matchAll(/\\d+/g)].map(function (m) { return m[0] + '@' + m.index; }).join(',');", "r", "1@1,22@3"),
        ("var r = [...'ab'.matchAll('b')].length;", "r", "1"),
        ("var g = /(a)/g; g.lastIndex = 1; var r = [...'aaa'.matchAll(g)].length + ',' + g.lastIndex;", "r", "2,1"),
        ("try { 'a'.matchAll(/a/); var r = 'no'; } catch (e) { r = e.name; }", "r", "TypeError"),
        ("var r = 'xxabc'.search(/b/) + ',' + 'abc'.search(/z/) + ',' + 'abc'.search('c');", "r", "3,-1,2"),
        ("var g = /b/g; g.lastIndex = 2; var r = 'abc'.search(g) + ',' + g.lastIndex;", "r", "1,2"),
    ]);
}

#[test]
fn replace_with_a_regexp() {
    check(&[
        // The Porter stemmer of elasticlunr.
        ("var r = 'caresses'.replace(/^(.+?)(ss|i)es$/, '$1$2') + ',' + 'cats'.replace(/^(.+?)([^s])s$/, '$1$2');", "r", "caress,cat"),
        ("var r = 'running'.replace(/(ing)$/, '') + ',' + 'hoped'.replace(/^(.+?)(ed|ing)$/, '$1');", "r", "runn,hop"),
        ("var r = 'a-b-c'.replace(/-/g, '+') + ',' + 'a-b-c'.replace(/-/, '+');", "r", "a+b+c,a+b-c"),
        ("var r = 'abc'.replace(/(b)(x)?/, '[$1|$2|$0|$3|$&|$`|$\\'|$$]');", "r", "a[b||$0|$3|b|a|c|$]c"),
        ("var r = 'abcdefghijkl'.replace(/(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)/, '$11-$1-$10-$12');", "r", "k-a-j-a2l"),
        ("var r = 'ab'.replace(/x*/g, '-');", "r", "-a-b-"),
        ("var r = ' a  b '.replace(/^\\s+|\\s+$/g, '');", "r", "a  b"),
        ("var r = 'a<b>c'.replace(/<[^>]*>/g, '');", "r", "ac"),
        ("var r = 'x'.replace(/y/, 'z') + ',' + 'aaa'.replaceAll(/a/g, 'b');", "r", "x,bbb"),
        ("try { 'a'.replaceAll(/a/, 'b'); var r = 'no'; } catch (e) { r = e.name; }", "r", "TypeError"),
        ("var g = /a/g; g.lastIndex = 5; 'aa'.replace(g, 'b'); var r = g.lastIndex;", "r", "0"),
    ]);
}

#[test]
fn replace_with_a_function() {
    check(&[
        ("var r = 'a1b2'.replace(/\\d/g, function (m, i, s) { return '<' + m + i + s.length + '>'; });", "r", "a<114>b<234>"),
        ("var r = 'ab'.replace(/(a)(x)?/, function (m, p1, p2, i) { return [m, p1, p2, i].join('|'); });", "r", "a|a||0b"),
        ("var r = 'a-b'.replace('-', function (m, i, s) { return '[' + m + i + s + ']'; });", "r", "a[-1a-b]b"),
        ("var r = 'aXbX'.replaceAll('X', function () { return 1; });", "r", "a1b1"),
        ("var c = 0, r = 'aaa'.replace(/a/g, function () { return ++c; });", "r", "123"),
        ("try { 'a'.replace(/a/, function () { throw new RangeError('x'); }); var r = 'no'; } catch (e) { r = e.name; }", "r", "RangeError"),
    ]);
}

#[test]
fn replace_with_a_string_pattern() {
    check(&[
        ("var r = 'a.b.c'.replace('.', '-') + ',' + 'a.b.c'.replaceAll('.', '-') + ',' + 'abc'.replaceAll('', '-');", "r", "a-b.c,a-b-c,-a-b-c-"),
        ("var r = 'abc'.replace('b', '$&$&$`$\\'$$');", "r", "abbac$c"),
    ]);
}

#[test]
fn split_with_a_regexp() {
    check(&[
        // The tokenizer of elasticlunr.
        ("var r = 'rust-book  search\\ttext'.split(/[\\s\\-]+/).join('|');", "r", "rust|book|search|text"),
        ("var r = 'a1b2c'.split(/\\d/).join('|') + ',' + 'a1b2c'.split(/(\\d)/).join('|');", "r", "a|b|c,a|1|b|2|c"),
        ("var r = 'abc'.split(/(?:)/).join('|') + ',' + 'abc'.split(/x*/).join('|') + ',' + 'abc'.split(/b/y).join('|');", "r", "a|b|c,a|b|c,a|c"),
        ("var r = 'a,b,c'.split(/,/, 2).join('|') + ',' + 'a,b,c'.split(/,/, 0).length;", "r", "a|b,0"),
        ("var r = ''.split(/x/).length + ',' + ''.split(/(?:)/).length;", "r", "1,0"),
        ("var r = 'a,b'.split(/(,)|(;)/).length;", "r", "4"),
        ("var r = ',a,'.split(/,/).join('|');", "r", "|a|"),
        // searchtools.js of the Python docs: a `u` class with property escapes.
        ("var r = 'asyncio, tasks.run'.split(/[^\\p{Letter}\\p{Number}_\\p{Emoji_Presentation}]+/u).join('|');", "r", "asyncio|tasks|run"),
    ]);
}
