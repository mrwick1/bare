use super::tests::check;

#[test]
fn foreach_step_reloads_elements_after_callbacks() {
    check(&[
        ("var a = [undefined, , 2, 3], r = ''; a.forEach(function(v, i, arr) { r += this.x + ':' + v + ':' + i + ':' + (arr === a) + ';'; if (i === 0) { arr[1] = 1; delete arr[2]; arr[4] = 4; } }, {x: 9});", "r", "9:undefined:0:true;9:1:1:true;9:3:3:true;"),
        ("var a = [1, 2, 3], r = ''; a.forEach((v, i) => { r += v; a.length = 1; });", "r", "1"),
        ("var a = [10, , undefined], r = ''; Object.defineProperty(Array.prototype, '1', {get() { r += 'g'; return 4; }, configurable: true}); a.forEach((v, i) => { r += ':' + i + '=' + v; }); delete Array.prototype[1];", "r", ":0=10g:1=4:2=undefined"),
        ("var o = {length: 3, get 0() { return 3; }, 2: 2}, r = ''; Array.prototype.forEach.call(o, (v, i) => { r += v + ':' + i + ';'; });", "r", "3:0;2:2;"),
        ("var r = ''; try { [1, 2, 3].forEach(v => { if (v === 2) throw 'stop'; r += v; }); } catch (e) { r += e; }", "r", "1stop"),
        ("var r = ''; var a = [1, 2]; Object.defineProperty(a, '1', {value: 7, writable: false}); a.forEach(v => { r += v; });", "r", "17"),
    ]);
}

#[test]
fn foreach_step_observes_presence_before_reading_a_host_key() {
    use crate::interp_rt::Realm;
    use crate::value::Value;
    use bare_js_api::{Args, Engine, Error, Host, Local, Named, Pending, Scope};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Log(RefCell<Vec<&'static str>>);

    impl Host<Realm> for Log {
        fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, _this: Local<'s>, _args: Args<'s>, _new: bool) -> Result<Local<'s>, Error<'s>> {
            Ok(s.undefined())
        }

        fn named_has(&self, _o: &Value, _key: u32, name: &[u16]) -> Result<bool, Pending> {
            let indexed = name == [u16::from(b'0')];
            if indexed {
                self.0.borrow_mut().push("has");
            }
            Ok(indexed)
        }

        fn named_get(&self, _o: &Value, _key: u32, name: &[u16]) -> Result<Option<Named<Value>>, Pending> {
            assert_eq!(name, [u16::from(b'0')]);
            self.0.borrow_mut().push("get");
            Ok(Some(Named::Value(Value::UNDEFINED)))
        }
    }

    let mut vm = super::load("").unwrap();
    let log = Rc::new(Log::default());
    vm.realm.set_host(log.clone());
    bare_js_api::enter(&mut vm.realm, |scope| {
        let proto = scope.run_script("({ length: 1 })").unwrap();
        let host = scope.new_host_named(Some(proto), 1, false).unwrap();
        let global = scope.global();
        scope.set_str(global, "host", host).unwrap();
        let result = scope.run_script("var r = 0; Array.prototype.forEach.call(host, v => { if (v === undefined) r++; }); r;").unwrap();
        assert_eq!(scope.as_number(result), Some(1.0));
    });
    assert_eq!(&*log.0.borrow(), &["has", "has", "get"]);
}

#[test]
fn foreach_back_edges_spend_fuel() {
    let mut vm = super::load("var calls = 0; [0, 1, 2, 3, 4].forEach(() => { calls++; });").unwrap();
    vm.fuel = 2;
    assert!(matches!(vm.run_main(0), Err(crate::interp::Exit::Fuel)));
    assert_eq!(vm.global_text("calls"), "3");
}

#[test]
fn foreach_step_verifier_refuses_an_overlapping_argument_window() {
    use crate::bytecode::{Asm, Code, Ins, Op};

    let mut asm = Asm::default();
    let skip = asm.label();
    asm.jump(Ins::new(Op::ForEachStep, 1, 0, 2), skip);
    asm.bind(skip);
    asm.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    let mut code = Code::new(4, 3);
    code.words = asm.finish().unwrap().words;
    code.roots.add(0, [0, 1, 2]);
    let err = crate::verify::verify(&code).unwrap_err();
    assert!(err.contains("the argument window overlaps a source register"), "{err}");
}
