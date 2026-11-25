use bare_vex::builtins::{NativeFn, arg};
use bare_vex::interp_rt::{R, Realm};
use bare_vex::shared_memory::{SharedBuffer, Test262AgentControl};
use bare_vex::value::Value;
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone)]
enum Data {
    Int(i32),
    Big(String),
}

struct Broadcast {
    buffer: SharedBuffer,
    value: Data,
    ack: SyncSender<()>,
}

struct Worker {
    inbox: Sender<Broadcast>,
    thread: JoinHandle<()>,
    control: Test262AgentControl,
}

struct Agent {
    report: Sender<String>,
    reports: Option<Receiver<String>>,
    inbox: Option<Receiver<Broadcast>>,
    workers: Vec<Worker>,
    clock: Instant,
}

thread_local! {
    static AGENT: RefCell<Option<Agent>> = const { RefCell::new(None) };
}

pub(super) struct Session;

impl Session {
    pub(super) fn begin() -> Self {
        let (report, reports) = mpsc::channel();
        AGENT.set(Some(Agent { report, reports: Some(reports), inbox: None, workers: Vec::new(), clock: Instant::now() }));
        Self
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(agent) = AGENT.take() {
            for worker in &agent.workers { worker.control.stop(); }
            for Worker { inbox, thread, .. } in agent.workers {
                drop(inbox);
                let _ = thread.join();
            }
        }
    }
}

pub(super) fn install(rt: &mut Realm, worker: bool, can_block: bool) -> R<()> {
    rt.install_test262_shared_memory(can_block);
    let methods: &[(&str, u8, NativeFn)] = if worker {
        &[
            ("receiveBroadcast", 1, receive), ("report", 1, report),
            ("leaving", 0, leaving),
        ]
    } else {
        &[("start", 1, start), ("broadcast", 2, broadcast), ("getReport", 0, get_report)]
    };
    for &(name, len, callback) in methods {
        rt.install_test262_agent_method(name, len, callback)?;
    }
    rt.install_test262_agent_method("sleep", 1, sleep)?;
    rt.install_test262_agent_method("monotonicNow", 0, monotonic_now)
}

fn start(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let source = rt.to_str(arg(args, 0))?;
    let source = rt.text(source);
    let (report, clock) = AGENT.with_borrow(|a| {
        let a = a.as_ref().expect("test agent");
        (a.report.clone(), a.clock)
    });
    let (send, inbox) = mpsc::channel();
    let (ready, started) = mpsc::sync_channel(0);
    let control = Test262AgentControl::default();
    let worker_control = control.clone();
    let thread = thread::Builder::new().name("test262-agent".into()).stack_size(16 << 20).spawn(move || {
        AGENT.set(Some(Agent { report, reports: None, inbox: Some(inbox), workers: Vec::new(), clock }));
        let _session = Session;
        let host = include_str!("../test262_host.inc.js");
        let Ok(mut vm) = bare_vex::script::load_with(host, &source) else {
            let _ = ready.send(false);
            return;
        };
        if install(&mut vm.realm, true, true).and_then(|()| vm.realm.install_test262_realms(host)).is_err() {
            let _ = ready.send(false);
            return;
        }
        vm.realm.set_test262_agent_control(worker_control);
        if ready.send(true).is_err() { return; }
        vm.fuel = 20_000_000;
        if vm.run_main(0).is_ok() {
            let _ = super::drain_agent(&mut vm);
        }
    });
    let Ok(thread) = thread else { return rt.type_error("cannot start the Test262 agent") };
    if started.recv() != Ok(true) {
        let _ = thread.join();
        return rt.type_error("Test262 agent failed to start");
    }
    AGENT.with_borrow_mut(|a| a.as_mut().unwrap().workers.push(Worker { inbox: send, thread, control }));
    Ok(Value::UNDEFINED)
}

fn broadcast(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let Some(buffer) = rt.exported_shared_buffer(arg(args, 0)) else { return rt.type_error("broadcast requires a SharedArrayBuffer") };
    let value = arg(args, 1);
    let value = if value.is_bigint() {
        let s = rt.to_str(value)?;
        Data::Big(rt.text(s))
    } else {
        Data::Int(Realm::to_int32(rt.to_number(value)?))
    };
    let workers: Vec<_> = AGENT.with_borrow(|a| a.as_ref().unwrap().workers.iter().map(|w| w.inbox.clone()).collect());
    let (ack, received) = mpsc::sync_channel(workers.len());
    for worker in &workers {
        if worker.send(Broadcast { buffer: buffer.clone(), value: value.clone(), ack: ack.clone() }).is_err() {
            return rt.type_error("Test262 agent stopped before broadcast");
        }
    }
    drop(ack);
    for _ in workers {
        if received.recv_timeout(Duration::from_secs(10)).is_err() {
            return rt.type_error("Test262 agent did not receive broadcast");
        }
    }
    Ok(Value::UNDEFINED)
}

fn receive(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let message = AGENT.with_borrow(|a| a.as_ref().unwrap().inbox.as_ref().unwrap().recv_timeout(Duration::from_secs(10)));
    let Ok(Broadcast { buffer, value, ack }) = message else { return rt.type_error("Test262 agent did not get a broadcast") };
    let _ = ack.send(());
    let buffer = rt.import_shared_buffer(buffer)?;
    let value = match value {
        Data::Int(i) => Value::int(i),
        Data::Big(s) => {
            let text = rt.string(&s);
            rt.to_bigint(text)?
        }
    };
    rt.call_function(arg(args, 0), Value::UNDEFINED, &[buffer, value])?;
    Ok(Value::UNDEFINED)
}

fn report(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let text = rt.to_str(arg(args, 0))?;
    let _ = AGENT.with_borrow(|a| a.as_ref().unwrap().report.send(rt.text(text)));
    Ok(Value::UNDEFINED)
}

fn get_report(rt: &mut Realm, _: Value, _: &[Value]) -> R<Value> {
    let report = AGENT.with_borrow(|a| a.as_ref().unwrap().reports.as_ref().unwrap().try_recv().ok());
    Ok(report.map_or(Value::NULL, |text| rt.string(&text)))
}

fn leaving(_: &mut Realm, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::UNDEFINED)
}

fn sleep(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let millis = rt.to_number(arg(args, 0))?;
    if millis.is_finite() && millis > 0.0 {
        thread::sleep(Duration::from_secs_f64(millis.min(10_000.0) / 1000.0));
    }
    Ok(Value::UNDEFINED)
}

fn monotonic_now(_: &mut Realm, _: Value, _: &[Value]) -> R<Value> {
    Ok(Realm::num(AGENT.with_borrow(|a| a.as_ref().unwrap().clock.elapsed().as_secs_f64()) * 1000.0))
}
