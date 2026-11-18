//! Threads: how many for each kind of work (the CPU use level), work split over them
//! (`in_parts`), a background thread's answer (`Receiving`), and work counted while it runs
//! (`Working`) and until the window has handled its answer (`AnswerSender`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, mpsc};

/// The CPU use level (index in CPU_LEVELS), for the worker threads, and the signal that wakes
/// the thumbnail workers it leaves idle when it changes.
pub(crate) static CPU_USE_LEVEL: AtomicUsize = AtomicUsize::new(1);

pub(crate) static CPU_CHANGED: (Mutex<()>, std::sync::Condvar) =
    (Mutex::new(()), std::sync::Condvar::new());

/// Glimt runs with one malloc arena (`one_malloc_arena`): set at startup.
pub(crate) static HAS_ONE_MALLOC_ARENA: AtomicBool = AtomicBool::new(false);

/// How long a worker of a pool may take over one job before the pool gives it up and starts
/// another worker in its place (`spawn_worker_pool`). A read from a share that stopped
/// answering waits for ever, and a large image on a slow share is read within this.
pub(crate) const STUCK_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// The most workers of one pool that are given up and still stuck at the same time. A folder
/// where a worker is stuck costs no more, as its later jobs are put aside (`Stalled`).
/// shortcut: past this, a stuck worker is not replaced and the pool waits as it did before 0.82;
/// raise it when shares that stall for good meet more folders at once than this.
const MOST_STUCK: usize = 32;

/// In a test, the files at which work panics where it calls `panic_in_a_test_at`, as a file
/// that breaks a parser would. Each test names files of its own, so the tests that run beside
/// it never meet them.
#[cfg(test)]
pub(crate) static BREAKING_FILES: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

/// The processor clock of a thread, which another thread may read. A thread that waits for a
/// share that stopped answering uses no processor time, and one that is only slow (a large
/// decode, a read over a slow network) does. So it tells a stuck job from a slow one, where
/// the time a job has taken alone does not.
pub(crate) struct ThreadClock(libc::clockid_t);

impl ThreadClock {
    /// Returns the clock of the thread that calls it, or `None` when the system has none.
    pub(crate) fn of_this_thread() -> Option<ThreadClock> {
        let mut clock: libc::clockid_t = 0;
        #[allow(unsafe_code)]
        // SAFETY: `pthread_self` is always a valid thread, and `clock` is valid for the call
        // to fill.
        let failed = unsafe { libc::pthread_getcpuclockid(libc::pthread_self(), &mut clock) };
        (failed == 0).then_some(ThreadClock(clock))
    }

    /// Returns the processor time its thread has used, in nanoseconds, or `None` when it
    /// cannot be read, such as after the thread ended.
    pub(crate) fn used(&self) -> Option<u64> {
        #[allow(unsafe_code)]
        // SAFETY: A timespec may be all zero, and `t` is valid for the call to fill.
        let (failed, t) = unsafe {
            let mut t: libc::timespec = std::mem::zeroed();
            (libc::clock_gettime(self.0, &mut t), t)
        };
        (failed == 0).then(|| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    }
}

/// What a background thread's receiver has: nothing yet, its answer, or the thread ended. For
/// `take` and `wait`, `Gone` means the thread ended without its answer (it panicked); for `next`
/// and `latest`, a stream that ended as it should gives `Gone` too. With `Gone` the receiver is
/// dropped, so nothing waits for it for ever.
pub(crate) enum Took<T> {
    Nothing,
    Answer(T),
    Gone,
}

/// A background thread's receiver, `none` while nothing runs. Its messages come only through
/// `take`, `next`, `latest` and `wait`, each of which tells once when the thread is gone
/// (`Took::Gone`) and forgets the receiver then, so no receiver polled by hand can leave the
/// window waiting for a thread that broke.
pub(crate) struct Receiving<T>(Option<mpsc::Receiver<T>>);

// By hand, as a derive would ask the answer itself for a default, which a `Working` has not.
impl<T> Default for Receiving<T> {
    fn default() -> Self {
        Receiving(None)
    }
}

impl<T> Receiving<T> {
    pub(crate) fn none() -> Self {
        Receiving(None)
    }

    pub(crate) fn of(rx: mpsc::Receiver<T>) -> Self {
        Receiving(Some(rx))
    }

    pub(crate) fn is_some(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn is_none(&self) -> bool {
        self.0.is_none()
    }

    /// Nothing is waited for any more: the thread's answer, if it comes, is dropped.
    pub(crate) fn clear(&mut self) {
        self.0 = None;
    }

    /// The one answer of a thread that sends one: the receiver goes with it.
    pub(crate) fn take(&mut self) -> Took<T> {
        let took = self.next();
        if matches!(took, Took::Answer(_)) {
            self.0 = None;
        }
        took
    }

    /// The next message of a thread that sends several; the receiver stays until `clear`, or
    /// the thread is gone.
    pub(crate) fn next(&mut self) -> Took<T> {
        match self.0.as_ref().map(mpsc::Receiver::try_recv) {
            None | Some(Err(mpsc::TryRecvError::Empty)) => Took::Nothing,
            Some(Ok(answer)) => Took::Answer(answer),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.0 = None;
                Took::Gone
            }
        }
    }

    /// The last of the messages queued, the earlier ones dropped: for a thread that sends the
    /// state so far again and again. The receiver stays, as `next`. When the thread ended after
    /// its last message, that message is given now and `Gone` at the next call, so the end is
    /// still told once.
    pub(crate) fn latest(&mut self) -> Took<T> {
        let mut last = Took::Nothing;
        loop {
            match self.0.as_ref().map(mpsc::Receiver::try_recv) {
                Some(Ok(answer)) => last = Took::Answer(answer),
                // Also when the thread is gone: the receiver is kept, so that the next call
                // finds it gone and tells.
                _ if matches!(last, Took::Answer(_)) => return last,
                _ => return self.next(),
            }
        }
    }

    /// As `take`, waiting up to `timeout` for the answer.
    pub(crate) fn wait(&mut self, timeout: std::time::Duration) -> Took<T> {
        let took = self.wait_next(timeout);
        if matches!(took, Took::Answer(_)) {
            self.0 = None;
        }
        took
    }

    /// As `next`, waiting up to `timeout` for the next message. Only a background thread waits
    /// so, for the messages of another, never the window's thread.
    pub(crate) fn wait_next(&mut self, timeout: std::time::Duration) -> Took<T> {
        match self.0.as_ref().map(|rx| rx.recv_timeout(timeout)) {
            None | Some(Err(mpsc::RecvTimeoutError::Timeout)) => Took::Nothing,
            Some(Ok(answer)) => Took::Answer(answer),
            Some(Err(mpsc::RecvTimeoutError::Disconnected)) => {
                self.0 = None;
                Took::Gone
            }
        }
    }
}

/// One piece of background work, for the busy pointer (`busy`) or for closing
/// (`file_io::WRITING`): counted while held, and no longer when dropped, also as its thread
/// panics, which a count by hand would leave on for good.
pub(crate) struct Working(Arc<AtomicUsize>);

impl Working {
    pub(crate) fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Working(count.clone())
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        // Release, so that a thread that reads the count as done with Acquire also sees what
        // the work told before it ended, such as a failed save (`is_writing`).
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// The sending half of a channel whose answers the window must handle before the work they end
/// is done: a folder's real path that a Copy waits for, a turn saved, a file operation's report.
/// Each answer carries a busy pointer of its own, made before the thread lets go of its own
/// pointer, and the window drops it after it has handled the answer. So the window never sees
/// no work running while an answer still waits in the channel. Before, a close in that moment
/// dropped a Copy that waited for its folder, and a test that waited for no work missed it.
pub(crate) struct AnswerSender<T> {
    tx: mpsc::Sender<(T, Working)>,
    busy: Arc<AtomicUsize>,
}

impl<T> Clone for AnswerSender<T> {
    fn clone(&self) -> Self {
        AnswerSender {
            tx: self.tx.clone(),
            busy: self.busy.clone(),
        }
    }
}

impl<T> AnswerSender<T> {
    /// Sends `answer` with a busy pointer of its own. Returns an error only when the window no
    /// longer waits for it, as `mpsc::Sender::send` does, and the pointer then goes at once.
    pub(crate) fn send(&self, answer: T) -> Result<(), mpsc::SendError<(T, Working)>> {
        self.tx.send((answer, Working::new(&self.busy)))
    }
}

/// When the worker in each place of a pool started the job it does, and the folder of the job's
/// file, `None` while it waits for a job. A worker given up leaves its place to a new worker
/// with a new `JobStart`, and ends once its job returns, when it finds that its place is no
/// longer its own.
type JobStart = Arc<Mutex<Option<(std::time::Instant, PathBuf)>>>;

/// What the workers of a pool share (`spawn_worker_pool`).
struct WorkerPool<J> {
    jobs: Mutex<mpsc::Receiver<J>>,
    /// The `JobStart` of the worker that has each place now.
    places: Mutex<Vec<JobStart>>,
    /// How many workers are given up and still stuck in their job.
    stuck: AtomicUsize,
    /// The folders where workers are stuck, and the jobs put aside there.
    stalled: Mutex<Stalled<J>>,
    /// Waits until the place, by its number, may take a job.
    wait_for_turn: Box<dyn Fn(usize) + Send + Sync>,
    work: Box<dyn Fn(J) + Send + Sync>,
}

/// The folders of the jobs of the workers given up and still stuck, once for each worker, and
/// the later jobs in those folders, put aside. A read in a folder of a share that stopped
/// answering waits for ever, as every later read there would, so a worker that took them would
/// be given up in turn. Before, a page of thumbnails of such a folder took a new place for
/// each file every few seconds, until `MOST_STUCK` were stuck, and no thumbnail was made
/// again, of any folder. A stuck worker whose job returns does the jobs put aside in its
/// folder before it ends.
struct Stalled<J> {
    folders: Vec<PathBuf>,
    jobs: Vec<J>,
}

/// A job of a worker pool, which is about one file. Its file's folder tells the pool which
/// jobs to put aside while a worker is stuck there (`Stalled`).
pub(crate) trait JobOfAFile {
    /// Returns the file the job is about.
    fn file(&self) -> &Path;

    /// Returns the folder of the job's file.
    fn folder(&self) -> PathBuf {
        self.file().parent().unwrap_or(Path::new("")).to_owned()
    }
}

impl JobOfAFile for PathBuf {
    fn file(&self) -> &Path {
        self
    }
}

impl JobOfAFile for (PathBuf, u64) {
    fn file(&self) -> &Path {
        &self.0
    }
}

/// What a background thread tells the window through a `NoticeSender`.
#[derive(Debug, PartialEq)]
pub(crate) enum Told {
    /// A text for the notice over the image.
    Notice(String),
    /// A write that failed. The window shows it, and a close that waited for the writing stays
    /// open, so the user sees it.
    FailedWrite(String),
}

/// The way a background thread tells the user something: the text becomes the notice over the
/// image. Sending also asks for a frame, because egui draws only when asked, and a notice sent
/// without one waited in the channel until the mouse moved.
#[derive(Clone)]
pub(crate) struct NoticeSender {
    tx: mpsc::Sender<Told>,
    ctx: eframe::egui::Context,
}

impl NoticeSender {
    /// Returns the sending half for the window's `ctx`, and the half the window reads.
    pub(crate) fn new(ctx: &eframe::egui::Context) -> (Self, mpsc::Receiver<Told>) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        (NoticeSender { tx, ctx }, rx)
    }

    /// Sends `text` to the window and asks it to draw a frame. Returns an error only when the
    /// window is gone, as `mpsc::Sender::send` does.
    pub(crate) fn send(&self, text: String) -> Result<(), mpsc::SendError<Told>> {
        let sent = self.tx.send(Told::Notice(text));
        self.ctx.request_repaint();
        sent
    }

    /// Prints a failed write on stderr, and sends it to the window, which shows it and stays
    /// open when a close waited for the writing. It is printed here, so that it is told also
    /// when the window is gone.
    pub(crate) fn send_failed_write(&self, text: String) {
        eprintln!("{text}");
        // A window that is gone has nothing to show it in.
        let _ = self.tx.send(Told::FailedWrite(text));
        self.ctx.request_repaint();
    }
}

/// Returns a channel whose answers carry a busy pointer counted in `busy` (`AnswerSender`), and
/// the half the window reads. The window drops the pointer after it has handled the answer.
pub(crate) fn answer_channel<T>(
    busy: &Arc<AtomicUsize>,
) -> (AnswerSender<T>, mpsc::Receiver<(T, Working)>) {
    let (tx, rx) = mpsc::channel();
    let busy = busy.clone();
    (AnswerSender { tx, busy }, rx)
}

/// Starts `places` workers that take the jobs of `jobs` and do each with `work`, until the
/// sending half is gone. A worker waits with `wait_for_turn`, given the number of its place,
/// before it takes each job. A worker whose job takes longer than `stuck_after` (a read from a
/// share that stopped answering) is given up: another worker takes its place and the jobs after
/// it, and the stuck one ends when its job returns. Before, a few files on such a share held
/// every worker for good, and no other image or thumbnail was decoded until a restart.
pub(crate) fn spawn_worker_pool<J: JobOfAFile + Send + 'static>(
    places: usize,
    stuck_after: std::time::Duration,
    jobs: mpsc::Receiver<J>,
    wait_for_turn: impl Fn(usize) + Send + Sync + 'static,
    work: impl Fn(J) + Send + Sync + 'static,
) {
    let starts: Vec<JobStart> = (0..places).map(|_| JobStart::default()).collect();
    let pool = Arc::new(WorkerPool {
        jobs: Mutex::new(jobs),
        places: Mutex::new(starts.clone()),
        stuck: AtomicUsize::new(0),
        stalled: Mutex::new(Stalled {
            folders: Vec::new(),
            jobs: Vec::new(),
        }),
        wait_for_turn: Box::new(wait_for_turn),
        work: Box::new(work),
    });
    for (place, start) in starts.into_iter().enumerate() {
        start_worker(&pool, place, start);
    }
    let pool = Arc::downgrade(&pool);
    // No busy pointer: the watch over a pool's workers, which only starts workers. It ends
    // with the pool, once its workers have ended.
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(stuck_after / 4);
            let Some(pool) = pool.upgrade() else {
                return;
            };
            replace_stuck_workers(&pool, stuck_after);
        }
    });
}

/// Gives up each worker of `pool` whose job has taken longer than `stuck_after`, and starts a
/// new worker in its place, while fewer than `MOST_STUCK` are stuck. The later jobs in the
/// given-up worker's folder are put aside from then on (`Stalled`).
fn replace_stuck_workers<J: JobOfAFile + Send + 'static>(
    pool: &Arc<WorkerPool<J>>,
    stuck_after: std::time::Duration,
) {
    let mut places = lock_even_if_poisoned(&pool.places);
    for (place, start) in places.iter_mut().enumerate() {
        let stuck_in = (lock_even_if_poisoned(start).as_ref())
            .filter(|(started, _)| started.elapsed() >= stuck_after)
            .map(|(_, folder)| folder.clone());
        if let Some(folder) = stuck_in
            && pool.stuck.load(Ordering::Relaxed) < MOST_STUCK
        {
            pool.stuck.fetch_add(1, Ordering::Relaxed);
            lock_even_if_poisoned(&pool.stalled).folders.push(folder);
            *start = JobStart::default();
            start_worker(pool, place, start.clone());
        }
    }
}

/// Starts the worker of `pool` in place `place`, which notes when it starts each job in `start`.
/// A job in a folder where a worker is stuck is put aside (`Stalled`).
fn start_worker<J: JobOfAFile + Send + 'static>(
    pool: &Arc<WorkerPool<J>>,
    place: usize,
    start: JobStart,
) {
    let pool = pool.clone();
    // No busy pointer: a worker of a pool, which lasts as long as the window. Each answer is
    // shown when it comes.
    std::thread::spawn(move || {
        loop {
            (pool.wait_for_turn)(place);
            let Ok(job) = lock_even_if_poisoned(&pool.jobs).recv() else {
                return;
            };
            let folder = job.folder();
            let job = {
                let mut stalled = lock_even_if_poisoned(&pool.stalled);
                if stalled.folders.contains(&folder) {
                    stalled.jobs.push(job);
                    continue;
                }
                job
            };
            *lock_even_if_poisoned(&start) = Some((std::time::Instant::now(), folder.clone()));
            (pool.work)(job);
            *lock_even_if_poisoned(&start) = None;
            if !Arc::ptr_eq(&lock_even_if_poisoned(&pool.places)[place], &start) {
                // Its folder answers again, so it does the jobs put aside there.
                for job in jobs_put_aside_in(&pool, &folder) {
                    (pool.work)(job);
                }
                pool.stuck.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }
    });
}

/// Notes that a stuck worker's job in `folder` has returned. Returns the jobs put aside in
/// `folder`, once no other worker is stuck there, for that worker to do.
fn jobs_put_aside_in<J: JobOfAFile>(pool: &WorkerPool<J>, folder: &Path) -> Vec<J> {
    let mut stalled = lock_even_if_poisoned(&pool.stalled);
    if let Some(at) = stalled.folders.iter().position(|f| f == folder) {
        stalled.folders.swap_remove(at);
    }
    if stalled.folders.iter().any(|f| f == folder) {
        return Vec::new();
    }
    let (here, elsewhere) = std::mem::take(&mut stalled.jobs)
        .into_iter()
        .partition(|job| job.folder() == folder);
    stalled.jobs = elsewhere;
    here
}

pub(crate) fn set_cpu_use(level: usize) {
    let _lock = lock_even_if_poisoned(&CPU_CHANGED.0);
    CPU_USE_LEVEL.store(level, Ordering::Relaxed);
    CPU_CHANGED.1.notify_all();
}

pub(crate) fn cores() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get())
}

/// Threads for parallel work in the background: searches, the EXIF index, Find Duplicates and
/// pan view.
pub(crate) fn background_threads() -> usize {
    background_threads_at(CPU_USE_LEVEL.load(Ordering::Relaxed))
}

pub(crate) fn background_threads_at(level: usize) -> usize {
    match level {
        0 => cores().min(2),
        _ => cores(),
    }
}

/// Thumbnails made at once; Balanced keeps one with the low memory level (`decoders` 1), so
/// two full-size images are not decoded at once.
pub(crate) fn thumb_threads(decoders: usize) -> usize {
    thumb_threads_at(CPU_USE_LEVEL.load(Ordering::Relaxed), decoders)
}

pub(crate) fn thumb_threads_at(level: usize, decoders: usize) -> usize {
    match level {
        0 => 1,
        1 => decoders.min(2),
        _ => cores(),
    }
}

/// Threads for work that allocates a little for every file, such as reading EXIF for the
/// exif: index or pan view. With one malloc arena they wait for each other in the allocator:
/// 2,000 files took 206 ms on one thread, 128 ms on two and 551 ms on four (20 cores); with an
/// arena each, eight threads took 68 ms.
pub(crate) fn small_alloc_threads() -> usize {
    match HAS_ONE_MALLOC_ARENA.load(Ordering::Relaxed) {
        true => background_threads().min(2),
        false => background_threads(),
    }
}

/// `work` on `items` cut into one part for each of `threads`, run at once; the results in the
/// order of the parts. `work` gets a part and the place in `items` where it starts. A part that
/// panics panics the caller too: a list short of it would put results on the wrong items.
pub(crate) fn in_parts<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    work: impl Fn(&[T], usize) -> Vec<R> + Sync,
) -> Vec<R> {
    let chunk = items.len().div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = (items.chunks(chunk).enumerate())
            .map(|(c, part)| {
                let work = &work;
                s.spawn(move || work(part, c * chunk))
            })
            .collect();
        (parts.into_iter())
            .flat_map(|p| p.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// The processor time this thread has used, in microseconds: a frame's own work, which other
/// threads busy at the same moment (summing the sizes of 500,000 files) do not add to.
pub(crate) fn thread_cpu_us() -> u64 {
    #[allow(unsafe_code)]
    // SAFETY: A timespec may be all zero, and `t` is valid for the call to fill.
    let t = unsafe {
        let mut t: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t);
        t
    };
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
}

/// Returns `work` made ready for the thread that runs it: inside `catch_panic`, with what it
/// returns told to the window through `notices`. A panic is told as a failed write, because
/// the work may have stopped halfway, and the user must see that before a close. `held` (the
/// busy pointer, the count of writing) is dropped only after the telling, so a close that
/// waited for the work takes its failure first, and the window `ctx` is then asked for a frame,
/// so the busy pointer goes at once. This is the one way work that writes is run in a thread
/// (`source_rules::writing_threads_tell_what_they_did`). Before, a panic outside a catch ended
/// such a thread with nothing told: no report, no Undo, and a close went ahead.
pub(crate) fn told_work(
    notices: NoticeSender,
    ctx: &eframe::egui::Context,
    held: impl Send + 'static,
    work: impl FnOnce() -> Option<Told> + Send + 'static,
) -> impl FnOnce() + Send + 'static {
    let ctx = ctx.clone();
    move || {
        let broke = || {
            Some(Told::FailedWrite(
                crate::support::lang::tr(
                    "A change could not be saved, because Glimt met an error while writing it",
                )
                .to_owned(),
            ))
        };
        match catch_panic(work).unwrap_or_else(broke) {
            Some(Told::Notice(text)) => drop(notices.send(text)),
            Some(Told::FailedWrite(text)) => notices.send_failed_write(text),
            None => {}
        }
        drop(held);
        ctx.request_repaint();
    }
}

/// Panics when `path` is one of `BREAKING_FILES`, for a test of what a file that breaks the
/// work does to the files after it.
#[cfg(test)]
pub(crate) fn panic_in_a_test_at(path: &std::path::Path) {
    if lock_even_if_poisoned(&BREAKING_FILES)
        .iter()
        .any(|p| p == path)
    {
        panic!("a file that breaks the work");
    }
}

/// Runs `work` and returns its result, or `None` when it panics. A worker uses this around
/// each file it reads or writes, so that a file that breaks a decoder or a parser costs only
/// that file, not the worker thread and every file after it. This is the one place Glimt
/// catches a panic (`source_rules::panics_are_caught_in_one_place`).
pub(crate) fn catch_panic<T>(work: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).ok()
}

/// Locks `mutex` and returns its guard, also when a thread panicked while it held the lock.
/// A panic is caught where it happens (`catch_panic`), so what the mutex holds is still whole,
/// and the threads that share it go on. This is the one way Glimt locks a mutex (clippy.toml).
pub(crate) fn lock_even_if_poisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[allow(clippy::disallowed_methods)] // the one place that locks, recovering a poisoned lock
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Locks `lock` for reading and returns its guard, also when a thread panicked while it held
/// the lock, as `lock_even_if_poisoned` does for a mutex. This is the one way Glimt reads
/// through a `RwLock` (clippy.toml).
pub(crate) fn read_even_if_poisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    #[allow(clippy::disallowed_methods)] // the one place that reads, recovering a poisoned lock
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Locks `lock` for writing and returns its guard, also when a thread panicked while it held
/// the lock. A write that a poisoned lock refused dropped the user's own keys without a word.
/// This is the one way Glimt writes through a `RwLock` (clippy.toml).
pub(crate) fn write_even_if_poisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    #[allow(clippy::disallowed_methods)] // the one place that writes, recovering a poisoned lock
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    /// A `RwLock` that a caught panic poisoned is still written and read. The user's own keys
    /// were dropped without a word, as their write took only a lock that was not poisoned.
    #[test]
    fn a_poisoned_rwlock_is_still_written() {
        let lock = std::sync::RwLock::new(1);
        let poisoned = super::catch_panic(|| {
            let _held = super::write_even_if_poisoned(&lock);
            panic!("a broken file");
        });
        assert!(poisoned.is_none() && lock.is_poisoned());
        *super::write_even_if_poisoned(&lock) = 2;
        assert_eq!(*super::read_even_if_poisoned(&lock), 2);
    }

    /// A notice sent from a background thread asks the window for a frame, so it shows at
    /// once, not when the mouse next moves.
    #[test]
    fn a_notice_asks_for_a_frame() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ctx = eframe::egui::Context::default();
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        ctx.set_request_repaint_callback(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        let (notice_sender, rx) = super::NoticeSender::new(&ctx);
        std::thread::spawn(move || notice_sender.send("done".into()).unwrap())
            .join()
            .unwrap();
        assert_eq!(rx.try_recv(), Ok(super::Told::Notice("done".into())));
        assert!(asked.load(Ordering::Relaxed) > 0);
    }

    /// A part that panics is no shorter list of results, which the callers would zip with the
    /// wrong items, but a panic of the caller.
    #[test]
    fn in_parts_passes_a_panic_on() {
        let items: Vec<u32> = (0..100).collect();
        let got = std::panic::catch_unwind(|| {
            super::in_parts(&items, 4, |part, at| match at {
                25 => panic!("a part breaks"),
                _ => part.to_vec(),
            })
        });
        assert!(got.is_err());
        assert_eq!(super::in_parts(&items, 4, |part, _| part.to_vec()), items);
    }

    /// A thread's answer is taken once; a thread gone without one is told once, then nothing
    /// waits for it.
    #[test]
    fn a_thread_gone_is_noticed_once() {
        use super::Took;
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        let mut rx = super::Receiving::of(rx);
        assert!(matches!(rx.take(), Took::Nothing));
        tx.send(7).unwrap();
        assert!(matches!(rx.take(), Took::Answer(7)));
        assert!(rx.is_none());
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        let mut rx = super::Receiving::of(rx);
        drop(tx);
        assert!(matches!(rx.take(), Took::Gone));
        assert!(matches!(rx.take(), Took::Nothing));
        // A stream: the latest of what is queued, the receiver kept until the thread is gone.
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        let mut rx = super::Receiving::of(rx);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        assert!(matches!(rx.latest(), Took::Answer(2)) && rx.is_some());
        drop(tx);
        assert!(matches!(rx.latest(), Took::Gone) && rx.is_none());
        // A last message and then the end in the same moment: the message now, and the end at
        // the next call, told once.
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        let mut rx = super::Receiving::of(rx);
        tx.send(3).unwrap();
        drop(tx);
        assert!(matches!(rx.latest(), Took::Answer(3)));
        assert!(matches!(rx.latest(), Took::Gone) && rx.is_none());
        assert!(matches!(rx.latest(), Took::Nothing));
    }

    /// A worker stuck in a job (a read from a share that stopped answering) is given up, and
    /// the jobs after it are done by another worker in its place. The stuck one ends when its
    /// job returns, and its answer still comes. Before, the one worker held the pool for good.
    #[test]
    fn a_stuck_worker_is_replaced() {
        use std::path::PathBuf;
        use std::time::Duration;
        let (jobs, rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let work = move |job: PathBuf| {
            if job.ends_with("0") {
                let _ = super::lock_even_if_poisoned(&held).recv();
            }
            done_tx.send(job).unwrap();
        };
        super::spawn_worker_pool(1, Duration::from_millis(200), rx, |_| {}, work);
        for name in ["/a/0", "/b/1", "/c/2"] {
            jobs.send(PathBuf::from(name)).unwrap();
        }
        let five_seconds = Duration::from_secs(5);
        assert_eq!(
            done.recv_timeout(five_seconds),
            Ok(PathBuf::from("/b/1")),
            "a job waited behind a stuck one"
        );
        assert_eq!(done.recv_timeout(five_seconds), Ok(PathBuf::from("/c/2")));
        release.send(()).unwrap();
        assert_eq!(done.recv_timeout(five_seconds), Ok(PathBuf::from("/a/0")));
    }

    /// A worker stuck on a folder of a share that stopped answering costs its pool the places
    /// that were in that folder when it stalled, not one more for each of its files: the jobs
    /// after it in that folder are put aside until a stuck one returns, and the jobs elsewhere
    /// go on. Before, a page of thumbnails there took a new place for each file every few
    /// seconds, until `MOST_STUCK` were stuck, and no thumbnail was made again, of any folder.
    #[test]
    fn a_stalled_folder_holds_no_more_places() {
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;
        let (jobs, rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let (started, answers) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (started_in_work, answers_in_work) = (started.clone(), answers.clone());
        let work = move |job: PathBuf| {
            if job.starts_with("/stalled") {
                started_in_work.fetch_add(1, Ordering::SeqCst);
                while !answers_in_work.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            done_tx.send(job).unwrap();
        };
        super::spawn_worker_pool(2, Duration::from_millis(200), rx, |_| {}, work);
        for n in 0..40 {
            jobs.send(PathBuf::from(format!("/stalled/{n}.jpg")))
                .unwrap();
        }
        jobs.send(PathBuf::from("/local/a.jpg")).unwrap();
        let five_seconds = Duration::from_secs(5);
        let local = done.recv_timeout(five_seconds);
        std::thread::sleep(Duration::from_secs(1));
        let stuck = started.load(Ordering::SeqCst);
        answers.store(true, Ordering::SeqCst);
        let rest = (0..40)
            .filter(|_| done.recv_timeout(five_seconds).is_ok())
            .count();
        assert_eq!(local, Ok(PathBuf::from("/local/a.jpg")));
        assert_eq!(stuck, 2, "workers stuck in the stalled folder");
        assert_eq!(rest, 40, "the jobs put aside were not done");
    }

    /// CPU use: thumbnails and background threads for each level.
    #[test]
    fn cpu_levels() {
        use super::{background_threads_at, cores, thumb_threads_at};
        // By level, not the global one: a window made by a test beside it sets that.
        let at = |level| (thumb_threads_at(level, 4), background_threads_at(level));
        assert_eq!(at(0), (1, cores().min(2)));
        assert_eq!(at(1), (2, cores()));
        assert_eq!(at(2), (cores(), cores()));
        assert_eq!(thumb_threads_at(1, 1), 1);
    }

    /// The busy pointer goes when its work ends, also by a panic.
    #[test]
    fn busy_ends_with_a_panic() {
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let pointer = super::Working::new(&count);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let _ = std::thread::spawn(move || {
            let _pointer = pointer;
            panic!("a broken file");
        })
        .join();
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}
