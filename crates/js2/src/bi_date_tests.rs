// Tests of `Date` (`bi_date.rs`): each case is a program, the global to read and the expected text. Each runs normally and under constant
// collection (the helpers of `script_tests.rs`). The expected texts are what Node 22 prints (the zone is set with `set_local_zone`).
use crate::bi_date::set_local_zone;
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

fn check_in(zone: &str, cases: &[(&str, &str, &str)]) {
    assert!(set_local_zone(zone), "{zone}");
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
        if !cfg!(miri) {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn the_constructor_forms() {
    check_in("UTC", &[
        ("var r = new Date(0).toISOString() + ' ' + new Date(1e12).getTime() + ' ' + new Date(2020, 0, 1).getTime() + ' ' + new Date(99, 0).getFullYear();", "r", "1970-01-01T00:00:00.000Z 1000000000000 1577836800000 1999"),
        ("var r = new Date('2024-02-29T12:34:56.789Z').getTime() + ' ' + new Date('2024-02-29').getTime() + ' ' + new Date(new Date(5)).getTime() + ' ' + new Date(undefined).getTime() + ' ' + new Date(null).getTime() + ' ' + new Date(true).getTime();", "r", "1709210096789 1709164800000 5 NaN 0 1"),
        ("var r = new Date(8.64e15).getTime() + ' ' + new Date(8.64e15 + 1).getTime() + ' ' + new Date(-0).getTime() + ' ' + Object.is(new Date(-0).getTime(), 0) + ' ' + new Date(1.9).getTime() + ' ' + new Date(-1.9).getTime();", "r", "8640000000000000 NaN 0 true 1 -1"),
        ("var r = new Date({ valueOf() { return 7; } }).getTime() + ' ' + new Date({ toString() { return '1970-01-01T00:00:00.009Z'; }, valueOf() { return {}; } }).getTime() + ' ' + new Date({ [Symbol.toPrimitive]() { return 11; } }).getTime();", "r", "7 9 11"),
        ("var r = typeof Date() + ' ' + (Date().length > 20) + ' ' + (typeof Date.now()) + ' ' + (Date.now() > 1.7e12) + ' ' + (new Date() - Date.now() < 1000);", "r", "string true number true true"),
        ("var r = new Date(2020, 0, 1, 0, 0, 0, 0).getTime() + ' ' + new Date(2020, 12, 1).getTime() + ' ' + new Date(2020, 0, 0).getDate() + ' ' + new Date(NaN, 0).getTime() + ' ' + new Date(2020, 1.9).getMonth();", "r", "1577836800000 1609459200000 31 NaN 1"),
    ]);
}

#[test]
fn date_utc_and_parse() {
    check_in("UTC", &[
        ("var r = Date.UTC(2020, 0, 1) + ' ' + Date.UTC(99, 0) + ' ' + Date.UTC(2020) + ' ' + Date.UTC() + ' ' + Date.UTC(2020, 0, 1, 0, 0, 0, 1.9) + ' ' + Date.UTC(275760, 8, 13) + ' ' + Date.UTC(275760, 8, 14);", "r", "1577836800000 915148800000 1577836800000 NaN 1577836800001 8640000000000000 NaN"),
        ("var r = Date.parse('2020-01-01T00:00:00Z') + ' ' + Date.parse('2020-01-01T05:30:00+05:30') + ' ' + Date.parse('2020-01') + ' ' + Date.parse('+275760-09-13T00:00:00.000Z') + ' ' + Date.parse('-000000-01-01T00:00:00Z') + ' ' + Date.parse('x') + ' ' + Date.parse('2021-02-30') + ' ' + Date.parse();", "r", "1577836800000 1577836800000 1577836800000 8640000000000000 NaN NaN NaN NaN"),
        ("var r = Date.parse('Wed, 01 Jan 2020 00:00:00 GMT') + ' ' + Date.parse('Jan 1 2020 10:00 PM UTC') + ' ' + Date.parse('1/2/2020 UTC') + ' ' + Date.parse('2020-01-01T24:00:00Z') + ' ' + Date.parse('1 January 2020 00:00:00 GMT+0100') + ' ' + Date.parse('2020-01-01T00:00:00.123456Z');", "r", "1577836800000 1577916000000 1577923200000 1577923200000 1577833200000 1577836800123"),
        ("var d = new Date(1577836800000); var r = Date.parse(d.toString()) + ' ' + Date.parse(d.toUTCString()) + ' ' + Date.parse(d.toISOString()) + ' ' + (Date.parse({ toString() { return '2020'; } }));", "r", "1577836800000 1577836800000 1577836800000 1577836800000"),
    ]);
}

#[test]
fn the_getters_and_setters_in_utc() {
    check_in("UTC", &[
        ("var d = new Date(Date.UTC(2024, 1, 29, 12, 34, 56, 789)); var r = [d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCDay(), d.getUTCHours(), d.getUTCMinutes(), d.getUTCSeconds(), d.getUTCMilliseconds(), d.getFullYear(), d.getYear(), d.getTimezoneOffset()].join();", "r", "2024,1,29,4,12,34,56,789,2024,124,0"),
        ("var d = new Date(0); var r = [d.setUTCFullYear(2000, 1, 29), d.setUTCMonth(11), d.setUTCDate(31), d.setUTCHours(23, 59, 59, 999), d.setUTCMinutes(0), d.setUTCSeconds(1, 2), d.setUTCMilliseconds(3), d.toISOString()].join();", "r", "951782400000,978048000000,978220800000,978307199999,978303659999,978303601002,978303601003,2000-12-31T23:00:01.003Z"),
        ("var d = new Date(NaN); var r = [d.setUTCHours(1), d.setUTCMonth(1), d.setUTCDate(1), d.getTime()].join() + ' ' + d.setUTCFullYear(2000) + ' ' + d.toISOString();", "r", "NaN,NaN,NaN,NaN 946684800000 2000-01-01T00:00:00.000Z"),
        ("var d = new Date(0); var r = d.setTime(5) + ' ' + d.getTime() + ' ' + d.setTime(8.64e15 + 1) + ' ' + d.getTime() + ' ' + d.setTime() + ' ' + d.setUTCDate(NaN) + ' ' + d.getTime();", "r", "5 5 NaN NaN NaN NaN NaN"),
        ("var log = []; var d = new Date(0); var r = d.setUTCHours({ valueOf() { log.push('h'); d.setTime(1e12); return 1; } }, { valueOf() { log.push('m'); return 2; } }) + ' ' + log.join('') + ' ' + d.getTime();", "r", "3720000 hm 3720000"),
        ("var d = new Date(NaN); var r = d.setUTCHours({ valueOf() { d.setTime(0); return 1; } }) + ' ' + d.getTime();", "r", "NaN 0"),
        ("var d = new Date(NaN); var log = []; d.setUTCHours({ valueOf() { log.push(1); return 1; } }, { valueOf() { log.push(2); return 1; } }, { valueOf() { log.push(3); return 1; } }); var r = log.join();", "r", "1,2,3"),
        ("var d = new Date(0); var r = d.setUTCFullYear(2020, 0) + ' ' + d.getUTCDate() + ' ' + d.setUTCMonth(1, 30) + ' ' + d.getUTCMonth() + d.getUTCDate() + ' ' + d.setYear(99) + ' ' + d.setYear(NaN) + ' ' + d.setYear(2000);", "r", "1577836800000 1 1583020800000 21 920246400000 NaN 946684800000"),
    ]);
}

#[test]
fn local_time_in_new_york() {
    check_in("America/New_York", &[
        ("var d = new Date(2020, 5, 15, 12); var r = d.getTime() + ' ' + d.getHours() + ' ' + d.getUTCHours() + ' ' + d.getTimezoneOffset() + ' ' + d.getDay() + ' ' + d.getMonth() + ' ' + d.getDate();", "r", "1592236800000 12 16 240 1 5 15"),
        ("var d = new Date(2020, 0, 15, 12); var r = d.getTime() + ' ' + d.getTimezoneOffset() + ' ' + d.toString();", "r", "1579107600000 300 Wed Jan 15 2020 12:00:00 GMT-0500 (Eastern Standard Time)"),
        ("var d = new Date(2020, 5, 15, 12); var r = d.toString() + '|' + d.toDateString() + '|' + d.toTimeString() + '|' + d.toUTCString() + '|' + d.toISOString();", "r", "Mon Jun 15 2020 12:00:00 GMT-0400 (Eastern Daylight Time)|Mon Jun 15 2020|12:00:00 GMT-0400 (Eastern Daylight Time)|Mon, 15 Jun 2020 16:00:00 GMT|2020-06-15T16:00:00.000Z"),
        // a gap (02:30 on 2020-03-08 does not exist) uses the offset from before it, an overlap (01:30 on 2020-11-01 happens twice) is the first instant
        ("var r = new Date(2020, 2, 8, 2, 30).toString() + '|' + new Date(2020, 10, 1, 1, 30).getTime() + '|' + new Date(2020, 10, 1, 2, 30).getTime();", "r", "Sun Mar 08 2020 03:30:00 GMT-0400 (Eastern Daylight Time)|1604208600000|1604215800000"),
        ("var d = new Date(2020, 5, 15, 12); var r = [d.setHours(0), d.setDate(1), d.setMonth(11, 25), d.setFullYear(2021, 0, 31), d.setMinutes(30, 15), d.setSeconds(5), d.setMilliseconds(9), d.toString()].join('|');", "r", "1592193600000|1590984000000|1608872400000|1612069200000|1612071015000|1612071005000|1612071005009|Sun Jan 31 2021 00:30:05 GMT-0500 (Eastern Standard Time)"),
        ("var d = new Date('2020-06-15T12:00:00'); var r = d.getTime() + ' ' + new Date('2020-06-15').getTime() + ' ' + Date.parse('Jun 15 2020 12:00') + ' ' + Date.parse('Jun 15 2020 12:00 EST');", "r", "1592236800000 1592179200000 1592236800000 1592240400000"),
        ("var d = new Date(1850, 0, 1); var r = d.getTimezoneOffset() + ' ' + d.getFullYear() + ' ' + new Date(2060, 6, 1).getTimezoneOffset();", "r", "296 1850 240"),
        ("var r = new Date(2020, 5, 15, 12).toLocaleString() + '|' + new Date(2020, 5, 15, 0, 5, 9).toLocaleString('en-US') + '|' + new Date(2020, 5, 15, 12).toLocaleDateString() + '|' + new Date(2020, 5, 15, 13, 7, 8).toLocaleTimeString();", "r", "6/15/2020, 12:00:00 PM|6/15/2020, 12:05:09 AM|6/15/2020|1:07:08 PM"),
    ]);
}

#[test]
fn local_time_in_india_and_utc() {
    check_in("Asia/Kolkata", &[
        ("var d = new Date(2020, 0, 1); var r = d.getTime() + ' ' + d.getTimezoneOffset() + ' ' + d.toString() + ' ' + d.getHours();", "r", "1577817000000 -330 Wed Jan 01 2020 00:00:00 GMT+0530 (India Standard Time) 0"),
        ("var d = new Date(1943, 0, 1); var r = d.getTimezoneOffset();", "r", "-390"),
    ]);
    check_in("UTC", &[("var d = new Date(2020, 0, 1); var r = d.toString() + ' ' + d.getTimezoneOffset();", "r", "Wed Jan 01 2020 00:00:00 GMT+0000 (Coordinated Universal Time) 0")]);
    check_in("+05:30", &[("var d = new Date(2020, 0, 1); var r = d.getTime() + ' ' + d.toString();", "r", "1577817000000 Wed Jan 01 2020 00:00:00 GMT+0530 (GMT+05:30)")]);
    check_in("Asia/Kathmandu", &[("var d = new Date(2020, 0, 1); var r = d.toString();", "r", "Wed Jan 01 2020 00:00:00 GMT+0545 (GMT+05:45)")]);
}

#[test]
fn text_forms() {
    check_in("UTC", &[
        ("var r = new Date(NaN).toString() + '|' + new Date(NaN).toDateString() + '|' + new Date(NaN).toTimeString() + '|' + new Date(NaN).toUTCString() + '|' + new Date(NaN).toLocaleString() + '|' + String(new Date(NaN)) + '|' + new Date(NaN).toJSON();", "r", "Invalid Date|Invalid Date|Invalid Date|Invalid Date|Invalid Date|Invalid Date|null"),
        ("var r = ''; try { new Date(NaN).toISOString(); } catch (e) { r = e.name + ': ' + e.message; }", "r", "RangeError: Invalid time value"),
        ("var r = new Date(8.64e15).toISOString() + ' ' + new Date(-62198755200000).toISOString() + ' ' + new Date(-1).toUTCString() + ' ' + new Date(-62198755200000).toUTCString() + ' ' + new Date(-62198755200000).toString();", "r", "+275760-09-13T00:00:00.000Z -000001-01-01T00:00:00.000Z Wed, 31 Dec 1969 23:59:59 GMT Fri, 01 Jan -0001 00:00:00 GMT Fri Jan 01 -0001 00:00:00 GMT+0000 (Coordinated Universal Time)"),
        ("var d = new Date(0); var r = (d.toGMTString === d.toUTCString) + ' ' + d.toUTCString();", "r", "true Thu, 01 Jan 1970 00:00:00 GMT"),
        ("var r = JSON.stringify({ d: new Date(0), n: new Date(NaN) }) + ' ' + Date.prototype.toJSON.call({ toISOString() { return 'x'; } }) + ' ' + Date.prototype.toJSON.call({ valueOf() { return Infinity; }, toISOString() { return 'x'; } });", "r", "{\"d\":\"1970-01-01T00:00:00.000Z\",\"n\":null} x null"),
        ("var r = ''; try { Date.prototype.toJSON.call({}); } catch (e) { r = e.name; } try { Date.prototype.toJSON.call(null); } catch (e) { r += e.name; }", "r", "TypeErrorTypeError"),
    ]);
}

#[test]
fn conversion_to_a_primitive() {
    check_in("UTC", &[
        ("var d = new Date(5); var r = (d - 1) + ' ' + (d * 2) + ' ' + (d > new Date(4)) + ' ' + (+d) + ' ' + (d + 1) + ' ' + `${d}` + ' ' + (d == d.toString()) + ' ' + (d == 5);", "r", "4 10 true 5 Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)1 Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time) true false"),
        ("var d = new Date(5); var f = Date.prototype[Symbol.toPrimitive]; var r = f.call(d, 'number') + ' ' + f.call(d, 'string').slice(0, 3) + ' ' + f.call(d, 'default').slice(0, 3) + ' ' + f.name + ' ' + f.length;", "r", "5 Thu Thu [Symbol.toPrimitive] 1"),
        ("var r = ''; var f = Date.prototype[Symbol.toPrimitive]; try { f.call(new Date(), 'x'); } catch (e) { r += e.name; } try { f.call(new Date()); } catch (e) { r += e.name; } try { f.call(1, 'number'); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeError"),
        ("var f = Date.prototype[Symbol.toPrimitive]; var o = { valueOf() { return 1; }, toString() { return 's'; } }; var r = f.call(o, 'number') + f.call(o, 'string') + f.call(o, 'default');", "r", "1ss"),
        ("var d = Object.getOwnPropertyDescriptor(Date.prototype, Symbol.toPrimitive); var r = d.writable + ' ' + d.enumerable + ' ' + d.configurable;", "r", "false false true"),
        ("var r = Object.prototype.toString.call(new Date(0)) + ' ' + Object.prototype.toString.call(Date.prototype) + ' ' + Object.prototype.toString.call(Date());", "r", "[object Date] [object Object] [object String]"),
    ]);
}

#[test]
fn the_receiver_is_checked() {
    check_in("UTC", &[
        ("var r = ''; for (var k of ['getTime', 'valueOf', 'getFullYear', 'getUTCDay', 'setTime', 'setHours', 'toISOString', 'toString', 'toUTCString', 'toLocaleString', 'getTimezoneOffset', 'getYear', 'setYear']) { try { Date.prototype[k].call({}); r += 'x'; } catch (e) { r += e.name[0]; } }", "r", "TTTTTTTTTTTTT"),
        ("var r = ''; try { Date.prototype.getTime(); } catch (e) { r += e.name; } try { Date.prototype.getTime.call(Date.prototype); } catch (e) { r += e.name; } try { Date.prototype.getTime.call(5); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeError"),
        ("var r = ''; try { Date.prototype.getTime.call(new Date(7)); r = Date.prototype.getTime.call(new Date(7)); } catch (e) { r = e.name; }", "r", "7"),
    ]);
}

#[test]
fn the_properties_of_the_built_ins() {
    check_in("UTC", &[
        ("var r = [Date.length, Date.name, Date.now.length, Date.parse.length, Date.UTC.length, Date.prototype.setHours.length, Date.prototype.setUTCFullYear.length, Date.prototype.toJSON.length, Date.prototype.getTime.name, Date.prototype.constructor === Date, Object.getPrototypeOf(new Date(0)) === Date.prototype].join();", "r", "7,Date,0,1,7,4,3,1,getTime,true,true"),
        ("var d = Object.getOwnPropertyDescriptor(Date, 'prototype'); var e = Object.getOwnPropertyDescriptor(Date.prototype, 'getTime'); var r = d.writable + ' ' + d.enumerable + ' ' + d.configurable + ' ' + e.writable + ' ' + e.enumerable + ' ' + e.configurable;", "r", "false false false true false true"),
        ("var d = new Date(0); var r = Object.keys(d).length + ' ' + Object.getOwnPropertyNames(d).length + ' ' + JSON.stringify(Object.getOwnPropertyDescriptors(d)) + ' ' + (d instanceof Date) + ' ' + typeof d;", "r", "0 0 {} true object"),
        ("var r = typeof new Date(0).setUTCMilliseconds + typeof new Date(0).getYear + typeof Date.prototype.toGMTString + Object.getOwnPropertyNames(Date.prototype).length;", "r", "functionfunctionfunction47"),
        ("var r = ''; try { new Date.now(); } catch (e) { r += e.name; } try { new Date.prototype.getTime(); } catch (e) { r += e.name; } try { Date.prototype.setHours.call(new Date(0), Symbol()); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeError"),
    ]);
}

#[test]
fn a_class_can_extend_date() {
    check_in("UTC", &[
        ("class D extends Date { x() { return this.getUTCFullYear(); } } var d = new D(2020, 5); var r = d.x() + ' ' + (d instanceof D) + ' ' + (d instanceof Date) + ' ' + Object.prototype.toString.call(d) + ' ' + d.toISOString();", "r", "2020 true true [object Date] 2020-06-01T00:00:00.000Z"),
        // a constructor whose `prototype` is not an object: the `Date.prototype` of the constructor (`Reflect.construct` tests this once it is on main)
        ("class A extends Date {} A.prototype = null; var r = 0;", "r", "0"),
    ]);
}

#[test]
fn dates_survive_collection_and_keep_their_time() {
    check_in("UTC", &[("var a = []; for (var i = 0; i < 200; i++) { a.push(new Date(i * 1000)); var junk = [i, { i }, 'x' + i]; } var s = 0; for (var d of a) s += d.getTime(); var r = s + ' ' + a[199].toISOString();", "r", "19900000 1970-01-01T00:03:19.000Z")]);
}
