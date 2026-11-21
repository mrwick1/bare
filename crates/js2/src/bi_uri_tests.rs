// `encodeURI`, `encodeURIComponent`, `decodeURI` and `decodeURIComponent` (`bi_uri.rs`), run normally and under constant collection.
// `encodeURIComponent` is what the search of the Rust book calls on js2 (`searcher.js`: `encodeURIComponent(terms.join(' '))`).
use super::tests::check;

#[test]
fn encode_and_decode() {
    check(&[
        ("var r = encodeURIComponent('own ership'.split(' ').join(' ')) + ',' + encodeURIComponent(\"a'b/c?d=e&f\");", "r", "own%20ership,a'b%2Fc%3Fd%3De%26f"),
        ("var r = encodeURI('http://a/b c?d=é#x') + ',' + encodeURIComponent('é\\u{1F600}');", "r", "http://a/b%20c?d=%C3%A9#x,%C3%A9%F0%9F%98%80"),
        ("var r = decodeURIComponent('a%20b%2Fc%C3%A9') + ',' + decodeURI('a%20b%2Fc%23');", "r", "a b/cé,a b%2Fc%23"),
        ("var r = [typeof encodeURI, typeof decodeURI, encodeURIComponent.length, encodeURIComponent.name, encodeURIComponent(undefined)].join();", "r", "function,function,1,encodeURIComponent,undefined"),
        ("var r = [];  ['\\ud800', 'x'].forEach(function (s) { try { encodeURIComponent(s); r.push('ok'); } catch (e) { r.push(e.name); } }); r = r.join();", "r", "URIError,ok"),
        ("var r = []; ['%', '%E0%A4%A', '%C0%80', '%ZZ', '%80'].forEach(function (s) { try { decodeURIComponent(s); r.push('ok'); } catch (e) { r.push(e instanceof URIError); } }); r = r.join();", "r", "true,true,true,true,true"),
    ]);
}
