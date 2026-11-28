//! Threads: starting one (`spawn`, which says what it does for the busy pointer), how many for
//! each kind of work (the CPU use level), work split over them (`in_parts`), a background
//! thread's answer (`Receiving`), and work counted while it runs
//! (`Working`) and until the window has handled its answer (`AnswerSender`).

use crate::viewer::view::Failure;
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

/// How long a worker of a pool may go without progress in one job before the pool gives it up
/// and starts another worker in its place (`spawn_worker_pool`). A read from a share that
/// stopped answering waits for ever and uses no processor time (`Progress`).
pub(crate) const STUCK_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// The most workers of one pool that are given up and still stuck at the same time.
/// shortcut: past this, a stuck worker is not replaced and the pool waits as it did before 0.82;
/// raise it when a share that stalls for good meets more files at once than this.
const MOST_STUCK: usize = 32;

/// In a test, the files at which work panics where it calls `panic_in_a_test_at`, as a file
/// that breaks a parser would. Each test names files of its own, so the tests that run beside
/// it never meet them.
#[cfg(test)]
pub(crate) static BREAKING_FILES: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

/// In a test, the files whose work waits where it calls `wait_in_a_test_while_held`, until the
/// test takes them out, as a write to a slow share would. Each test names files of its own.
#[cfg(test)]
pub(crate) static HELD_FILES: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

thread_local! {
    /// Whether this thread waits for room in the decode budget (`WaitingForRoom`). The
    /// `Progress` of its job, which another thread reads, holds it too.
    static WAITING_FOR_ROOM: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
}

/// The processor clock of a thread, which another thread may read (`Progress`).
struct ThreadClock(libc::clockid_t);

impl ThreadClock {
    /// Returns the clock of the thread that calls it, or `None` when the system has none.
    fn of_this_thread() -> Option<ThreadClock> {
        let mut clock: libc::clockid_t = 0;
        #[allow(unsafe_code)]
        // SAFETY: `pthread_self` is always a valid thread, and `clock` is valid for the call
        // to fill.
        let failed = unsafe { libc::pthread_getcpuclockid(libc::pthread_self(), &mut clock) };
        (failed == 0).then_some(ThreadClock(clock))
    }

    /// Returns the processor time its thread has used, in nanoseconds, or `None` when it
    /// cannot be read, such as after the thread ended.
    fn used(&self) -> Option<u64> {
        #[allow(unsafe_code)]
        // SAFETY: A timespec may be all zero, and `t` is valid for the call to fill.
        let (failed, t) = unsafe {
            let mut t: libc::timespec = std::mem::zeroed();
            (libc::clock_gettime(self.0, &mut t), t)
        };
        (failed == 0).then(|| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    }
}

/// Whether the job of a thread still makes progress, told by the processor time its thread
/// uses. A thread that waits for a share that stopped answering uses none, and one that is
/// only slow in its own work (a large decode) does. So it tells a stuck job from a slow one,
/// where the time a job has taken alone does not. A read over a slow network or FUSE share
/// spends its time asleep in the system and may use none either, so it is taken to be stuck as
/// before. A thread that only wakes now and then to look whether it may go on uses less than
/// `PROGRESS`, which is not counted. A thread that waits for room in the decode budget
/// (`WaitingForRoom`) waits for another job, not for the disk, and so counts as making
/// progress.
pub(crate) struct Progress {
    clock: Option<ThreadClock>,
    /// Whether the thread waits for room in the decode budget now (`WAITING_FOR_ROOM`).
    is_waiting_for_room: Arc<AtomicBool>,
    /// The processor time the thread had used at `since`, in nanoseconds.
    used: u64,
    /// When the job started, or last made progress.
    since: std::time::Instant,
    /// When the job started, or last waited for room in the decode budget. The ceiling counts
    /// from here, so that a long wait for another job does not use up the time of its own work.
    started: std::time::Instant,
}

impl Progress {
    /// The processor time, in nanoseconds, that counts as progress.
    const PROGRESS: u64 = 1_000_000;

    /// How many times the time without progress that makes a job stuck a job may run in all,
    /// with progress or not. A decoder that a crafted file sent into an endless loop uses the
    /// processor for ever, and without this ceiling it would keep its worker and its bytes until
    /// a restart.
    pub(crate) const CEILING: u32 = 12;

    /// Returns the progress of the job that the calling thread starts now.
    pub(crate) fn of_this_thread() -> Progress {
        let clock = ThreadClock::of_this_thread();
        Progress {
            used: clock.as_ref().and_then(ThreadClock::used).unwrap_or(0),
            clock,
            is_waiting_for_room: WAITING_FOR_ROOM.with(Arc::clone),
            since: std::time::Instant::now(),
            started: std::time::Instant::now(),
        }
    }

    /// Returns whether the job is stuck: it has made no progress for `after`, or has run for
    /// `CEILING` times as long, unless it waits for room in the decode budget now.
    pub(crate) fn is_stuck(&mut self, after: std::time::Duration) -> bool {
        self.idle_for() >= after
            || (!self.is_waiting_for_room.load(Ordering::Relaxed)
                && self.started.elapsed() >= after * Self::CEILING)
    }

    /// Returns how long the job has made no progress. Without a clock to read, that is how
    /// long it has run.
    fn idle_for(&mut self) -> std::time::Duration {
        if self.is_waiting_for_room.load(Ordering::Relaxed) {
            self.since = std::time::Instant::now();
            self.started = self.since;
        } else if let Some(used) = self.clock.as_ref().and_then(ThreadClock::used)
            && used >= self.used + Self::PROGRESS
        {
            self.used = used;
            self.since = std::time::Instant::now();
        }
        self.since.elapsed()
    }
}

/// The mark that the calling thread waits for room in the decode budget, while it lives
/// (`Progress`).
pub(crate) struct WaitingForRoom;

impl WaitingForRoom {
    pub(crate) fn start() -> WaitingForRoom {
        WAITING_FOR_ROOM.with(|waits| waits.store(true, Ordering::Relaxed));
        WaitingForRoom
    }
}

impl Drop for WaitingForRoom {
    fn drop(&mut self) {
        WAITING_FOR_ROOM.with(|waits| waits.store(false, Ordering::Relaxed));
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
/// window waiting for a thread that broke. Beside the receiver it holds a mark that lives as
/// long as the receiver, by which the thread can ask whether its answer is still waited for
/// (`is_waited_for`).
pub(crate) struct Receiving<T>(Option<(mpsc::Receiver<T>, Arc<()>)>);

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
        Receiving(Some((rx, Arc::new(()))))
    }

    /// Returns, for the thread that answers, whether this still waits for its answer. It stops
    /// waiting when it is cleared, replaced or dropped, or once it has taken the answer. So the
    /// work of a thread whose answer nobody waits for any more, such as the sum of a selection
    /// changed since, can stop before it reads more files on a share.
    pub(crate) fn is_waited_for(&self) -> impl Fn() -> bool + Send + 'static {
        let mark = (self.0.as_ref()).map_or_else(std::sync::Weak::new, |(_, m)| Arc::downgrade(m));
        move || mark.strong_count() > 0
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
        match self.0.as_ref().map(|(rx, _)| rx.try_recv()) {
            None | Some(Err(mpsc::TryRecvError::Empty)) => Took::Nothing,
            Some(Ok(answer)) => Took::Answer(answer),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.0 = None;
                Took::Gone
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
        match self.0.as_ref().map(|(rx, _)| rx.recv_timeout(timeout)) {
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

/// What a background thread does for the busy pointer, which every thread says when it is
/// started (`spawn`). `--remote … --wait` and the tests answer when the busy count is down, so
/// work the user waits for without a pointer let them answer before it was done.
pub(crate) enum Busy {
    /// The thread holds the busy pointer until its work has ended, and then asks the window
    /// for a frame, so that the pointer goes at once.
    Pointer(Working, eframe::egui::Context),
    /// The thread's busy pointer is shown in a way of its own, which the text names: the
    /// answer it sends through an `AnswerSender`, or a state of the window that waits for it.
    ShownBy(&'static str),
    /// The thread shows no busy pointer, for the reason the text gives: nobody waits for it,
    /// or the thread that started it waits for it itself.
    NoPointer(&'static str),
}

/// The progress of the job that the worker in each place of a pool does, with the folder its
/// job is set aside by (`spawn_worker_pool`), `None` while it waits for a job. A worker given up
/// leaves its place to a new worker with a new `JobStart`, and ends once its job returns, when
/// it finds that its place is no longer its own.
type JobStart = Arc<Mutex<Option<(Progress, Option<PathBuf>)>>>;

/// What gives the folder a job of a pool is set aside by, and the file the job is for
/// (`WorkerPool::set_aside_by`).
type SetAsideBy<J> = Box<dyn Fn(&J) -> Option<(PathBuf, PathBuf)> + Send + Sync>;

/// The folders of a pool where a given-up worker is still stuck, and the jobs set aside until
/// that folder answers again.
struct Stalled<J> {
    /// The folder of each given-up worker's job, once for each worker.
    folders: Vec<PathBuf>,
    /// The jobs of a stalled folder, each with that folder and the file it is for, in the
    /// order they came.
    set_aside: Vec<(PathBuf, PathBuf, J)>,
    /// The place in `set_aside` of the job for each file.
    set_aside_files: std::collections::HashMap<PathBuf, usize>,
    /// The jobs set aside whose folder answered again, which the workers take before new ones.
    ready: std::collections::VecDeque<J>,
}

impl<J> Stalled<J> {
    /// Sets `job` for `file` aside by its stalled `folder`. A job set aside for the same file
    /// before gives its place to this newer one. Before, each time the user came back to the
    /// folder while its share was down, its thumbnails were set aside again, all of them.
    fn set_aside(&mut self, folder: PathBuf, file: PathBuf, job: J) {
        match self.set_aside_files.get(&file) {
            Some(&at) => self.set_aside[at] = (folder, file, job),
            None => {
                self.set_aside_files
                    .insert(file.clone(), self.set_aside.len());
                self.set_aside.push((folder, file, job));
            }
        }
    }

    /// Makes the jobs set aside by `folder` ready, in the order they came. Returns how many.
    fn let_go(&mut self, folder: &Path) -> usize {
        let (back, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.set_aside)
            .into_iter()
            .partition(|(f, _, _)| f == folder);
        self.set_aside = still;
        self.set_aside_files = (self.set_aside.iter().enumerate())
            .map(|(at, (_, file, _))| (file.clone(), at))
            .collect();
        let now_ready = back.len();
        self.ready.extend(back.into_iter().map(|(_, _, job)| job));
        now_ready
    }
}

/// What a worker of a pool waits for (`next_job`).
enum Next<J> {
    /// A new job.
    Job(J),
    /// A job set aside is ready, sent once for each job, so a worker that waits takes it.
    Ready,
    /// The pool's jobs have ended. Each worker that takes it sends it on to the next one.
    End,
}

/// What the workers of a pool share (`spawn_worker_pool`).
struct WorkerPool<J> {
    /// The new jobs, and the word that a job set aside is ready. The workers wait here
    /// without waking, also while nothing is set aside, so an idle pool costs nothing.
    jobs: Mutex<mpsc::Receiver<Next<J>>>,
    /// The sending half of `jobs`.
    wake: mpsc::Sender<Next<J>>,
    /// The `JobStart` of the worker that has each place now.
    places: Mutex<Vec<JobStart>>,
    /// How many workers are given up and still stuck in their job.
    stuck: AtomicUsize,
    /// Waits until the place, by its number, may take a job.
    wait_for_turn: Box<dyn Fn(usize) + Send + Sync>,
    /// The folder a job is set aside by while a worker is stuck in it, or `None` for a job that
    /// is never set aside.
    set_aside_by: SetAsideBy<J>,
    stalled: Mutex<Stalled<J>>,
    work: Box<dyn Fn(J) + Send + Sync>,
    /// How often a worker woke from its wait for a job, which a test counts.
    #[cfg(test)]
    wakeups: AtomicUsize,
}

/// The note in its `JobStart` that a worker of a pool does a job, made when the job starts
/// and taken away when it is dropped, also as a panic that escaped the job ends the worker.
/// A note left behind would read a thread clock of a thread that is gone, which the system may
/// give to another thread, so that its processor time counted as the job's progress and the
/// place was never filled again. A worker that a panic ends starts another in its place, or,
/// when it was given up, lets go of its count of the stuck. A worker that was given up lets go
/// of its job's folder on every way out, so the jobs set aside there are done.
struct JobNote<J: Send + 'static> {
    pool: Arc<WorkerPool<J>>,
    place: usize,
    start: JobStart,
    folder: Option<PathBuf>,
}

impl<J: Send + 'static> JobNote<J> {
    fn new(
        pool: &Arc<WorkerPool<J>>,
        place: usize,
        start: &JobStart,
        folder: Option<PathBuf>,
    ) -> JobNote<J> {
        *lock_even_if_poisoned(start) = Some((Progress::of_this_thread(), folder.clone()));
        JobNote {
            pool: pool.clone(),
            place,
            start: start.clone(),
            folder,
        }
    }
}

impl<J: Send + 'static> Drop for JobNote<J> {
    fn drop(&mut self) {
        *lock_even_if_poisoned(&self.start) = None;
        let is_given_up = !is_place_of(&self.pool, self.place, &self.start);
        let mut now_ready = 0;
        if is_given_up && let Some(folder) = &self.folder {
            let mut stalled = lock_even_if_poisoned(&self.pool.stalled);
            if let Some(i) = stalled.folders.iter().position(|f| f == folder) {
                stalled.folders.swap_remove(i);
            }
            if !stalled.folders.contains(folder) {
                now_ready = stalled.let_go(folder);
            }
        }
        for _ in 0..now_ready {
            // A pool whose workers have all ended has no one to wake.
            let _ = self.pool.wake.send(Next::Ready);
        }
        if std::thread::panicking() {
            if is_given_up {
                self.pool.stuck.fetch_sub(1, Ordering::Relaxed);
            } else {
                start_worker(&self.pool, self.place, self.start.clone());
            }
        }
    }
}

/// What a background thread tells the window through a `NoticeSender`.
#[derive(Debug, PartialEq)]
pub(crate) enum Told {
    /// A text for the notice over the image.
    Notice(crate::support::format::ShownText),
    /// A text for the notice over the image that tells of something that went wrong, other
    /// than a failed write, such as a program that could not be started.
    Error(Failure),
    /// A write that failed. The window shows it, and a close that waited for the writing stays
    /// open, so the user sees it.
    FailedWrite(Failure),
    /// Several of these, each told as itself, such as a job's own text and the failed write of
    /// its log.
    Several(Vec<Told>),
}

impl Told {
    /// Prints each failed write of this on stderr.
    fn print_failed_writes(&self) {
        match self {
            Told::FailedWrite(failure) => failure.print(),
            Told::Several(all) => all.iter().for_each(Told::print_failed_writes),
            Told::Notice(_) | Told::Error(_) => {}
        }
    }
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
    pub(crate) fn send(
        &self,
        text: crate::support::format::ShownText,
    ) -> Result<(), mpsc::SendError<Told>> {
        let sent = self.tx.send(Told::Notice(text));
        self.ctx.request_repaint();
        sent
    }

    /// Sends `failure` as `send` does, as an error, which the window shows as one.
    pub(crate) fn send_error(
        &self,
        failure: impl Into<Failure>,
    ) -> Result<(), mpsc::SendError<Told>> {
        let sent = self.tx.send(Told::Error(failure.into()));
        self.ctx.request_repaint();
        sent
    }

    /// Sends `told` to the window, whole, which shows the texts and errors of a
    /// `Told::Several` together. Its failed writes are printed here, as `send_failed_write`
    /// does, so that they are told also when the window is gone.
    pub(crate) fn tell(&self, told: Told) {
        told.print_failed_writes();
        // A window that is gone has nothing to show it in.
        let _ = self.tx.send(told);
        self.ctx.request_repaint();
    }

    /// Prints a failed write on stderr, and sends it to the window, which shows it and stays
    /// open when a close waited for the writing. It is printed here, so that it is told also
    /// when the window is gone.
    pub(crate) fn send_failed_write(&self, failure: impl Into<Failure>) {
        let failure = failure.into();
        failure.print();
        // A window that is gone has nothing to show it in.
        let _ = self.tx.send(Told::FailedWrite(failure));
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
/// While a given-up worker is stuck, the later jobs that `set_aside_by` gives the same folder
/// are set aside, without a look at the disk, and done once that worker's job returns. Before,
/// a page of thumbnails on a share that stopped answering gave up one worker after another,
/// until `MOST_STUCK` were stuck and no other folder's thumbnail was made (#154).
pub(crate) fn spawn_worker_pool<J: Send + 'static>(
    places: usize,
    stuck_after: std::time::Duration,
    jobs: mpsc::Receiver<J>,
    wait_for_turn: impl Fn(usize) + Send + Sync + 'static,
    set_aside_by: impl Fn(&J) -> Option<(PathBuf, PathBuf)> + Send + Sync + 'static,
    work: impl Fn(J) + Send + Sync + 'static,
) {
    start_pool(places, stuck_after, jobs, wait_for_turn, set_aside_by, work);
}

/// Starts a pool as `spawn_worker_pool` does. Returns the pool, which a test looks into.
fn start_pool<J: Send + 'static>(
    places: usize,
    stuck_after: std::time::Duration,
    jobs: mpsc::Receiver<J>,
    wait_for_turn: impl Fn(usize) + Send + Sync + 'static,
    set_aside_by: impl Fn(&J) -> Option<(PathBuf, PathBuf)> + Send + Sync + 'static,
    work: impl Fn(J) + Send + Sync + 'static,
) -> Arc<WorkerPool<J>> {
    let starts: Vec<JobStart> = (0..places).map(|_| JobStart::default()).collect();
    let (wake, next) = mpsc::channel();
    let handed_on = wake.clone();
    // It ends with the sending half of `jobs`.
    spawn(
        Busy::NoPointer(
            "it only hands the pool's jobs on to its workers, through the channel that also \
             says when a job set aside is ready",
        ),
        move || {
            while let Ok(job) = jobs.recv() {
                if handed_on.send(Next::Job(job)).is_err() {
                    return;
                }
            }
            // The workers may be gone already.
            let _ = handed_on.send(Next::End);
        },
    );
    let pool = Arc::new(WorkerPool {
        jobs: Mutex::new(next),
        wake,
        places: Mutex::new(starts.clone()),
        stuck: AtomicUsize::new(0),
        wait_for_turn: Box::new(wait_for_turn),
        set_aside_by: Box::new(set_aside_by),
        stalled: Mutex::new(Stalled {
            folders: Vec::new(),
            set_aside: Vec::new(),
            set_aside_files: Default::default(),
            ready: Default::default(),
        }),
        work: Box::new(work),
        #[cfg(test)]
        wakeups: AtomicUsize::new(0),
    });
    for (place, start) in starts.into_iter().enumerate() {
        start_worker(&pool, place, start);
    }
    let watched = Arc::downgrade(&pool);
    // It ends with the pool, once its workers have ended.
    spawn(
        Busy::NoPointer("the watch over a pool's workers, which only starts workers"),
        move || {
            loop {
                std::thread::sleep(stuck_after / 4);
                let Some(pool) = watched.upgrade() else {
                    return;
                };
                replace_stuck_workers(&pool, stuck_after);
            }
        },
    );
    pool
}

/// Gives up each worker of `pool` whose job is stuck (`Progress::is_stuck`), and starts a
/// new worker in its place, while fewer than `MOST_STUCK` are stuck. The job's folder is set
/// aside only when the job made no progress, as a read from a share that stopped answering
/// does. A job that ran past the ceiling on the processor, such as a long decode on a slow
/// machine, says nothing about its folder. Before, the folder's later jobs waited for it too.
fn replace_stuck_workers<J: Send + 'static>(
    pool: &Arc<WorkerPool<J>>,
    stuck_after: std::time::Duration,
) {
    let mut places = lock_even_if_poisoned(&pool.places);
    for (place, start) in places.iter_mut().enumerate() {
        let stuck_in = (lock_even_if_poisoned(start).as_mut()).and_then(|(progress, folder)| {
            let stalls_its_folder = progress.idle_for() >= stuck_after;
            let folder = folder.clone().filter(|_| stalls_its_folder);
            progress.is_stuck(stuck_after).then_some(folder)
        });
        if let Some(folder) = stuck_in
            && pool.stuck.load(Ordering::Relaxed) < MOST_STUCK
        {
            pool.stuck.fetch_add(1, Ordering::Relaxed);
            // Noted before the place is given away, so the worker's note sees it given up.
            lock_even_if_poisoned(&pool.stalled).folders.extend(folder);
            *start = JobStart::default();
            start_worker(pool, place, start.clone());
        }
    }
}

/// Starts the worker of `pool` in place `place`, which notes when it starts each job in `start`.
fn start_worker<J: Send + 'static>(pool: &Arc<WorkerPool<J>>, place: usize, start: JobStart) {
    let pool = pool.clone();
    // Each answer is shown when it comes.
    spawn(
        Busy::NoPointer("a worker of a pool, which lasts as long as the window"),
        move || {
            loop {
                (pool.wait_for_turn)(place);
                let Some(job) = next_job(&pool) else {
                    return;
                };
                let set_aside_by = (pool.set_aside_by)(&job);
                let folder = set_aside_by.as_ref().map(|(folder, _)| folder.clone());
                if let Some((folder, file)) = set_aside_by {
                    let mut stalled = lock_even_if_poisoned(&pool.stalled);
                    if stalled.folders.contains(&folder) {
                        stalled.set_aside(folder, file, job);
                        continue;
                    }
                }
                let note = JobNote::new(&pool, place, &start, folder);
                (pool.work)(job);
                drop(note);
                if !is_place_of(&pool, place, &start) {
                    pool.stuck.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        },
    );
}

/// Returns the next job of `pool`: a job set aside whose folder answers again, or else a new
/// one. Returns `None` when the sending half is gone. Each job set aside that is ready sends a
/// `Next::Ready`, so a worker that waits wakes for it, and one that took it already finds
/// nothing and waits again.
fn next_job<J>(pool: &WorkerPool<J>) -> Option<J> {
    loop {
        if let Some(job) = lock_even_if_poisoned(&pool.stalled).ready.pop_front() {
            return Some(job);
        }
        let received = lock_even_if_poisoned(&pool.jobs).recv();
        #[cfg(test)]
        pool.wakeups.fetch_add(1, Ordering::Relaxed);
        match received {
            Ok(Next::Job(job)) => return Some(job),
            Ok(Next::Ready) => {}
            Ok(Next::End) | Err(_) => {
                // The next worker that waits ends too.
                let _ = pool.wake.send(Next::End);
                return None;
            }
        }
    }
}

/// Returns whether the worker that notes its jobs in `start` still has place `place` of `pool`,
/// and was not given up.
fn is_place_of<J>(pool: &WorkerPool<J>, place: usize, start: &JobStart) -> bool {
    Arc::ptr_eq(&lock_even_if_poisoned(&pool.places)[place], start)
}

pub(crate) fn set_cpu_use(level: usize) {
    let _lock = lock_even_if_poisoned(&CPU_CHANGED.0);
    #[cfg(test)]
    crate::support::test_support::shared_setting_set(
        CPU_USE_LEVEL.load(Ordering::Relaxed) != level,
    );
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
            Some(Told::FailedWrite(Failure::from(crate::support::lang::tr(
                "A change could not be saved, because Glimt met an error while writing it",
            ))))
        };
        if let Some(told) = catch_panic(work).unwrap_or_else(broke) {
            notices.tell(told);
        }
        drop(held);
        ctx.request_repaint();
    }
}

/// Starts a thread that does `work`, and shows the busy pointer as `busy` says. This is the one
/// way a thread is started outside the tests (clippy.toml), so that every thread says what it
/// does for the busy pointer. A scope's thread (`std::thread::scope`) ends before its scope,
/// so the thread that runs the scope holds the pointer for it. Work that writes is started with
/// `spawn_told` instead.
pub(crate) fn spawn<R: Send + 'static>(
    busy: Busy,
    work: impl FnOnce() -> R + Send + 'static,
) -> std::thread::JoinHandle<R> {
    if let Busy::ShownBy(why) | Busy::NoPointer(why) = &busy {
        debug_assert!(
            !why.trim().is_empty(),
            "a thread says why it has no pointer of its own"
        );
    }
    // Its busy pointer: the one that `busy` holds or names, or none for the reason it gives.
    #[allow(clippy::disallowed_methods)] // the one place that starts a thread, given its Busy
    std::thread::spawn(move || {
        let answer = work();
        if let Busy::Pointer(pointer, ctx) = busy {
            drop(pointer);
            ctx.request_repaint();
        }
        answer
    })
}

/// Starts a thread that does `work`, which writes, through `told_work`: what it returns and a
/// panic in it are told to the window through `notices`, and `held` (the busy pointer, the
/// count of writing) is dropped only after the telling.
pub(crate) fn spawn_told(
    notices: NoticeSender,
    ctx: &eframe::egui::Context,
    held: impl Send + 'static,
    work: impl FnOnce() -> Option<Told> + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let work = told_work(notices, ctx, held, work);
    spawn(
        Busy::ShownBy("what it holds, dropped once it has told what it did"),
        work,
    )
}

/// Starts a thread for a test, which says nothing of the busy pointer.
#[cfg(test)]
pub(crate) fn spawn_in_a_test<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> std::thread::JoinHandle<R> {
    // No busy pointer: a test's own thread, which the test waits for itself.
    #[allow(clippy::disallowed_methods)] // a test's own thread, which no busy pointer counts
    std::thread::spawn(work)
}

/// Starts a thread named `name` for a test, as `spawn_in_a_test` does. Returns the error of a
/// thread that could not be started.
#[cfg(test)]
pub(crate) fn named_thread_in_a_test<R: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> R + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<R>> {
    // No busy pointer: a test's own thread, which the test waits for itself.
    #[allow(clippy::disallowed_methods)] // a test's own named thread, which no pointer counts
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
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

/// Waits while `path` is one of `HELD_FILES`, for a test of what the window does while the work
/// on that file still runs.
#[cfg(test)]
pub(crate) fn wait_in_a_test_while_held(path: &std::path::Path) {
    while lock_even_if_poisoned(&HELD_FILES).iter().any(|p| p == path) {
        std::thread::sleep(std::time::Duration::from_millis(5));
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
        crate::support::threads::spawn_in_a_test(move || {
            notice_sender.send("done".into()).unwrap()
        })
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
        super::spawn_worker_pool(1, Duration::from_millis(200), rx, |_| {}, |_| None, work);
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

    /// Runs a pool of one worker that sets jobs aside by their folder. A job in "/share" waits
    /// for `answers`, as a read from a share that stopped answering does, and panics when its
    /// name ends in "panics". Returns the sending half and the jobs done, in order.
    fn pool_beside_a_share(
        answers: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (
        std::sync::mpsc::Sender<std::path::PathBuf>,
        std::sync::mpsc::Receiver<std::path::PathBuf>,
    ) {
        use std::path::PathBuf;
        use std::sync::atomic::Ordering;
        let (jobs, rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let answers = answers.clone();
        let work = move |job: PathBuf| {
            while job.starts_with("/share") && !answers.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(!job.ends_with("panics"), "a broken file");
            done_tx.send(job).unwrap();
        };
        let by_folder = |job: &PathBuf| Some((job.parent()?.to_path_buf(), job.clone()));
        let stuck_after = std::time::Duration::from_millis(100);
        super::spawn_worker_pool(1, stuck_after, rx, |_| {}, by_folder, work);
        (jobs, done)
    }

    /// A page of thumbnails on a share that stopped answering gives up one worker, and the
    /// later jobs of its folder are set aside, so another folder's job is done at once. They
    /// are done once the share answers again. Before, each one gave up another worker, until
    /// `MOST_STUCK` were stuck and no job of any folder was done (#154).
    #[test]
    fn a_stalled_folder_does_not_use_up_the_pool() {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, Ordering};
        let answers = std::sync::Arc::new(AtomicBool::new(false));
        let (jobs, done) = pool_beside_a_share(&answers);
        for n in 0..=super::MOST_STUCK + 8 {
            jobs.send(PathBuf::from(format!("/share/{n}.jpg"))).unwrap();
        }
        jobs.send(PathBuf::from("/local/a.jpg")).unwrap();
        let ten_seconds = std::time::Duration::from_secs(10);
        assert_eq!(
            done.recv_timeout(ten_seconds),
            Ok(PathBuf::from("/local/a.jpg")),
            "the stalled folder used up the pool"
        );
        answers.store(true, Ordering::SeqCst);
        for _ in 0..=super::MOST_STUCK + 8 {
            let job = done.recv_timeout(ten_seconds).unwrap();
            assert!(job.starts_with("/share"), "{job:?}");
        }
    }

    /// An idle pool waits for a job without waking, as nothing is set aside. Before, every
    /// pool woke four times a second for ever, also those that never set a job aside.
    #[test]
    fn an_idle_pool_does_not_wake() {
        use std::sync::atomic::Ordering;
        let (jobs, rx) = std::sync::mpsc::channel::<std::path::PathBuf>();
        let stuck_after = std::time::Duration::from_secs(3600);
        let pool = super::start_pool(2, stuck_after, rx, |_| {}, |_| None, |_| {});
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert_eq!(pool.wakeups.load(Ordering::Relaxed), 0, "an idle pool woke");
        drop(jobs);
    }

    /// A job for a file that is set aside again, as the user comes back to a folder whose share
    /// is still down, takes the place of the one set aside before, and only the newest is done
    /// once the share answers. Before, each time added the same jobs again (#262).
    #[test]
    fn a_job_set_aside_again_takes_the_place_of_the_one_before() {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, Ordering};
        let answers = std::sync::Arc::new(AtomicBool::new(false));
        let (jobs, rx) = std::sync::mpsc::channel::<(PathBuf, u32)>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let still_down = answers.clone();
        let work = move |job: (PathBuf, u32)| {
            while job.0.starts_with("/share") && !still_down.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            done_tx.send(job).unwrap();
        };
        let by_folder = |job: &(PathBuf, u32)| Some((job.0.parent()?.to_path_buf(), job.0.clone()));
        let stuck_after = std::time::Duration::from_millis(100);
        let pool = super::start_pool(1, stuck_after, rx, |_| {}, by_folder, work);
        jobs.send((PathBuf::from("/share/0.jpg"), 0)).unwrap();
        for n in 1..=50 {
            jobs.send((PathBuf::from("/share/1.jpg"), n)).unwrap();
        }
        jobs.send((PathBuf::from("/local/a.jpg"), 0)).unwrap();
        let ten_seconds = std::time::Duration::from_secs(10);
        assert_eq!(
            done.recv_timeout(ten_seconds),
            Ok((PathBuf::from("/local/a.jpg"), 0))
        );
        let set_aside = super::lock_even_if_poisoned(&pool.stalled).set_aside.len();
        assert_eq!(set_aside, 1, "the jobs set aside grew");
        answers.store(true, Ordering::SeqCst);
        let mut rest = vec![
            done.recv_timeout(ten_seconds).unwrap(),
            done.recv_timeout(ten_seconds).unwrap(),
        ];
        rest.sort();
        assert_eq!(
            rest,
            [
                (PathBuf::from("/share/0.jpg"), 0),
                (PathBuf::from("/share/1.jpg"), 50)
            ]
        );
        let more = done.recv_timeout(std::time::Duration::from_millis(300));
        assert!(more.is_err(), "{more:?}");
    }

    /// The jobs set aside are done also when the job that stalled their folder ends in a
    /// panic, after it was given up.
    #[test]
    fn a_stalled_folder_is_let_go_after_a_panic() {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, Ordering};
        let answers = std::sync::Arc::new(AtomicBool::new(false));
        let (jobs, done) = pool_beside_a_share(&answers);
        jobs.send(PathBuf::from("/share/panics")).unwrap();
        jobs.send(PathBuf::from("/share/next.jpg")).unwrap();
        jobs.send(PathBuf::from("/local/a.jpg")).unwrap();
        let ten_seconds = std::time::Duration::from_secs(10);
        assert_eq!(
            done.recv_timeout(ten_seconds),
            Ok(PathBuf::from("/local/a.jpg"))
        );
        answers.store(true, Ordering::SeqCst);
        assert_eq!(
            done.recv_timeout(ten_seconds),
            Ok(PathBuf::from("/share/next.jpg")),
            "a job set aside stayed after the stalled job panicked"
        );
    }

    /// A worker that a panic in its job ends takes away its note of the job and starts another
    /// worker in its place at once. Before, its note stayed and read the clock of a thread
    /// that was gone, which the system may give to another thread. The place was then filled
    /// only when that clock had shown no progress for `stuck_after`, or never.
    #[test]
    fn a_worker_ended_by_a_panic_is_replaced_at_once() {
        use std::path::PathBuf;
        use std::time::Duration;
        let (jobs, rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let work = move |job: PathBuf| {
            assert!(!job.ends_with("panics"), "a broken file");
            done_tx.send(job).unwrap();
        };
        super::spawn_worker_pool(1, Duration::from_secs(3600), rx, |_| {}, |_| None, work);
        for name in ["/a/panics", "/b/next"] {
            jobs.send(PathBuf::from(name)).unwrap();
        }
        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(PathBuf::from("/b/next")),
            "the place of a worker ended by a panic was not filled"
        );
    }

    /// A worker whose job runs on the processor for ever, as a decoder sent into an endless
    /// loop by a crafted file does, is given up after `Progress::CEILING` times `stuck_after`,
    /// and the next job is done by a new worker. Before, it counted as slow, not stuck, and was
    /// never given up. Its folder is not set aside, as for a stalled share, because a job that
    /// uses the processor says nothing about the folder. Before, the folder's next job waited
    /// for it to end.
    #[test]
    fn a_job_that_never_ends_is_given_up() {
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        let stuck_after = Duration::from_millis(50);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_spinning = stop.clone();
        let (jobs, rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let work = move |job: PathBuf| {
            while job.ends_with("endless.jpg") && !stop_spinning.load(Ordering::SeqCst) {
                std::hint::black_box(0);
            }
            done_tx.send(job).unwrap();
        };
        let by_folder = |job: &PathBuf| Some((job.parent()?.to_path_buf(), job.clone()));
        super::spawn_worker_pool(1, stuck_after, rx, |_| {}, by_folder, work);
        jobs.send(PathBuf::from("/crafted/endless.jpg")).unwrap();
        jobs.send(PathBuf::from("/crafted/next.jpg")).unwrap();
        let next = done.recv_timeout(Duration::from_secs(10));
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            next,
            Ok(PathBuf::from("/crafted/next.jpg")),
            "the endless job was kept, or its folder set aside"
        );
    }

    /// A job that waited for room in the decode budget for longer than the ceiling is not
    /// stuck when its wait ends, because the ceiling counts from the end of the wait. Before,
    /// a preload that waited long for another job was given up as soon as it began to decode.
    #[test]
    fn the_ceiling_counts_from_the_end_of_a_wait_for_room() {
        use std::time::Duration;
        let stuck_after = Duration::from_millis(50);
        let mut progress = super::Progress::of_this_thread();
        let waiting = super::WaitingForRoom::start();
        let start = std::time::Instant::now();
        while start.elapsed() < stuck_after * (super::Progress::CEILING + 2) {
            assert!(
                !progress.is_stuck(stuck_after),
                "a job waiting for room was stuck"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(waiting);
        assert!(
            !progress.is_stuck(stuck_after),
            "the wait for room counted against the ceiling"
        );
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

    /// A thread of `spawn` holds its busy pointer while its work runs, and lets it go and asks
    /// for a frame when the work ends, also by a panic. Risk: `--remote … --wait` answering
    /// before the work is done, or a pointer left on until the mouse moves.
    #[test]
    fn a_thread_holds_its_busy_pointer_until_its_work_ends() {
        use super::{Busy, Working, spawn};
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering, mpsc};
        let ctx = eframe::egui::Context::default();
        let asked = Arc::new(AtomicUsize::new(0));
        let frames = asked.clone();
        ctx.set_request_repaint_callback(move |_| {
            frames.fetch_add(1, Ordering::Relaxed);
        });
        let count = Arc::new(AtomicUsize::new(0));
        let (go, wait) = mpsc::channel::<()>();
        let pointer = Busy::Pointer(Working::new(&count), ctx.clone());
        let held = spawn(pointer, move || wait.recv().is_ok());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        go.send(()).unwrap();
        assert!(held.join().unwrap());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(asked.load(Ordering::Relaxed) > 0);
        let broke = spawn(Busy::Pointer(Working::new(&count), ctx), || {
            panic!("a broken file");
        });
        assert!(broke.join().is_err());
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    /// The busy pointer goes when its work ends, also by a panic.
    #[test]
    fn busy_ends_with_a_panic() {
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let pointer = super::Working::new(&count);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let _ = crate::support::threads::spawn_in_a_test(move || {
            let _pointer = pointer;
            panic!("a broken file");
        })
        .join();
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}
