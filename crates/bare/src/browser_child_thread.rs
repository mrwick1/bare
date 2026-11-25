//! Dedicated workers with isolated heaps. Only structured messages cross the thread boundary.
use super::*;
use bare_js_api::clone::AgentCluster;
use bare_vex::shared_memory::Test262AgentControl;
use std::sync::{Mutex, atomic::{AtomicU8, Ordering::{Acquire, Release}}};
use std::thread::{self, Thread};
use std::time::Duration;

pub(super) struct Mailbox {
    hub: Arc<PortHub>,
    messages: Mutex<(usize, Vec<Out>)>,
    max_messages: usize,
    max_bytes: usize,
}

impl Mailbox {
    fn new(hub: Arc<PortHub>, max_messages: usize, max_bytes: usize) -> Self {
        Self { hub, messages: Mutex::default(), max_messages, max_bytes }
    }

    pub(super) fn push(&self, message: Out) {
        let mut messages = self.messages.lock().unwrap();
        let bytes = messages.0.saturating_add(16).saturating_add(message.cloned.cost());
        if bytes > self.max_bytes || messages.1.len() >= self.max_messages {
            drop(messages);
            self.hub.release(message.cloned.moved());
            return;
        }
        messages.0 = bytes;
        messages.1.push(message);
    }

    pub(super) fn len(&self) -> usize {
        self.messages.lock().unwrap().1.len()
    }

    fn take(&self) -> Vec<Out> {
        std::mem::take(&mut *self.messages.lock().unwrap()).1
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        for message in self.messages.get_mut().unwrap().1.drain(..) {
            self.hub.release(message.cloned.moved());
        }
    }
}

pub(super) struct ThreadWorker {
    control: Test262AgentControl,
    thread: Thread,
    inbox: Arc<Mailbox>,
    outbox: Arc<Mailbox>,
    finished: Arc<AtomicU8>,
}

impl ThreadWorker {
    pub(super) fn start<E: Eng>(cfg: &Cfg, dom: &Dom<E>, url: &str, of: Of<E::Obj>, from: &str) -> Result<Self, String> {
        let Of::Worker(_, module) = of else { return Err("the child is not a worker".into()) };
        let cluster = dom.agent_cluster.get().expect("isolated worker").clone();
        let (url, html, inline) = worker_doc(url, module, from)?;
        let mut cfg = cfg.fork();
        if cfg.client.has_embedder_policy() && inline.is_none() {
            let entry = Url::parse(&url).map_err(|_| "the worker entry URL is invalid")?;
            cfg.client.require_isolated_worker_entry(entry);
        }
        let max_messages = dom.max_messages();
        let max_bytes = dom.sched_max_message_bytes();
        cfg.hub.enable_thread_wake();
        let control = Test262AgentControl::default();
        let inbox = Arc::new(Mailbox::new(cfg.hub.clone(), max_messages, max_bytes));
        let outbox = Arc::new(Mailbox::new(cfg.hub.clone(), max_messages, max_bytes));
        let finished = Arc::new(AtomicU8::new(0));
        let input = Arc::downgrade(&inbox);
        let output = outbox.clone();
        let status = finished.clone();
        let stop = control.clone();
        let worker = thread::Builder::new().name("bare-worker".into()).stack_size(16 << 20).spawn(move || {
            struct Done(Arc<AtomicU8>, bool);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.store(if self.1 { 1 } else { 2 }, Release);
                    bare_net::parallel::wake();
                }
            }
            let mut done = Done(status, false);
            done.1 = run_worker::<E>(cfg, url, html, inline, cluster, stop, input, output);
        }).map_err(|e| e.to_string())?;
        Ok(Self { control, thread: worker.thread().clone(), inbox, outbox, finished })
    }

    pub(super) fn post(&self, message: Out) {
        self.inbox.push(message);
        self.thread.unpark();
    }

    pub(super) fn take(&self) -> Vec<Out> {
        self.outbox.take()
    }

    pub(super) fn finished(&self) -> Option<bool> {
        match self.finished.load(Acquire) {
            0 => None,
            1 => Some(false),
            _ => Some(true),
        }
    }

    #[cfg(test)]
    pub(super) fn status_handle(&self) -> Arc<AtomicU8> {
        self.finished.clone()
    }

    pub(super) fn ready(&self) -> bool {
        self.finished().is_some() || self.outbox.len() != 0
    }
}

impl Drop for ThreadWorker {
    fn drop(&mut self) {
        self.control.stop();
        self.thread.unpark();
    }
}

fn run_worker<E: Eng>(
    mut cfg: Cfg,
    url: String,
    html: String,
    inline: Option<String>,
    cluster: AgentCluster,
    control: Test262AgentControl,
    inbox: std::sync::Weak<Mailbox>,
    output: Arc<Mailbox>,
) -> bool {
    let Ok(mut browser) = Browser::<E>::make(tree::parse(&html), cfg.browser, cfg.env.eval, cfg.env.heap) else { return false };
    if browser.set_document_url(&url).is_err() {
        return false;
    }
    if control.is_stopped() { return true; }
    browser.it.start_shared_worker(control.clone());
    let dom = browser.dom.clone();
    if dom.agent_cluster.set(cluster).is_err() || dom.kids.worker_parent.set(output).is_err() {
        return false;
    }
    if browser.it.host_scope_o(|it| crate::browser_worker::install_scope(&dom, it)).is_err() {
        return false;
    }
    cfg.depth += 1;
    dom.kids.page.set(cfg.hub.new_page());
    *dom.kids.cfg.borrow_mut() = Some(Rc::new(cfg.fork()));
    dom.kids.poll.set(true);
    let current = thread::current();
    let _wake = bare_net::parallel::on_end(move || current.unpark());
    let was_inline = inline.is_some();
    if let Some(text) = inline {
        if browser.configure_fetch(crate::browser_fetch::FetchConfig {
            client: cfg.client.fork(), base_url: Some(url.clone()), limits: cfg.fetch,
        }).is_err() || browser.run_script(&text).is_err() {
            return false;
        }
    }
    let mut result = browser.run_page(PageConfig {
        url, client: cfg.client.fork(), limits: cfg.page.clone(), fetch: Some((cfg.client.fork(), cfg.fetch)),
    });
    let mut script_ran = was_inline || result.scripts_run > 0;
    loop {
        if control.is_stopped() || dom.kids.closed.get() { return true; }
        if !matches!(result.stop, Stop::Settled) { return false; }
        let Some(input) = inbox.upgrade() else { return true };
        let messages = input.take();
        drop(input);
        let has_messages = !messages.is_empty();
        for m in messages {
            let _ = browser.it.host_scope_o(|it| dom.deliver(it, m, None));
        }
        let due = browser.next_scheduled_due();
        if !has_messages && !dom.kids.hub_busy() && browser.live_work_of(true) != Some(true) {
            if !script_ran && browser.loaded() { return false; }
            match due {
                Some(t) => thread::park_timeout(Duration::from_secs_f64(((t - browser.now()) / 1000.0).clamp(0.0, 60.0))),
                None => thread::park(),
            }
            if control.is_stopped() { return true; }
        }
        result = browser.resume_page(cfg.client.fork(), cfg.page.clone(), &[]);
        script_ran |= result.scripts_run > 0;
    }
}
