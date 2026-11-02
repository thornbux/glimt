//! Threads: how many for each kind of work (the CPU use level), work split over them
//! (`in_parts`), a background thread's answer (`Receiving`), and work counted while it runs
//! (`Working`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// The CPU use level (index in CPU_LEVELS), for the worker threads, and the signal that wakes
/// the thumbnail workers it leaves idle when it changes.
pub(crate) static CPU_USE: AtomicUsize = AtomicUsize::new(1);

pub(crate) static CPU_CHANGED: (Mutex<()>, std::sync::Condvar) =
    (Mutex::new(()), std::sync::Condvar::new());

pub(crate) fn set_cpu_use(level: usize) {
    let _lock = CPU_CHANGED.0.lock().unwrap();
    CPU_USE.store(level, Ordering::Relaxed);
    CPU_CHANGED.1.notify_all();
}

pub(crate) fn cores() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get())
}

/// Threads for parallel work in the background: searches, the EXIF index, Find Duplicates and
/// pan view.
pub(crate) fn background_threads() -> usize {
    background_threads_at(CPU_USE.load(Ordering::Relaxed))
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
    thumb_threads_at(CPU_USE.load(Ordering::Relaxed), decoders)
}

pub(crate) fn thumb_threads_at(level: usize, decoders: usize) -> usize {
    match level {
        0 => 1,
        1 => decoders.min(2),
        _ => cores(),
    }
}

/// Glimt runs with one malloc arena (`low_memory`): set at startup.
pub(crate) static ONE_ARENA: AtomicBool = AtomicBool::new(false);

/// Threads for work that allocates a little for every file, such as reading EXIF for the
/// exif: index or pan view. With one malloc arena they wait for each other in the allocator:
/// 2,000 files took 206 ms on one thread, 128 ms on two and 551 ms on four (20 cores); with an
/// arena each, eight threads took 68 ms.
pub(crate) fn small_alloc_threads() -> usize {
    match ONE_ARENA.load(Ordering::Relaxed) {
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

/// What a background thread's receiver has: nothing yet, its answer, or the thread gone without
/// one (it panicked). Taken, the receiver is dropped, so nothing waits for it for ever.
pub(crate) enum Took<T> {
    Nothing,
    Answer(T),
    Gone,
}

/// A background thread's receiver, `none` while nothing runs. Its messages come only through
/// `take`, `next`, `latest` and `wait`, each of which tells once when the thread is gone
/// (`Took::Gone`) and forgets the receiver then, so no receiver polled by hand can leave the
/// window waiting for a thread that broke.
#[derive(Default)]
pub(crate) struct Receiving<T>(Option<mpsc::Receiver<T>>);

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
    /// state so far again and again. The receiver stays, as `next`.
    pub(crate) fn latest(&mut self) -> Took<T> {
        let mut last = Took::Nothing;
        loop {
            match self.next() {
                Took::Answer(a) => last = Took::Answer(a),
                Took::Gone if matches!(last, Took::Nothing) => return Took::Gone,
                _ => return last,
            }
        }
    }

    /// As `take`, waiting up to `timeout` for the answer.
    pub(crate) fn wait(&mut self, timeout: std::time::Duration) -> Took<T> {
        match self.0.as_ref().map(|rx| rx.recv_timeout(timeout)) {
            None | Some(Err(mpsc::RecvTimeoutError::Timeout)) => Took::Nothing,
            Some(Ok(answer)) => {
                self.0 = None;
                Took::Answer(answer)
            }
            Some(Err(mpsc::RecvTimeoutError::Disconnected)) => {
                self.0 = None;
                Took::Gone
            }
        }
    }
}

/// One piece of background work, for the busy pointer (`busy`) or for closing
/// (`config::WRITING`): counted while held, and no longer when dropped, also as its thread
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
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[repr(C)]
pub(crate) struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    pub(crate) fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

/// The processor time this thread has used, in microseconds: a frame's own work, which other
/// threads busy at the same moment (summing the sizes of 500,000 files) do not add to.
pub(crate) fn thread_cpu_us() -> u64 {
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `t` is a valid timespec for the call to fill.
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.sec as u64 * 1_000_000 + t.nsec as u64 / 1000
}

#[cfg(test)]
mod tests {
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
}
