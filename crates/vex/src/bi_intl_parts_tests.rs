use crate::script::run;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(vm.global_text("r"), expected);
}

const PARTS: &str = "function parts(list) { return list.map(function (p) { return p.type + '=' + p.value; }).join(','); }";

#[test]
fn date_time_format_gives_its_parts() {
    check(
        &format!(r#"{PARTS}
        var f = new Intl.DateTimeFormat('en-GB', {{timeZone: 'UTC', timeZoneName: 'short', hour: '2-digit', minute: '2-digit'}});
        var p = Intl.DateTimeFormat.prototype.formatToParts;
        var bad; try {{ p.call({{}}, 0); }} catch (e) {{ bad = e instanceof TypeError; }}
        var r = [parts(f.formatToParts(0)), p.length, p.name, bad].join('|');
        "#),
        "hour=00,literal=:,minute=00,literal= ,timeZoneName=UTC|1|formatToParts|true",
    );
}

#[test]
fn number_format_gives_its_parts() {
    check(
        &format!(r#"{PARTS}
        var f = new Intl.NumberFormat('en', {{style: 'currency', currency: 'USD'}});
        var r = parts(f.formatToParts(-1234.5));
        "#),
        "minusSign=-,currency=$,integer=1,group=,,integer=234,decimal=.,fraction=50",
    );
}
