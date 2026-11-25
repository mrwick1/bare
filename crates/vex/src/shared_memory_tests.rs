use super::*;
use crate::script;

#[test]
fn separate_agents_share_growth_atomic_operations_and_waiters() {
    let mut main = script::load(r#"
        var buffer = new SharedArrayBuffer(8, {maxByteLength: 16});
        var view = new Int32Array(buffer);
        var done;
        var pending = Atomics.waitAsync(view, 0, 0, 10000);
        pending.value.then(value => { done = value; });
    "#).unwrap();
    main.realm.install_test262_shared_memory(false);
    main.run_main(0).unwrap();
    let object = main.global("buffer");
    let buffer = main.realm.exported_shared_buffer(object).unwrap();
    let worker = std::thread::Builder::new().stack_size(8 << 20).spawn(move || {
        let mut vm = script::load(r#"
            buffer.grow(16);
            var view = new Int32Array(buffer);
            var old = Atomics.add(view, 3, 42);
            var count = Atomics.notify(view, 0, 1);
        "#).unwrap();
        vm.realm.install_test262_shared_memory(true);
        let object = vm.realm.import_shared_buffer(buffer).unwrap();
        vm.realm.set_own(vm.realm.global, "buffer", object);
        vm.run_main(0).unwrap();
        assert_eq!(vm.global("old").as_int(), Some(0));
        assert_eq!(vm.global("count").as_int(), Some(1));
    }).unwrap();
    worker.join().unwrap();
    main.collect_idle();
    assert!(main.realm.deliver_test262_wait(Duration::ZERO).unwrap());
    main.drain_microtasks(&mut |_, _| {}).unwrap();
    assert_eq!(main.global_text("done"), "ok");
    let view = main.global("view");
    let value = main.realm.get_named(view, "3").unwrap();
    assert_eq!(value.as_int(), Some(42));
}

#[test]
fn shutdown_releases_a_waiter_without_a_timeout() {
    let block = SharedBuffer::new(4, None).unwrap();
    let control = Test262AgentControl::default();
    let child = control.clone();
    let copy = block.clone();
    let join = std::thread::spawn(move || copy.wait(0, &[0; 4], None, Some(&child)));
    control.stop();
    assert_eq!(join.join().unwrap(), None);
    assert_eq!(block.notify(0, 1, |_| {}), 0);
}

#[test]
fn same_agent_notify_queues_reactions_before_later_microtasks() {
    let mut vm = script::load(r#"
        var view = new Int32Array(new SharedArrayBuffer(4));
        var order = "";
        Atomics.waitAsync(view, 0, 0).value.then(value => { order += value; });
        Atomics.notify(view, 0);
        Promise.resolve().then(() => { order += "!"; });
    "#).unwrap();
    vm.realm.install_test262_shared_memory(true);
    vm.run_main(0).unwrap();
    vm.drain_microtasks(&mut |_, _| {}).unwrap();
    assert_eq!(vm.global_text("order"), "ok!");
}
