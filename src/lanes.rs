use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};

pub type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Lanes {
    pub interactive: rayon::ThreadPool,
    pub background: rayon::ThreadPool,
    pub edits: LatestLane,
}

#[derive(Clone, Copy, Debug)]
pub struct LaneSizes {
    pub interactive: usize,
    pub background: usize,
}

impl LaneSizes {
    pub fn for_machine() -> LaneSizes {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        LaneSizes { interactive: (cores / 4).clamp(2, 4), background: cores.saturating_sub(1).max(1) }
    }
}

pub fn lower_current_thread_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

pub fn guarded(job: impl FnOnce()) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(job)) {
        let message = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_default();
        eprintln!("scratchpilerd: job panicked: {message}");
    }
}

impl Lanes {
    pub fn new(sizes: LaneSizes) -> Lanes {
        let interactive =
            rayon::ThreadPoolBuilder::new().num_threads(sizes.interactive).thread_name(|i| format!("sp-interactive-{i}")).build().expect("interactive pool");
        let background = rayon::ThreadPoolBuilder::new()
            .num_threads(sizes.background)
            .thread_name(|i| format!("sp-background-{i}"))
            .start_handler(|_| lower_current_thread_priority())
            .build()
            .expect("background pool");
        Lanes { interactive, background, edits: LatestLane::new("sp-edits") }
    }

    pub fn interactive(&self, job: impl FnOnce() + Send + 'static) {
        self.interactive.spawn(move || guarded(job));
    }

    pub fn background(&self, job: impl FnOnce() + Send + 'static) {
        self.background.spawn_fifo(move || guarded(job));
    }
}

struct Mailbox {
    pending: Vec<(String, Job)>,
    closed: bool,
}

pub struct LatestLane {
    state: Arc<(Mutex<Mailbox>, Condvar)>,
}

impl LatestLane {
    pub fn new(name: &str) -> LatestLane {
        let state = Arc::new((Mutex::new(Mailbox { pending: Vec::new(), closed: false }), Condvar::new()));
        let worker = state.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                loop {
                    let job = {
                        let (lock, ready) = &*worker;
                        let mut mailbox = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while mailbox.pending.is_empty() && !mailbox.closed {
                            mailbox = ready.wait(mailbox).unwrap_or_else(|e| e.into_inner());
                        }
                        if mailbox.pending.is_empty() {
                            return;
                        }
                        mailbox.pending.remove(0).1
                    };
                    guarded(job);
                }
            })
            .expect("edit lane");
        LatestLane { state }
    }

    pub fn submit(&self, key: String, job: Job) {
        let (lock, ready) = &*self.state;
        let mut mailbox = lock.lock().unwrap_or_else(|e| e.into_inner());
        match mailbox.pending.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = job,
            None => mailbox.pending.push((key, job)),
        }
        ready.notify_one();
    }
}

impl Drop for LatestLane {
    fn drop(&mut self) {
        let (lock, ready) = &*self.state;
        lock.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        ready.notify_all();
    }
}
