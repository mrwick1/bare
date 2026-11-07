//! Tests of the login submit/control foundation (`browser_form.rs`): control reflection, form owner, the driver's click
//! activation and `SubmitEvent`. Real scripts run in a real realm over a real `Document`. Expectations are written by hand
//! from the HTML and DOM Standards (no Node DOM oracle, nothing compared with Chrome). Each test runs on a big stack.

use crate::browser::*;
use crate::browser_loader::{Input, Stop};
use crate::browser_session::*;
use crate::resources::test_server::{Reply, serve};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page_with(body: &str, lim: BrowserLimits, steps: u64) -> Browser {
    let doc = tree::parse(&format!("<!doctype html><html><body>{body}</body></html>"));
    Browser::new(doc, lim, EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn page(body: &str) -> Browser {
    page_with(body, BrowserLimits::default(), 5_000_000)
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

fn outcome(r: Result<Dispatched, Error>) -> String {
    match r {
        Ok(d) => format!("ok:{}:{}", d.not_canceled, d.reports.len()),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn click(b: &mut Browser, sel: &str) -> Result<Dispatched, Error> {
    let n = b.query(sel).unwrap().unwrap_or_else(|| panic!("no {sel}"));
    b.click(n)
}

/// Every `form` logs its `submit` as `submit@id:submitter`, and calls `preventDefault` (the native navigation is gated).
const WIRE: &str = "var log = []; var forms = document.querySelectorAll('form');
    for (var i = 0; i < forms.length; i++) { (function (f) { f.addEventListener('submit', function (e) { e.preventDefault(); log.push('submit@' + f.id + ':' + (e.submitter && e.submitter.id)) }) })(forms[i]) }
    var go = document.getElementById('go'); 'wired'";

// ------------------------------------------------------------------------------------------ activation

#[test]
fn an_enabled_novalidate_submit_click_dispatches_one_submit_with_the_submitter() {
    big(|| {
        for (control, label) in [
            ("<button id=go type=submit>go</button>", "button submit"),
            ("<button id=go>go</button>", "button with no type"),
            ("<button id=go type=foo>go</button>", "button with an invalid type"),
            ("<input id=go type=submit value=go>", "input submit"),
            ("<input id=go type=SUBMIT>", "input submit, upper case"),
            ("<input id=go type=image>", "input image"),
        ] {
            let mut b = page(&format!("<form id=f novalidate>{control}</form>"));
            js(&mut b, "var log = []; var f = document.getElementById('f'), go = document.getElementById('go');
                go.addEventListener('click', function (e) { log.push('click:' + e.isTrusted) });
                document.addEventListener('submit', function () { log.push('doc-submit') });
                f.addEventListener('submit', function (e) { e.preventDefault();
                  log.push('submit:' + (e instanceof SubmitEvent) + ':' + (e.submitter === go) + ':' + e.isTrusted + ':' + e.target.id + ':' + e.bubbles + e.cancelable + ':' + e.eventPhase) });
                'wired'");
            let r = click(&mut b, "#go").unwrap();
            assert!(r.not_canceled && r.reports.is_empty(), "{label}: {r:?}");
            assert_eq!(js(&mut b, "log.join()"), "click:false,submit:true:true:false:f:truetrue:2,doc-submit", "{label}");
        }
    });
}

#[test]
fn disabled_controls_get_no_click_and_no_default_action() {
    big(|| {
        let mut b = page("<form id=f novalidate><button id=go disabled>x</button><input id=in type=submit disabled>
            <fieldset disabled><legend><button id=leg>l</button></legend><button id=fs>f</button><div><input id=fs2 type=submit></div></fieldset>
            <button id=host disabled><span id=inner>y</span></button></form>");
        js(&mut b, WIRE);
        js(&mut b, "document.addEventListener('click', function (e) { log.push('click:' + e.target.id) }, true); 'ok'");
        for sel in ["#go", "#in", "#fs", "#fs2"] {
            let r = click(&mut b, sel).unwrap();
            assert!(r.not_canceled, "{sel}");
        }
        assert_eq!(js(&mut b, "log.join()"), "", "a disabled form control, also through its fieldset, is not dispatched at");
        // The first legend child of a disabled fieldset is outside the disabled set (HTML: "disabled").
        click(&mut b, "#leg").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click:leg,submit@f:leg");
        // A click at a descendant is dispatched (the standard says "on the element"), the disabled button does not activate.
        js(&mut b, "log.length = 0; 'x'");
        click(&mut b, "#inner").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click:inner");
        // Enabled again: it works.
        js(&mut b, "log.length = 0; go.removeAttribute('disabled'); 'x'");
        click(&mut b, "#go").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click:go,submit@f:go");
    });
}

#[test]
fn a_canceled_click_does_not_activate() {
    big(|| {
        for hook in [
            "go.addEventListener('click', function (e) { e.preventDefault() })",
            "document.getElementById('f').addEventListener('click', function (e) { e.preventDefault() })",
            "window.addEventListener('click', function (e) { e.preventDefault() }, true)",
        ] {
            let mut b = page("<form id=f novalidate><button id=go>go</button></form>");
            js(&mut b, WIRE);
            js(&mut b, hook);
            let r = click(&mut b, "#go").unwrap();
            assert!(!r.not_canceled, "{hook}");
            assert_eq!(js(&mut b, "log.join()"), "", "{hook}");
        }
        // A listener that only stops propagation does not stop the activation.
        let mut b = page("<form id=f novalidate><button id=go><span id=in>x</span></button></form>");
        js(&mut b, WIRE);
        js(&mut b, "document.getElementById('in').addEventListener('click', function (e) { e.stopPropagation() })");
        click(&mut b, "#in").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "submit@f:go", "the activation target is chosen before the listeners run");
    });
}

#[test]
fn buttons_of_other_types_and_other_controls_never_submit() {
    big(|| {
        for control in [
            "<button id=go type=button>x</button>",
            "<button id=go type=BUTTON>x</button>",
            "<button id=go type=reset>x</button>",
            "<button id=go command=show-modal commandfor=f>x</button>",
            "<button id=go commandfor=f>x</button>",
            "<input id=go type=button>",
            "<input id=go type=reset>",
            "<input id=go type=text>",
            "<input id=go type=checkbox>",
            "<input id=go>",
            "<span id=go>x</span>",
            "<a id=go href=#>x</a>",
            "<label id=go>x</label>",
            "<select id=go><option>x</option></select>",
            "<textarea id=go></textarea>",
            "<button id=go><a id=in href=#>x</a></button>",
        ] {
            let mut b = page(&format!("<form id=f novalidate>{control}</form>"));
            js(&mut b, WIRE);
            let target = if control.contains("id=in") { "#in" } else { "#go" };
            click(&mut b, target).unwrap();
            assert_eq!(js(&mut b, "log.join()"), "", "{control}");
        }
        // An element in another namespace is not a button.
        let mut b = page("<form id=f novalidate></form>");
        js(&mut b, WIRE);
        js(&mut b, "var sb = document.createElementNS('http://www.w3.org/2000/svg', 'button'); sb.setAttribute('type', 'submit');
            document.getElementById('f').appendChild(sb); 'ok'");
        let v = b.run_script("sb").unwrap();
        let n = b.node(v).unwrap();
        b.click(n).unwrap();
        assert_eq!(js(&mut b, "log.join()"), "");
        // A script-made click is not the driver's user activation.
        js(&mut b, "document.getElementById('f').innerHTML = '<button id=go>x</button>'; go = document.getElementById('go'); go.dispatchEvent(new Event('click', { bubbles: true, cancelable: true })); 'sent'");
        assert_eq!(js(&mut b, "log.join()"), "");
    });
}

#[test]
fn state_read_after_the_click_decides_the_activation() {
    big(|| {
        const FORMS: &str = "<form id=f novalidate><button id=go>go</button></form><form id=g novalidate></form>";
        let cases: [(&str, &str, &str); 12] = [
            ("type changes", "go.type = 'button'", "click"),
            ("disabled set", "go.disabled = true", "click"),
            ("removed from the form", "document.getElementById('f').removeChild(go)", "click"),
            ("moved to another form", "document.getElementById('g').appendChild(go)", "click,submit@g:go"),
            ("form attribute set", "go.setAttribute('form', 'g')", "click,submit@g:go"),
            ("form removed from the document", "document.getElementById('f').remove()", "click"),
            ("moved into a detached form", "document.createElement('form').appendChild(go)", "click"),
            ("moved into a foreign element", "document.createElementNS('http://www.w3.org/2000/svg', 'form').appendChild(go)", "click"),
            ("noValidate cleared, form attribute pointing nowhere", "go.setAttribute('form', 'nope')", "click"),
            ("name only", "go.name = 'n'", "click,submit@f:go"),
            ("type set to an invalid value", "go.type = 'bogus'", "click,submit@f:go"),
            ("command added", "go.setAttribute('command', 'x')", "click"),
        ];
        for (label, body, want) in cases {
            let mut b = page(FORMS);
            js(&mut b, WIRE);
            js(&mut b, &format!("go.addEventListener('click', function () {{ log.push('click'); {body} }})"));
            click(&mut b, "#go").unwrap();
            assert_eq!(js(&mut b, "log.join()"), want, "{label}");
        }
        // A form attribute that is removed during the click: the ancestor form is the owner again.
        let mut b = page("<form id=f novalidate><button id=go form=g>go</button></form><form id=g novalidate></form>");
        js(&mut b, WIRE);
        js(&mut b, "go.addEventListener('click', function () { log.push('click'); go.removeAttribute('form') })");
        click(&mut b, "#go").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click,submit@f:go");
        // An `<input type=submit>` that becomes text at click time does not submit.
        let mut b = page("<form id=f novalidate><input id=go type=submit></form>");
        js(&mut b, WIRE);
        js(&mut b, "go.addEventListener('click', function () { log.push('click'); go.type = 'text' })");
        click(&mut b, "#go").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click");
        // The form's own novalidate changes at click time.
        let mut b = page("<form id=f><button id=go>go</button></form>");
        js(&mut b, WIRE);
        js(&mut b, "go.addEventListener('click', function () { log.push('click'); document.getElementById('f').noValidate = true })");
        click(&mut b, "#go").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "click,submit@f:go");
    });
}

// ------------------------------------------------------------------------------------------ form owner

#[test]
fn the_form_owner_follows_the_form_attribute_then_the_nearest_ancestor() {
    big(|| {
        let mut b = page("<form id=f><button id=b1></button><input id=i1></form><button id=b2 form=f></button>
            <div id=notform></div><form id=f2><button id=b5 form=notform></button><button id=b6 form=nope></button><button id=b7 form=''></button></form>
            <div id=dup></div><form id=dup><button id=b8 form=dup></button></form>");
        assert_eq!(js(&mut b, "var f = document.getElementById('f'), b1 = document.getElementById('b1'), i1 = document.getElementById('i1'), b2 = document.getElementById('b2'), b5 = document.getElementById('b5'), b6 = document.getElementById('b6'), b7 = document.getElementById('b7'), b8 = document.getElementById('b8'); [b1.form === f, i1.form === f, b2.form === f, b5.form, b6.form, b7.form, b8.form].join()"), "true,true,true,,,,");
        // Nested forms (only a script makes them): the nearest ancestor.
        assert_eq!(js(&mut b, "var outer = document.createElement('form'), inner = document.createElement('form'), bn = document.createElement('button'), mid = document.createElement('div');
            outer.appendChild(mid); mid.appendChild(inner); inner.appendChild(bn); [bn.form === inner, mid.appendChild(bn) === bn, bn.form === outer].join()"), "true,true,true");
        // An id that changes, or a form attribute that changes, is read again.
        assert_eq!(js(&mut b, "b2.removeAttribute('form'); var r1 = b2.form; var g = document.getElementById('f2'); b2.setAttribute('form', 'f2'); var r2 = b2.form === g; g.id = 'zzz'; var r3 = b2.form; g.id = 'f2'; [r1, r2, r3, b2.form === g].join()"), ",true,,true");
        // Disconnected: the explicit owner needs a connected control, so the ancestor form is the owner.
        assert_eq!(js(&mut b, "var df = document.createElement('form'), bb = document.createElement('button'); df.appendChild(bb); bb.setAttribute('form', 'f'); var d1 = bb.form === df; document.body.appendChild(df); [d1, bb.form === f].join()"), "true,true");
        // A form attribute does not follow the control into another form; a foreign `form` is not a form.
        assert_eq!(js(&mut b, "var sf = document.createElementNS('http://www.w3.org/2000/svg', 'form'); sf.id = 'sf'; document.body.appendChild(sf);
            var hb = document.createElement('button'); sf.appendChild(hb); var e1 = hb.form; hb.setAttribute('form', 'sf'); document.body.appendChild(hb); [e1, hb.form].join()"), ",");
        // `form` is a read-only accessor, not a place for an expando.
        assert_eq!(js(&mut b, "b1.form = 5; [b1.form === f, b1.hasOwnProperty('form'), typeof Object.getOwnPropertyDescriptor(HTMLButtonElement.prototype, 'form').get, Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'form').set].join()"), "true,false,function,");
    });
}

#[test]
fn parsed_and_cloned_trees_keep_the_same_owner_rules() {
    big(|| {
        let mut b = page("<div id=host></div>");
        assert_eq!(js(&mut b, "var host = document.getElementById('host'); host.innerHTML = '<form id=x novalidate><button id=y name=n disabled>b</button></form><input id=z form=x>';
            var x = document.getElementById('x'), y = document.getElementById('y'), z = document.getElementById('z');
            [x instanceof HTMLFormElement, y instanceof HTMLButtonElement, y.form === x, z.form === x, y.name, y.disabled, x.noValidate].join()"), "true,true,true,true,n,true,true");
        // A deep clone: the clone's button belongs to the clone's form, which is not connected.
        assert_eq!(js(&mut b, "var c = x.cloneNode(true), cb = c.firstChild; [c instanceof HTMLFormElement, cb instanceof HTMLButtonElement, cb.form === c, cb.form === x, cb.name, cb.disabled, c.noValidate, c.isConnected].join()"), "true,true,true,false,n,true,true,false");
        // A shallow clone of the button has no owner until it is put in a form; the form attribute is copied.
        assert_eq!(js(&mut b, "var s = z.cloneNode(false); var t1 = s.form; document.body.appendChild(s); [t1, s.form === x].join()"), ",true");
        // Moving the whole form keeps its controls.
        assert_eq!(js(&mut b, "var other = document.createElement('div'); document.body.appendChild(other); other.appendChild(x); y.form === x"), "true");
        // Collection keeps the brands and the rules.
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var y2 = document.getElementById('y'); [y2 instanceof HTMLButtonElement, y2.form.id, y2.type].join()"), "true,x,submit");
    });
}

// ------------------------------------------------------------------------------------------ reflection

#[test]
fn name_disabled_and_type_reflect_the_native_attributes() {
    big(|| {
        let mut b = page("<form id=f><input id=i><button id=b>x</button></form>");
        assert_eq!(js(&mut b, "var i = document.getElementById('i'), bt = document.getElementById('b'), f = document.getElementById('f');
            i.name = 'email'; [i.getAttribute('name'), i.name, i.hasOwnProperty('name'), document.querySelector('input[name=email]') === i, document.querySelectorAll('[name]').length].join()"), "email,email,false,true,1");
        assert_eq!(js(&mut b, "i.name = 5; var a = i.name; i.name = undefined; var u = i.name; i.name = null; [a, u, i.name, i.getAttribute('name')].join()"), "5,undefined,null,null");
        assert_eq!(js(&mut b, "bt.name = 'go'; i.setAttribute('name', 'x'); [bt.name, i.name, bt.getAttribute('name'), document.querySelector('button[name=go]') === bt].join()"), "go,x,go,true");
        // Only the null namespace counts.
        assert_eq!(js(&mut b, "var j = document.createElement('input'); j.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:name', 'q'); j.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:disabled', ''); [j.name === '', j.disabled, j.hasAttribute('name'), j.hasAttribute('xlink:name')].join()"), "true,false,false,true");
        // Boolean reflection: ToBoolean on set, presence on get; false removes.
        assert_eq!(js(&mut b, "bt.disabled = 'false'; var d1 = [bt.disabled, bt.getAttribute('disabled'), bt.hasAttribute('disabled')]; bt.disabled = 0; var d2 = [bt.disabled, bt.hasAttribute('disabled')];
            i.disabled = {}; var d3 = [i.disabled, i.hasAttribute('disabled')]; i.setAttribute('disabled', 'disabled'); i.disabled = ''; [d1, d2, d3, i.disabled].join('|')"), "true,,true|false,false|true,true|false");
        assert_eq!(js(&mut b, "f.noValidate = []; var n1 = [f.noValidate, f.hasAttribute('novalidate')]; f.noValidate = NaN; [n1, f.noValidate, f.hasAttribute('novalidate')].join('|')"), "true,true|false|false");
        assert_eq!(js(&mut b, "bt.formNoValidate = 1; i.formNoValidate = 1; var n = [bt.hasAttribute('formnovalidate'), i.hasAttribute('formnovalidate'), bt.formNoValidate, i.formNoValidate]; bt.formNoValidate = 0; [n, bt.formNoValidate].join('|')"), "true,true,true,true|false");
        // Button `type`: the keywords, case-insensitive; a missing or invalid value is a submit button; the setter writes the text.
        assert_eq!(js(&mut b, "var out = [bt.type]; bt.type = 'RESET'; out.push(bt.type, bt.getAttribute('type')); bt.type = { toString: function () { return 'Button' } }; out.push(bt.type);
            bt.type = 'nonsense'; out.push(bt.type, bt.getAttribute('type')); bt.removeAttribute('type'); out.push(bt.type); out.join()"), "submit,reset,RESET,button,submit,nonsense,submit");
        // With `command` or `commandfor`, the missing or invalid state is not a submit button and reads "button".
        assert_eq!(js(&mut b, "bt.setAttribute('command', 'x'); var c1 = bt.type; bt.type = 'submit'; var c2 = bt.type; bt.removeAttribute('type'); bt.removeAttribute('command'); bt.setAttribute('commandfor', 'f'); [c1, c2, bt.type].join()"), "button,submit,button");
        // The control reflections stay native: `input.type` and `value` are untouched.
        assert_eq!(js(&mut b, "i.type = 'password'; i.value = 'secret'; i.name = 'pw'; [i.type, i.value, i.name, i.getAttribute('type')].join()"), "password,secret,pw,password");
    });
}

#[test]
fn brands_and_receivers_are_checked() {
    big(|| {
        let mut b = page("<form id=f><input id=i><button id=b>x</button></form><div id=d></div>");
        assert_eq!(js(&mut b, "var f = document.getElementById('f'), i = document.getElementById('i'), bt = document.getElementById('b'), d = document.getElementById('d');
            [f instanceof HTMLFormElement, f instanceof HTMLElement, bt instanceof HTMLButtonElement, bt instanceof HTMLInputElement, i instanceof HTMLButtonElement, d instanceof HTMLFormElement,
             typeof HTMLFormElement, typeof HTMLButtonElement, typeof SubmitEvent, HTMLButtonElement.prototype.constructor === HTMLButtonElement,
             Object.getPrototypeOf(HTMLButtonElement.prototype) === HTMLElement.prototype, Object.getPrototypeOf(SubmitEvent.prototype) === Event.prototype].join()"),
            "true,true,true,false,false,false,function,function,function,true,true,true");
        assert_eq!(js(&mut b, "var sv = document.createElementNS('http://www.w3.org/2000/svg', 'button'), si = document.createElementNS('http://www.w3.org/2000/svg', 'input'), sf = document.createElementNS('http://www.w3.org/2000/svg', 'form');
            [sv instanceof HTMLButtonElement, si instanceof HTMLInputElement, sf instanceof HTMLFormElement, sv.name, si.name, sf.noValidate, si.form, sv.type].join()"), "false,false,false,,,,,");
        for (proto, prop, bad) in [
            ("HTMLButtonElement", "name", "i"), ("HTMLButtonElement", "type", "i"), ("HTMLButtonElement", "disabled", "d"), ("HTMLButtonElement", "form", "sv"),
            ("HTMLButtonElement", "formNoValidate", "f"), ("HTMLInputElement", "name", "bt"), ("HTMLInputElement", "disabled", "si"), ("HTMLInputElement", "form", "d"),
            ("HTMLInputElement", "formNoValidate", "bt"), ("HTMLFormElement", "noValidate", "i"), ("HTMLFormElement", "noValidate", "sf"),
        ] {
            for recv in [bad, "{}", "undefined", "document", "window"] {
                for op in ["get.call", "set.call"] {
                    let src = format!("var dd = Object.getOwnPropertyDescriptor({proto}.prototype, '{prop}'); var fn = dd.{}; if (!fn) 'none'; else {{ try {{ fn.call({recv}, '1'); 'no throw' }} catch (e) {{ e.name + ':' + e.message }} }}", op.split('.').next().unwrap());
                    let got = js(&mut b, &src);
                    assert!(got == "TypeError:Illegal invocation" || (got == "none" && prop == "form" && op == "set.call"), "{proto}.{prop} {op} on {recv}: {got}");
                }
            }
            // The prototype itself is not an instance.
            assert_eq!(js(&mut b, &format!("try {{ {proto}.prototype.{prop}; 'no throw' }} catch (e) {{ e.name }}")), "TypeError", "{proto}.prototype.{prop}");
        }
        assert_eq!(js(&mut b, "try { new HTMLButtonElement() } catch (e) { e.name + ':' + e.message }"), "TypeError:Failed to construct 'HTMLButtonElement': Illegal constructor");
        assert_eq!(js(&mut b, "try { HTMLButtonElement() } catch (e) { e.name + ':' + e.message }"), "TypeError:Failed to construct 'HTMLButtonElement': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
        assert_eq!(js(&mut b, "try { new HTMLFormElement() } catch (e) { e.name }"), "TypeError");
        // A symbol cannot become a string: the property is not touched.
        assert_eq!(js(&mut b, "bt.name = 'keep'; try { bt.name = Symbol('s') } catch (e) { var tn = e.name } [tn, bt.name].join()"), "TypeError,keep");
        // Instances of a descendant `input` keep their own accessors.
        assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').get + ',' + (Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'name').get !== Object.getOwnPropertyDescriptor(HTMLButtonElement.prototype, 'name').get)"), "function,true");
    });
}

#[test]
fn a_cap_error_leaves_no_partial_attribute() {
    big(|| {
        let body = "<form id=f novalidate><input id=i name=a><button id=b>x</button></form>";
        let used = page(body).stats().attr_slots;
        let mut b = page_with(body, BrowserLimits { max_attr_slots: used, ..BrowserLimits::default() }, 5_000_000);
        js(&mut b, "var f = document.getElementById('f'), i = document.getElementById('i'), bt = document.getElementById('b'); 'x'");
        assert_eq!(js(&mut b, "bt.name = 'n'"), lim("dom-attrs"));
        assert_eq!(js(&mut b, "bt.disabled = true"), lim("dom-attrs"));
        assert_eq!(js(&mut b, "bt.type = 'button'"), lim("dom-attrs"));
        assert_eq!(js(&mut b, "i.formNoValidate = true"), lim("dom-attrs"));
        assert_eq!(js(&mut b, "f.noValidate = false; [bt.hasAttribute('name'), bt.hasAttribute('disabled'), bt.hasAttribute('type'), i.hasAttribute('formnovalidate')].join()"), "false,false,false,false");
        // Removing never needs a slot.
        assert_eq!(js(&mut b, "i.disabled = false; i.removeAttribute('name'); i.name"), "");
        // A string over the cap is refused before anything is written.
        let mut b = page_with(body, BrowserLimits { max_string_bytes: 8, ..BrowserLimits::default() }, 5_000_000);
        js(&mut b, "var i = document.getElementById('i'); 'x'");
        assert_eq!(js(&mut b, "i.name = 'much too long for the cap'"), lim("dom-string"));
        assert_eq!(js(&mut b, "i.name"), "a");
    });
}

// ------------------------------------------------------------------------------------------ SubmitEvent

#[test]
fn submit_event_is_a_real_interface_with_a_read_only_submitter() {
    big(|| {
        let mut b = page("<form id=f novalidate><button id=go>x</button><span id=sp>s</span></form>");
        js(&mut b, "var f = document.getElementById('f'), go = document.getElementById('go'), sp = document.getElementById('sp'); 'x'");
        assert_eq!(js(&mut b, "var e0 = new SubmitEvent('submit'); [e0 instanceof SubmitEvent, e0 instanceof Event, e0.type, e0.submitter, e0.bubbles, e0.cancelable, e0.isTrusted, e0.defaultPrevented].join()"), "true,true,submit,,false,false,false,false");
        assert_eq!(js(&mut b, "var e1 = new SubmitEvent('submit', { submitter: go, bubbles: true, cancelable: true }); [e1.submitter === go, e1.bubbles, e1.cancelable, new SubmitEvent('x', { submitter: null }).submitter, new SubmitEvent('x', { submitter: undefined }).submitter, new SubmitEvent('x', { submitter: sp }).submitter === sp].join()"), "true,true,true,,,true");
        for bad in ["{}", "5", "'go'", "document", "window", "document.createElementNS('http://www.w3.org/2000/svg', 'g')", "document.createTextNode('t')"] {
            let got = js(&mut b, &format!("try {{ new SubmitEvent('submit', {{ submitter: {bad} }}); 'no throw' }} catch (e) {{ e.name }}"));
            assert_eq!(got, "TypeError", "{bad}");
        }
        assert_eq!(js(&mut b, "try { new SubmitEvent() } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { SubmitEvent('submit') } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { new SubmitEvent('submit', 5) } catch (e) { e.name }"), "TypeError");
        // Read-only accessor on the prototype, with brand check.
        assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(SubmitEvent.prototype, 'submitter'); e1.submitter = null; [typeof d.get, d.set, e1.submitter === go, e1.hasOwnProperty('submitter'), Object.keys(e1).length].join()"), "function,,true,false,0");
        assert_eq!(js(&mut b, "var r = []; var bads = [new Event('submit'), new CustomEvent('submit'), new FocusEvent('submit'), {}, undefined]; for (var k = 0; k < bads.length; k++) { try { d.get.call(bads[k]); r.push('no throw') } catch (e) { r.push(e.name + ':' + e.message) } } r.join()"),
            "TypeError:Illegal invocation,TypeError:Illegal invocation,TypeError:Illegal invocation,TypeError:Illegal invocation,TypeError:Illegal invocation");
        // A script-made submit event dispatched at a form submits nothing: no gate, no navigation, no other event.
        assert_eq!(js(&mut b, "var log = []; f.addEventListener('submit', function (e) { log.push('submit:' + (e === e1)) }); f.addEventListener('click', function () { log.push('click') });
            var res = f.dispatchEvent(e1); [res, log.join()].join('|')"), "true|submit:true");
    });
}

#[test]
fn script_made_activation_events_and_submit_events_do_not_submit() {
    big(|| {
        let mut b = page("<form id=f><button id=go>x</button></form>");
        js(&mut b, "var log = [], f = document.getElementById('f'), go = document.getElementById('go');
            f.addEventListener('submit', function (e) { log.push('submit') }); go.addEventListener('click', function () { log.push('click') }); 'x'");
        // No validation gate and no navigation gate: the form is not novalidate, and nothing cancels the events.
        assert_eq!(js(&mut b, "go.dispatchEvent(new Event('click', { bubbles: true, cancelable: true })) + ',' + log.join()"), "true,click");
        assert_eq!(js(&mut b, "log.length = 0; f.dispatchEvent(new SubmitEvent('submit', { submitter: go, cancelable: true })) + ',' + log.join()"), "true,submit");
        assert_eq!(js(&mut b, "log.length = 0; f.dispatchEvent(new Event('submit')) + ',' + log.join()"), "true,submit");
    });
}

// ------------------------------------------------------------------------------------------ validation and navigation

#[test]
fn validation_and_native_navigation_follow_the_standard() {
    big(|| {
        // 1. An invalid field stops the click after `click`: `invalid` at the field, no `submit`, no navigation.
        let mut b = page("<form id=f><input id=e type=email required><button id=go>go</button></form>");
        b.set_document_url("http://t.test/p").unwrap();
        js(&mut b, WIRE);
        js(&mut b, "go.addEventListener('click', function () { log.push('click') }); document.getElementById('e').addEventListener('invalid', function (e) { log.push('invalid:' + e.bubbles + e.cancelable) })");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        assert_eq!(js(&mut b, "log.join()"), "click,invalid:falsetrue", "no submit event: an invalid form does not fire it");
        assert!(b.take_navigation_request().is_none());
        // 2. formnovalidate on the submitter skips the checks. A submit that nobody cancels navigates.
        let mut b = page("<form id=f><input name=a value=1><button id=go formnovalidate>go</button></form>");
        b.set_document_url("http://t.test/p").unwrap();
        js(&mut b, "var log = []; var f = document.getElementById('f'); f.addEventListener('submit', function () { log.push('submit') }); 'x'");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        assert_eq!(js(&mut b, "log.join()"), "submit");
        let r = b.take_navigation_request().expect("a navigation");
        assert_eq!(r.url, "http://t.test/p?a=1");
        // 3. With the submit canceled there is no navigation.
        js(&mut b, "f.addEventListener('submit', function (e) { e.preventDefault() }); 'x'");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        assert!(b.take_navigation_request().is_none());
        // 4. The form removed by a submit listener that did not cancel: it cannot navigate.
        let mut b = page("<form id=f novalidate><button id=go>go</button></form>");
        js(&mut b, "var f = document.getElementById('f'); f.addEventListener('submit', function () { f.remove() }); 'x'");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        // 5. `input type=submit` with formnovalidate behaves the same.
        let mut b = page("<form id=f><input id=go type=submit formnovalidate></form>");
        js(&mut b, "var log = [], f = document.getElementById('f'); f.addEventListener('submit', function (e) { e.preventDefault(); log.push('s') }); 'x'");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        assert_eq!(js(&mut b, "log.join()"), "s");
        // 6. No document URL: a form that nobody cancels cannot name the page to load.
        let mut b = page("<form id=f novalidate><button id=go>go</button></form>");
        let got = outcome(click(&mut b, "#go"));
        assert!(got.starts_with("unsupported:") && got.contains("URL"), "{got}");
    });
}

#[test]
fn the_dispatch_only_driver_submit_keeps_its_contract() {
    big(|| {
        let mut b = page("<form id=f><input required><button id=go>go</button></form>");
        js(&mut b, "var n = 0; document.getElementById('f').addEventListener('submit', function (e) { n++; e.preventDefault() }); 'x'");
        let f = b.query("#f").unwrap().unwrap();
        let r = b.submit(f).unwrap();
        assert!(!r.not_canceled);
        let r = b.submit(f).unwrap();
        assert!(!r.not_canceled);
        assert_eq!(js(&mut b, "n"), "2", "no validation, no navigation, plain events: as before");
        let mut b = page("<form id=f><button id=go>go</button></form>");
        let f = b.query("#f").unwrap().unwrap();
        assert!(b.submit(f).unwrap().not_canceled, "an uncanceled driver submit is not gated");
    });
}

// ------------------------------------------------------------------------------------------ limits, throws, GC

#[test]
fn listener_throws_are_reported_in_both_stages_and_dispatch_goes_on() {
    big(|| {
        let mut b = page("<form id=f novalidate><button id=go>go</button></form>");
        js(&mut b, "var log = [], f = document.getElementById('f'), go = document.getElementById('go');
            go.addEventListener('click', function () { log.push('click1'); throw new Error('c1') });
            go.addEventListener('click', function () { log.push('click2') });
            f.addEventListener('submit', function () { log.push('submit1'); throw new TypeError('s1') });
            f.addEventListener('submit', function (e) { log.push('submit2'); e.preventDefault() }); 'x'");
        let r = click(&mut b, "#go").unwrap();
        assert!(r.not_canceled, "the click was not canceled");
        assert_eq!(js(&mut b, "log.join()"), "click1,click2,submit1,submit2");
        let seen: Vec<_> = r.reports.iter().map(|x| (x.event_type.as_str(), x.target.as_str(), x.message.as_str())).collect();
        assert_eq!(seen, [("click", "button", "Error: c1"), ("submit", "form", "TypeError: s1")]);
        assert_eq!(b.take_reports().len(), 2);
    });
}

#[test]
fn a_limit_in_the_click_stops_before_the_submit_and_a_limit_in_the_submit_stops_later_javascript() {
    big(|| {
        let body = "<form id=f novalidate><button id=go>go</button></form>";
        // In the click: later click listeners, and the submit, do not run.
        let mut b = page_with(body, BrowserLimits::default(), 200_000);
        js(&mut b, "var log = [], f = document.getElementById('f'), go = document.getElementById('go');
            go.addEventListener('click', function () { log.push('c1'); while (true) {} });
            go.addEventListener('click', function () { log.push('c2') });
            f.addEventListener('submit', function () { log.push('s1') }); 'x'");
        let got = outcome(click(&mut b, "#go"));
        assert!(got.starts_with("limit:"), "{got}");
        assert_eq!(js(&mut b, "log.join()"), "c1");
        // In the submit: the next submit listener and the bubbling document listener do not run.
        let mut b = page_with(body, BrowserLimits::default(), 200_000);
        js(&mut b, "var log = [], f = document.getElementById('f');
            f.addEventListener('submit', function () { log.push('s1'); while (true) {} });
            f.addEventListener('submit', function () { log.push('s2') });
            document.addEventListener('submit', function () { log.push('doc') }); 'x'");
        let got = outcome(click(&mut b, "#go"));
        assert!(got.starts_with("limit:"), "{got}");
        assert_eq!(js(&mut b, "log.join()"), "s1");
        // The event cap: the submit event cannot be made, so no submit listener runs and no event is left behind.
        let mut b = page_with(body, BrowserLimits { max_events: 1, ..BrowserLimits::default() }, 5_000_000);
        js(&mut b, "var log = [], f = document.getElementById('f'); f.addEventListener('submit', function () { log.push('s') }); document.getElementById('go').addEventListener('click', function () { log.push('c') }); 'x'");
        assert_eq!(outcome(click(&mut b, "#go")), lim("dom-events"));
        assert_eq!(js(&mut b, "log.join()"), "c");
        assert_eq!(b.stats().events, 1, "only the click event exists");
    });
}

#[test]
fn the_click_and_its_activation_share_one_step_budget() {
    big(|| {
        let body = "<form id=f novalidate><button id=go>go</button></form>";
        let setup = "var f = document.getElementById('f'), go = document.getElementById('go'), done = [];
            function spin(n) { var i = 0; while (i < n) { i++ } }
            go.addEventListener('click', function () { spin(N1); done.push('click') });
            f.addEventListener('submit', function (e) { e.preventDefault(); spin(N2); done.push('submit') }); 'x'";
        // Each part fits alone.
        for (n1, n2) in [(8_000, 0), (0, 8_000)] {
            let mut b = page_with(body, BrowserLimits::default(), 100_000);
            js(&mut b, &format!("var N1 = {n1}, N2 = {n2}; {setup}"));
            assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0", "{n1} {n2}");
        }
        // Together they do not: the budget of the one host call ran out, not the budget of each dispatch.
        let mut b = page_with(body, BrowserLimits::default(), 100_000);
        js(&mut b, &format!("var N1 = 8000, N2 = 8000; {setup}"));
        let got = outcome(click(&mut b, "#go"));
        assert!(got.starts_with("limit:"), "{got}");
        assert_eq!(js(&mut b, "done.join()"), "click", "the submit listener started and ran out");
    });
}

#[test]
fn a_saved_event_keeps_its_submitter_and_the_slots_are_reused_safely() {
    big(|| {
        let mut b = page("<form id=f novalidate><button id=go name=g>go</button></form>");
        // Two scripts: the callback lives in the first, the wiring in the second.
        js(&mut b, "var saved = null; function keep(e) { e.preventDefault(); saved = e } 'a'");
        js(&mut b, "document.getElementById('f').addEventListener('submit', function (e) { keep(e) }); 'b'");
        assert_eq!(outcome(click(&mut b, "#go")), "ok:true:0");
        // Only the saved event holds the detached form and its button now.
        assert_eq!(js(&mut b, "document.getElementById('f').remove(); 'x'"), "x");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "for (var k = 0; k < 3000; k++) { var o = { a: k, b: [k, { c: k }] }; document.createElement('p') } 'churn'"), "churn");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "[saved.submitter.id, saved.submitter.tagName, saved.submitter.name, saved.target.id, saved.submitter.form === saved.target, saved.submitter.parentNode === saved.target, saved instanceof SubmitEvent, saved.isTrusted].join()"),
            "go,BUTTON,g,f,true,true,true,false");
        // Dropped: the event, the form and the button go, and nothing keeps them alive.
        let before = b.stats();
        js(&mut b, "saved = null; 'x'");
        b.collect(&[]).unwrap();
        let after = b.stats();
        assert_eq!(after.events, 0);
        assert!(after.wrappers < before.wrappers, "{before:?} {after:?}");
        // A second round on a fresh form after the collection works with fresh wrappers.
        js(&mut b, "var f2 = document.createElement('form'); f2.noValidate = true; var b2 = document.createElement('button'); b2.id = 'b2'; f2.appendChild(b2); document.body.appendChild(f2); var seen = [];
            f2.addEventListener('submit', function (e) { e.preventDefault(); seen.push(e.submitter === b2) }); 'x'");
        assert_eq!(outcome(click(&mut b, "#b2")), "ok:true:0");
        assert_eq!(js(&mut b, "seen.join()"), "true");
    });
}

// ------------------------------------------------------------------------------------------ session

fn html(body: &str) -> Reply {
    Reply::new(200, Some("text/html; charset=utf-8"), body)
}

/// POST requests that the loopback server saw for `path` (`Server::hits` counts GET only).
fn posts(srv: &crate::resources::test_server::Server, path: &str) -> usize {
    let line = format!("post {path} http/1.1");
    srv.heads.lock().unwrap().iter().filter(|h| h.starts_with(&line)).count()
}

fn cfg(url: String) -> SessionConfig {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

/// A login form built by script like an app does: controlled fields, `noValidate`, a submit button that is disabled until both
/// fields have text. The submit handler is the app's: it cancels the event and calls `fetch`.
const LOGIN: &str = "<div id=root></div><script>
    var email = '', pw = '', form = document.createElement('form'), em = document.createElement('input'), pwd = document.createElement('input'), btn = document.createElement('button');
    form.noValidate = true; em.id = 'email'; em.name = 'email'; em.type = 'email'; pwd.id = 'pw'; pwd.name = 'password'; pwd.type = 'password'; btn.id = 'login'; btn.type = 'submit'; btn.disabled = true; btn.textContent = 'Log in';
    form.appendChild(em); form.appendChild(pwd); form.appendChild(btn); document.getElementById('root').appendChild(form);
    function sync() { btn.disabled = !(email && pw) }
    em.addEventListener('input', function (e) { email = e.target.value; sync() });
    pwd.addEventListener('input', function (e) { pw = e.target.value; sync() });
    form.addEventListener('submit', function (e) { e.preventDefault();
      fetch('/api/login', { method: 'POST', body: JSON.stringify({ email: email, password: pw }) }).then(function (r) { return r.text() }).then(function (t) { localStorage.setItem('login', t) }) });
    </script>";

#[test]
fn session_resume_click_runs_the_authored_on_submit_fetch_once_over_loopback() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => html(LOGIN),
            "/api/login" => Reply::new(200, Some("text/plain"), "welcome"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut s = Session::new(cfg(srv.url("/")));
        assert_eq!(s.run().stop, SessionStop::Page(Stop::Settled));
        assert_eq!(posts(&srv, "/api/login"), 0);
        // The button is disabled: the click is not dispatched and the app does nothing.
        let r = s.resume(&[Input::Click("#login")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(posts(&srv, "/api/login"), 0, "a disabled submit button is inert");
        // Typed fields enable it. One click, one request.
        let r = s.resume(&[Input::Type("#email", "a@b.c"), Input::Type("#pw", "pw1"), Input::Click("#login")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((posts(&srv, "/api/login"), srv.total()), (1, 2), "one POST and the page, nothing else");
        assert_eq!(s.storage().get_item(false, &srv.url(""), "login").as_deref(), Some("welcome"));
        assert_eq!(srv.hits("/"), 1, "no reload: the same realm");
    });
}

#[test]
fn session_resume_click_on_a_form_loads_the_action_with_the_entry_list() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => html("<form id=f action=/search><input name=q value='a b'><input type=checkbox name=c checked><input type=checkbox name=d><button id=go name=go value=1>go</button></form>"),
            p if p.starts_with("/search") => html("<p>found</p>"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut s = Session::new(cfg(srv.url("/")));
        s.run();
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(srv.hits("/search?q=a+b&c=on&go=1"), 1);
    });
}

