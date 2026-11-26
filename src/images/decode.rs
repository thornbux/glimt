//! Decoding turns an image file into pixels at the size it is shown (a JPEG scaled while it is
//! decoded, an SVG drawn to size), with its EXIF turn, its colours and its histogram.
//! Animations are decoded frame by frame.

use crate::images::colour::{ADOBE_RGB, to_display, to_srgb};
use crate::model::listing::has_ext;
use crate::support::format::human_size;
use crate::support::threads::{Progress, WaitingForRoom, lock_even_if_poisoned};
use crate::{images::raw, metadata::exif};
use eframe::egui;
use image::ImageDecoder;
use image::metadata::Orientation;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// How many bytes of decoded frames are kept of one animation. A longer one loops its first
/// part.
pub static MAX_ANIMATION_BYTES: AtomicUsize = AtomicUsize::new(128 << 20);

/// Frames of an animation kept at most, whatever their size, and given at most to Save This
/// Frame and Export Frames (`each_frame`).
const MAX_FRAMES: usize = 1000;

/// The most bytes of whole canvases that the view's decode of one animation makes, over all its
/// frames. Every frame is a whole canvas before it is shrunk, so a small GIF with a huge canvas
/// and many tiny frames would else take minutes, though what is kept of each frame is small.
const MAX_ANIMATION_CANVAS_BYTES: u64 = 4 * MAX_DECODE_BYTES;

/// image's default limit (`Limits::max_alloc`). Pixels are measured against it only by
/// `fits_decode_limit`; elsewhere it caps the bytes read from a file or a program.
pub const MAX_DECODE_BYTES: u64 = 512 << 20;

/// What all decodes at once may allocate, as much as the largest single decode may (a JPEG XL,
/// `4 * MAX_DECODE_BYTES`). Each decode keeps its own limit, but the view's decoders, the
/// thumbnail workers, Print and Check Images run at the same time, and a folder of files that
/// each claim nearly the limit would otherwise take gigabytes together.
pub const DECODE_BUDGET: u64 = 4 * MAX_DECODE_BYTES;

/// The error of a decode whose file could not be looked at, such as one on a share that
/// dropped for a moment. It says nothing about the file (`failure_of`).
const NOT_LOOKED_AT: &str = "the file could not be looked at";

/// The error of a decode whose file changed while it was read (`DecodeInput::holding`).
const CHANGED_WHILE_READ: &str = "the file changed while it was read";

/// The error of a read or a decode whose bytes the budget does not count (`count_whole_read`,
/// `is_counted_input`). It is a fault in Glimt, never in the file (`failure_of`).
const NOT_COUNTED: &str = "the bytes to decode are not counted in the decode budget";

/// How long a decode job may hold bytes of the budget without using the processor while
/// another decode waits for room, before the budget takes them back: as long as a worker of a
/// pool may take over one job. A read from a share that stopped answering waits for ever, and
/// before, two large images there held the whole budget, and no other image was decoded until
/// a restart. The tests wait less.
#[cfg(not(test))]
const HELD_TOO_LONG: Duration = crate::support::threads::STUCK_AFTER;
#[cfg(test)]
const HELD_TOO_LONG: Duration = Duration::from_secs(1);

/// The shared budget that every decode takes its bytes from (`reserve_from_budget`).
static BUDGET: DecodeBudget = DecodeBudget::new(DECODE_BUDGET);

/// The number of the next decode job, by which the budget knows what each job holds.
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The bytes of the budget that this thread's decode job holds, or `None` outside a job.
    static JOB_HOLDS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// The number of this thread's decode job (`NEXT_JOB`), or 0 outside a job.
    static JOB_NUMBER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Whether this thread's decodes take their bytes before the others that wait, because
    /// they are of the shown image (`let_this_thread_go_first`).
    static GOES_FIRST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The bytes of the files and program output that this thread's decodes hold while they
    /// make pixels of them (`DecodeInput`). Every reservation takes them on top.
    static INPUT_HOLDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The part of `INPUT_HOLDS` that this thread's decode job read whole outside a
    /// `DecodeInput` (`count_whole_read`). It counts until the job ends.
    static JOB_INPUT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// The budget of this thread, when it was given one. The tests run side by side in one process,
// and a test that held much of the shared budget for seconds, or waited first in line for all
// of it, held up every decode of the other tests behind it. So each test's thread (named after
// its test) has a budget of its own, and the other threads share `BUDGET`. A thread that a test
// starts is given the test's budget, or one of its own, by setting this.
#[cfg(test)]
thread_local! {
    static TEST_BUDGET: std::cell::Cell<Option<&'static DecodeBudget>> =
        const { std::cell::Cell::new(None) };
    /// The path whose opens `open_regular_with_flags` counts on this thread, and how many there
    /// were, so that a test sees how often a turn or a copy reads its file.
    pub(crate) static OPENS: std::cell::RefCell<Option<(PathBuf, usize)>> =
        const { std::cell::RefCell::new(None) };
}

/// The page of a multi-page file, counted from 0, as a decode job and its result carry it. It
/// has a type of its own so that it is never taken for a place in the list, which a pool's
/// workers must not judge a job by (`source_rules::worker_pools_judge_jobs_by_their_files`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageNumber(pub usize);

pub struct Decoded {
    /// The file the job was for. It rejects results from a folder that has since been replaced.
    pub path: PathBuf,
    /// Whether it was decoded with the EXIF rotation. It is rejected if the setting changed
    /// meanwhile.
    pub rotated: bool,
    /// The page of a multi-page TIFF. It is 0 for everything else.
    pub page: PageNumber,
    /// The request's number, as in `App::pending`.
    pub request_number: u64,
    /// `None` = skipped because the user has already moved past it.
    pub result: Option<Result<DecodedImage, String>>,
    /// The fingerprint of the file, taken before it was read, so the window can tell whether
    /// the file under its name is still the one decoded.
    pub read: Option<crate::files::file_io::FileFingerprint>,
}

/// The frames, the original size after EXIF rotation, the histogram and the number of pages.
pub type DecodedImage = (Frames, [usize; 2], Box<HistogramCounts>, usize);

/// Pixel counts per level of red, green, blue, brightness and alpha.
pub type HistogramCounts = [[u32; 256]; 5];

/// The decoded frames with their delays in ms. A still image has one frame.
pub type Frames = Vec<(egui::ColorImage, u32)>;

/// An image, its ICC profile if it has one, and its size upright before any shrinking.
pub type ShrunkImage = (image::DynamicImage, Option<Vec<u8>>, [usize; 2]);

/// A page of an image, whole, and its ICC profile if it has one.
pub type Page = (image::DynamicImage, Option<Vec<u8>>);

/// An image as read, its ICC profile, the EXIF turn still to do, and its real size where it
/// came smaller.
type Unturned = (
    image::DynamicImage,
    Option<Vec<u8>>,
    Orientation,
    Option<[usize; 2]>,
);

/// An animation's frames, decoded as they are taken, and its size, or why it is not decoded.
type Animated = Result<(image::Frames<'static>, (u32, u32)), String>;

/// What a failed decode says about its file, told by its error (`failure_of`).
#[derive(Debug, PartialEq)]
pub enum DecodeFailure {
    /// The file is broken, or it is no image that Glimt reads.
    OfTheFile,
    /// The file is larger than Glimt decodes, or needs more memory than a decode may take. It
    /// may be a good image, such as a large panorama.
    TooLarge,
    /// The program that reads it did not end in time. A busy computer or a share that woke
    /// slowly causes that too.
    TimedOut,
    /// Nothing about the file: the program that reads it is not there, or the file could not
    /// be looked at or read.
    NotOfTheFile,
}

/// Bytes that decodes take from a shared total before they allocate, and give back after.
pub struct DecodeBudget {
    state: std::sync::Mutex<BudgetState>,
    freed: std::sync::Condvar,
    total: u64,
}

impl DecodeBudget {
    const fn new(total: u64) -> DecodeBudget {
        DecodeBudget {
            state: std::sync::Mutex::new(BudgetState {
                free: total,
                turns_given: [0; 2],
                turns_served: [0; 2],
                holds: Vec::new(),
            }),
            freed: std::sync::Condvar::new(),
            total,
        }
    }

    /// Takes `bytes`, and waits while other decodes hold too much of the budget. The decodes
    /// take their bytes in the order they asked, so a large one is not passed again and again
    /// by smaller ones that come after it, and those of the shown image (`goes_first`) take
    /// theirs before every other that waits. Returns an error when `bytes` alone is more than
    /// the whole budget, because no wait makes room then.
    ///
    /// The bytes that the caller's job `job` holds are given back first, in the same step as
    /// its turn is taken, so that no other decode takes them between the two. A decode that
    /// needs more than it holds would else lose them to one that came after it.
    ///
    /// A job that has held its bytes for `HELD_TOO_LONG` without using the processor while this
    /// one waits for room is taken to be stuck, and its bytes are taken back
    /// (`BudgetState::take_back_stuck`).
    fn take(&self, job: u64, bytes: u64, goes_first: bool) -> Result<(), String> {
        if bytes > self.total {
            self.give_back(job);
            return Err(format!("image too large ({})", human_size(bytes)));
        }
        let line = usize::from(!goes_first);
        let mut state = lock_even_if_poisoned(&self.state);
        state.let_go(job);
        let turn = state.turns_given[line];
        state.turns_given[line] += 1;
        let _waiting = WaitingForRoom::start();
        while !state.is_turn_of(line, turn, bytes) {
            state = (self.freed.wait_timeout(state, HELD_TOO_LONG / 4))
                .unwrap_or_else(|e| e.into_inner())
                .0;
            if state.free < bytes && state.take_back_stuck() {
                self.freed.notify_all();
            }
        }
        state.free -= bytes;
        state.turns_served[line] += 1;
        state.holds.push(Hold {
            job,
            bytes,
            progress: Progress::of_this_thread(),
        });
        drop(state);
        // The next in line may fit in what is left.
        self.freed.notify_all();
        Ok(())
    }

    /// Gives back the bytes that job `job` holds, and wakes the decodes that wait for room.
    fn give_back(&self, job: u64) {
        if lock_even_if_poisoned(&self.state).let_go(job) {
            self.freed.notify_all();
        }
    }
}

/// The bytes of a `DecodeBudget` that are free, and the turns of the decodes that wait for
/// them, in two lines: the shown image's decodes (0) and the others (1).
struct BudgetState {
    free: u64,
    /// The number of turns given in each line, and of those served, which took their bytes.
    turns_given: [u64; 2],
    turns_served: [u64; 2],
    /// What each decode job holds, and since when.
    holds: Vec<Hold>,
}

impl BudgetState {
    /// Frees what job `job` holds. Returns whether it held anything. A job whose bytes were
    /// taken back while it was stuck holds nothing, so they are not freed twice when it wakes.
    fn let_go(&mut self, job: u64) -> bool {
        let Some(at) = self.holds.iter().position(|h| h.job == job) else {
            return false;
        };
        self.free += self.holds.swap_remove(at).bytes;
        true
    }

    /// Takes back the bytes of every job that has made no progress for `HELD_TOO_LONG`, such
    /// as one whose read from a stalled share never returns. A job that is only slow uses the
    /// processor, and keeps its bytes, unless it runs past the ceiling (`Progress::is_stuck`).
    /// Returns whether it took any.
    /// What such a job has made of them lies outside the budget from then on, as its worker
    /// lies outside its pool (`spawn_worker_pool`), which gives it up after as long.
    fn take_back_stuck(&mut self) -> bool {
        let mut freed = 0;
        self.holds.retain_mut(|h| {
            let is_stuck = h.progress.is_stuck(HELD_TOO_LONG);
            if is_stuck {
                freed += h.bytes;
            }
            !is_stuck
        });
        self.free += freed;
        freed > 0
    }

    /// Returns whether turn `turn` in line `line` may take `bytes` now: it is the first in its
    /// line, the bytes are free, and no decode of the shown image waits before the others.
    fn is_turn_of(&self, line: usize, turn: u64, bytes: u64) -> bool {
        let shown_waits = self.turns_served[0] < self.turns_given[0];
        self.turns_served[line] == turn && self.free >= bytes && (line == 0 || !shown_waits)
    }
}

/// The bytes of the budget that a decode job holds (`BudgetState::holds`).
struct Hold {
    job: u64,
    bytes: u64,
    /// Whether the job still uses the processor, since it took the bytes.
    progress: Progress,
}

/// What a decode took from the budget (`reserve_from_budget`). Outside a decode job it holds the
/// job that the reservation started, which gives the bytes back when it is dropped. Inside a
/// job it holds nothing, because the job holds the bytes.
pub struct BudgetReservation {
    _job: Option<DecodeJobBudget>,
}

/// A decode job on this thread (`start_decode_job`). When the outermost one is dropped, the
/// bytes that the job's decodes took are given back.
pub struct DecodeJobBudget {
    is_outermost: bool,
    /// The job's bytes are counted in this thread's `JOB_HOLDS`, so it is not sent to another
    /// thread, where its drop would give back that thread's bytes.
    _stays_on_its_thread: std::marker::PhantomData<*const ()>,
}

impl Drop for DecodeJobBudget {
    fn drop(&mut self) {
        if self.is_outermost {
            INPUT_HOLDS.set(INPUT_HOLDS.get() - JOB_INPUT.take());
            JOB_HOLDS.take();
            budget().give_back(JOB_NUMBER.take());
        }
    }
}

/// A file's bytes, or a program's output, that a decode holds while it makes pixels of them.
/// Its bytes are taken from the budget before they are read. While it lives, every later
/// reservation of this thread asks for them on top (`INPUT_HOLDS`), so the budget counts what
/// was read together with the pixels made of it. Before, only the pixels counted, and a folder
/// of small JPEGs that each end in 500 MB of zeros held gigabytes outside the budget.
pub struct DecodeInput {
    bytes: Vec<u8>,
    reserved: u64,
    /// It is not sent to another thread, because its bytes count in this thread's
    /// `INPUT_HOLDS`. The reservation's job keeps it there.
    _reservation: BudgetReservation,
}

impl DecodeInput {
    /// Takes `cap` bytes of the budget for input that is about to be read, such as a program's
    /// output of at most `cap` bytes, and returns it empty. Returns an error when `cap` with
    /// what this thread holds already is more than the whole budget.
    pub fn reserve(cap: u64) -> Result<DecodeInput, String> {
        let reservation = reserve_from_budget(cap)?;
        INPUT_HOLDS.set(INPUT_HOLDS.get() + cap);
        Ok(DecodeInput {
            bytes: Vec::new(),
            reserved: cap,
            _reservation: reservation,
        })
    }

    /// Puts `bytes` into the room reserved. Returns an error when they are more than was
    /// reserved, such as a file that grew while it was read.
    pub fn holding(mut self, bytes: Vec<u8>) -> Result<DecodeInput, String> {
        if bytes.len() as u64 > self.reserved {
            return Err(CHANGED_WHILE_READ.into());
        }
        self.bytes = bytes;
        Ok(self)
    }

    /// Takes more of the budget for input that grows past the room reserved while it is read,
    /// such as what a program still writes, so that it counts in the budget all the while.
    /// It takes at least twice the room it had, so that a large output asks only a few times,
    /// but never more than `cap`, the most the input may grow to. Before, the doubling could
    /// take nearly twice the cap, which other decodes then waited for. Returns an error when
    /// `length` with what this thread holds already is more than the whole budget.
    pub fn grow_to(&mut self, length: u64, cap: u64) -> Result<(), String> {
        if length <= self.reserved {
            return Ok(());
        }
        let more = length.max((self.reserved * 2).min(cap)) - self.reserved;
        INPUT_HOLDS.set(INPUT_HOLDS.get() + more);
        self.reserved += more;
        reserve_from_budget(0).map(drop)
    }

    /// Reads the whole of `path`, as `read_limited` does, after its size is taken from the
    /// budget. This is the way a decode reads a file whole.
    pub fn read(path: &Path) -> std::io::Result<DecodeInput> {
        let (file, meta) = open_regular_with_metadata(path)?;
        if meta.len() > MAX_DECODE_BYTES {
            return Err(file_too_large());
        }
        let input = DecodeInput::reserve(meta.len()).map_err(std::io::Error::other)?;
        let bytes = read_open_file((file, meta))?;
        input.holding(bytes).map_err(std::io::Error::other)
    }

    /// Reads the first `max` bytes of `path`, as `read_head` does, after they are taken from
    /// the budget.
    pub fn read_head(path: &Path, max: u64) -> std::io::Result<DecodeInput> {
        use std::io::Read;
        let (file, meta) = open_regular_with_metadata(path)?;
        let cap = meta.len().min(max);
        let input = DecodeInput::reserve(cap).map_err(std::io::Error::other)?;
        let mut head = Vec::new();
        #[allow(clippy::disallowed_methods)] // bounded by take
        file.take(cap).read_to_end(&mut head)?;
        input.holding(head).map_err(std::io::Error::other)
    }
}

impl std::ops::Deref for DecodeInput {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for DecodeInput {
    fn drop(&mut self) {
        INPUT_HOLDS.set(INPUT_HOLDS.get() - self.reserved);
    }
}

/// An image's file read whole once, for a turn or a copy made of it. Every step of the turn or
/// copy takes the pixels, the profile, the EXIF block, the XMP, a PNG's chunks and whether the
/// image is animated from these bytes (`ImageBytes`). So the file is read once, and that read
/// either fails and is told, or every step sees all of the file. Before, each step read the
/// file again, and a share that failed one of those reads made that step's part none, untold.
/// A turned PNG then lost its text and DPI, and a turned WebP its colour profile (#325). Its
/// bytes count in the decodes' budget while it lives, so it is dropped before a lock is
/// waited for.
pub(crate) struct Source {
    path: PathBuf,
    input: DecodeInput,
}

impl Source {
    /// Reads `path` whole (`DecodeInput::read`). Returns its error, of the kind `FileTooLarge`
    /// when the file is larger than a decode may read.
    pub(crate) fn read(path: &Path) -> std::io::Result<Source> {
        let input = DecodeInput::read(path)?;
        Ok(Source {
            path: path.to_owned(),
            input,
        })
    }
}

impl std::ops::Deref for Source {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.input
    }
}

/// Where a reader of an image takes its bytes from. That is the file at a path, opened by each
/// read as the view, the panel and the sort by date read it, or a `Source` read once for a turn
/// or a copy, whose reads cannot fail. The readers of pixels and metadata take either, so that
/// a turn or a copy reads its file through the same readers as the view, and never twice.
pub(crate) trait ImageBytes<'a>: Copy {
    /// The reader of the bytes: the file, or a `Cursor` over the bytes read.
    type Reader: std::io::BufRead + std::io::Seek;

    /// Returns the image's path. It is for the image's name and extension, and for the
    /// programs that read the file themselves (ffmpeg, the SVG drawer, an added format).
    fn path(self) -> &'a Path;

    /// Opens the bytes to be read from their start.
    fn open_reader(self) -> std::io::Result<Self::Reader>;

    /// Returns the first `max` bytes, or all of them when there are fewer (`read_head`).
    fn first_bytes(self, max: u64) -> std::io::Result<std::borrow::Cow<'a, [u8]>>;

    /// Returns all of the bytes (`read_limited`).
    fn all_bytes(self) -> std::io::Result<std::borrow::Cow<'a, [u8]>>;

    /// Returns an image crate reader of the bytes, of the format `image_reader` guesses.
    fn image_reader(self) -> std::io::Result<image::ImageReader<Self::Reader>>;

    /// Returns the format of the image, as `image_reader` guesses it, without decoding it.
    fn image_format(self) -> std::io::Result<Option<image::ImageFormat>>;

    /// Decodes the image that `dec`, a decoder of these bytes, reads (`from_decoder`).
    fn decode<D: ImageDecoder>(self, dec: D) -> Result<image::DynamicImage, String>;

    /// Calls `then` with all of the bytes, counted in the decodes' budget beside the pixels
    /// that `then` makes of them (`DecodeInput`).
    fn with_counted<T>(self, then: impl FnOnce(&[u8]) -> Result<T, String>) -> Result<T, String>;
}

impl<'a> ImageBytes<'a> for &'a Path {
    type Reader = std::io::BufReader<std::fs::File>;

    fn path(self) -> &'a Path {
        self
    }

    fn open_reader(self) -> std::io::Result<Self::Reader> {
        open_regular(self).map(std::io::BufReader::new)
    }

    fn first_bytes(self, max: u64) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        read_head(self, max).map(std::borrow::Cow::Owned)
    }

    fn all_bytes(self) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        read_limited(self).map(std::borrow::Cow::Owned)
    }

    fn image_reader(self) -> std::io::Result<image::ImageReader<Self::Reader>> {
        image_reader(self)
    }

    fn image_format(self) -> std::io::Result<Option<image::ImageFormat>> {
        image_format(self)
    }

    fn decode<D: ImageDecoder>(self, dec: D) -> Result<image::DynamicImage, String> {
        from_decoder(dec)
    }

    fn with_counted<T>(self, then: impl FnOnce(&[u8]) -> Result<T, String>) -> Result<T, String> {
        then(&DecodeInput::read(self).map_err(|e| e.to_string())?)
    }
}

impl<'a> ImageBytes<'a> for &'a PathBuf {
    type Reader = std::io::BufReader<std::fs::File>;

    fn path(self) -> &'a Path {
        self
    }

    fn open_reader(self) -> std::io::Result<Self::Reader> {
        self.as_path().open_reader()
    }

    fn first_bytes(self, max: u64) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        self.as_path().first_bytes(max)
    }

    fn all_bytes(self) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        self.as_path().all_bytes()
    }

    fn image_reader(self) -> std::io::Result<image::ImageReader<Self::Reader>> {
        self.as_path().image_reader()
    }

    fn image_format(self) -> std::io::Result<Option<image::ImageFormat>> {
        self.as_path().image_format()
    }

    fn decode<D: ImageDecoder>(self, dec: D) -> Result<image::DynamicImage, String> {
        self.as_path().decode(dec)
    }

    fn with_counted<T>(self, then: impl FnOnce(&[u8]) -> Result<T, String>) -> Result<T, String> {
        self.as_path().with_counted(then)
    }
}

impl<'a> ImageBytes<'a> for &'a Source {
    type Reader = std::io::Cursor<&'a [u8]>;

    fn path(self) -> &'a Path {
        &self.path
    }

    fn open_reader(self) -> std::io::Result<Self::Reader> {
        Ok(std::io::Cursor::new(&self.input[..]))
    }

    fn first_bytes(self, max: u64) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        let length = usize::try_from(max).unwrap_or(usize::MAX).min(self.len());
        Ok(std::borrow::Cow::Borrowed(&self.input[..length]))
    }

    fn all_bytes(self) -> std::io::Result<std::borrow::Cow<'a, [u8]>> {
        Ok(std::borrow::Cow::Borrowed(&self.input[..]))
    }

    fn image_reader(self) -> std::io::Result<image::ImageReader<Self::Reader>> {
        with_format_guessed(image::ImageReader::new(self.open_reader()?), &self.path)
    }

    fn image_format(self) -> std::io::Result<Option<image::ImageFormat>> {
        self.image_reader().map(|reader| reader.format())
    }

    fn decode<D: ImageDecoder>(self, dec: D) -> Result<image::DynamicImage, String> {
        // The bytes lie in the source's `DecodeInput`, which the budget counts.
        is_counted_input(&self.input[..])?;
        decode_within_limits(dec)
    }

    fn with_counted<T>(self, then: impl FnOnce(&[u8]) -> Result<T, String>) -> Result<T, String> {
        then(&self.input[..])
    }
}

/// Returns the budget that this thread's decodes take their bytes from, the shared `BUDGET`.
/// In tests, each test's own thread has a budget of its own (`TEST_BUDGET`).
fn budget() -> &'static DecodeBudget {
    #[cfg(test)]
    return TEST_BUDGET.get().unwrap_or_else(|| {
        let is_a_test = std::thread::current()
            .name()
            .is_some_and(|n| n.contains("::"));
        let budget = match is_a_test {
            true => own_budget(),
            false => &BUDGET,
        };
        TEST_BUDGET.set(Some(budget));
        budget
    });
    #[cfg(not(test))]
    return &BUDGET;
}

/// Returns a whole budget of its own, for a test's thread. It is kept to the end of the tests.
#[cfg(test)]
fn own_budget() -> &'static DecodeBudget {
    Box::leak(Box::new(DecodeBudget::new(DECODE_BUDGET)))
}

/// Starts a decode job on this thread, or joins the one already running there. While the job
/// lives, what its decodes took from the budget stays taken, because the pictures they made
/// live on until the job has shrunk them. Every public way into decoding starts one.
pub fn start_decode_job() -> DecodeJobBudget {
    let is_outermost = JOB_HOLDS.get().is_none();
    if is_outermost {
        JOB_HOLDS.set(Some(0));
        JOB_NUMBER.set(NEXT_JOB.fetch_add(1, Ordering::Relaxed));
    }
    DecodeJobBudget {
        is_outermost,
        _stays_on_its_thread: std::marker::PhantomData,
    }
}

/// Takes `bytes` of the decodes' shared budget for a decode that is about to allocate them, and
/// waits while other decodes hold too much. Returns an error when `bytes` alone is more than
/// the budget. In a decode job, the job holds the most that one of its decodes asked for,
/// because a decode's buffers are dropped before the next one starts (a scaled JPEG that falls
/// back to a whole one). A job that needs more gives back what it holds before it waits, so two
/// jobs never wait for each other. Outside a job, the reservation starts one, so a thread never
/// waits for the budget while it holds some of it. Before, a second reservation beside the
/// first waited behind a larger one in line, which waited for the first: a deadlock.
///
/// The input that this thread's decode holds (`DecodeInput`) is asked for on top of `bytes`,
/// because it stays in memory while the pixels are made. Such a decode also asks before the
/// others in line, because its input stays in memory while it waits. Behind them, the other
/// threads could each read their input while it waited, and the input would again lie outside
/// the budget.
pub fn reserve_from_budget(bytes: u64) -> Result<BudgetReservation, String> {
    let job = JOB_HOLDS.get().is_none().then(start_decode_job);
    let held = JOB_HOLDS.get().unwrap_or(0);
    let input = INPUT_HOLDS.get();
    let bytes = bytes.saturating_add(input);
    if bytes > held {
        JOB_HOLDS.set(Some(0));
        budget().take(JOB_NUMBER.get(), bytes, GOES_FIRST.get() || input > 0)?;
        JOB_HOLDS.set(Some(bytes));
    }
    Ok(BudgetReservation { _job: job })
}

/// Returns `cap`, or less when the input that this thread's decode holds (`DecodeInput`) leaves
/// less of the budget. A decoder whose own limit is set to it never needs more than the budget
/// can give beside that input. Before, a JPEG XL copied from the file it held asked for the
/// whole budget on top of the file, and Save As refused it as too large.
#[cfg(feature = "jxl")]
fn room_beside_input(cap: u64) -> u64 {
    cap.min(budget().total.saturating_sub(INPUT_HOLDS.get()))
}

/// Returns the bytes of the budget that the render of a JPEG XL of `w`×`h` pixels takes, at
/// most what jxl-oxide's limit lets it allocate (`room_beside_input`).
/// shortcut: 40 bytes a pixel is an estimate above the 24 that a lossy file needs, not what
/// jxl-oxide counts; tie it to jxl-oxide's own limit if a file needs more.
#[cfg(feature = "jxl")]
fn jxl_render_reservation(w: u32, h: u32) -> u64 {
    let render = (u64::from(w) * u64::from(h)).saturating_mul(40);
    render
        .max(64 << 20)
        .min(room_beside_input(4 * MAX_DECODE_BYTES))
}

/// Makes this thread's later decodes take their bytes of the budget before every other decode
/// that waits, or in their turn again. The view's decoders set it for the shown image, so that
/// it does not wait behind the thumbnails and the images read ahead.
pub fn let_this_thread_go_first(goes_first: bool) {
    GOES_FIRST.set(goes_first);
}

pub fn histogram(img: &egui::ColorImage) -> Box<HistogramCounts> {
    let mut h = Box::new([[0; 256]; 5]);
    for p in &img.pixels {
        let [r, g, b, a] = p.to_array();
        h[4][a as usize] += 1;
        h[0][r as usize] += 1;
        h[1][g as usize] += 1;
        h[2][b as usize] += 1;
        // Rec. 709 luma.
        h[3][((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as usize] += 1;
    }
    h
}

/// Decodes image `path` scaled to fit `max`. Returns it with its original size and its number
/// of pages. `page` picks a page of a multi-page TIFF.
pub fn decode(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
    page: usize,
) -> Result<(Frames, [usize; 2], usize), String> {
    let _job = start_decode_job();
    // Also for a file from a collection or the command line, which the listing did not see.
    if !crate::files::file_io::is_surely_file(path) {
        return Err(match crate::files::file_io::is_surely_there(path) {
            true => "not a regular file".into(),
            false => NOT_LOOKED_AT.into(),
        });
    }
    if let Some((frames, orig)) = animation(path, max) {
        return Ok((frames, orig, 1));
    }
    let pages = pages(path);
    let (img, icc, orig) = match scaled_jpeg(path, max, rotate) {
        Some(scaled) => scaled,
        None => {
            // HEIC, AVIF and video frames come from ffmpeg no larger than the box, with their
            // real size from the probe, and an SVG is drawn at the box's size. The other
            // formats come whole and are shrunk here.
            let svg = has_ext(path, crate::model::listing::SVG_EXTS);
            let to_box = (crate::files::external::ffmpeg_decodes(path) || svg).then_some(max);
            let (img, icc, turn, size) = if page > 0 {
                let (img, icc) = page_of(path, page)?;
                (img, icc, Orientation::NoTransforms, None)
            } else {
                load_unturned(path, rotate, to_box)?
            };
            let (img, orig) = shrunk_upright(img, turn, size, max)?;
            (img, icc, orig)
        }
    };
    let img = to_display(shrink(img, max)?, icc.as_deref());
    Ok((vec![(color_image(img), 0)], orig, pages))
}

/// The number of pages of a multi-page TIFF, or of sizes in an ICO, at most `MAX_FRAMES`. It
/// is 1 for other files. Before, an ICO could claim 65,535 sizes, which Export Frames wrote as
/// many files, each after reading the whole directory again.
pub fn pages(path: &Path) -> usize {
    let pages = if has_ext(path, &["tif", "tiff"]) {
        tiff_pages(path)
    } else if has_ext(path, &["ico"]) {
        ico_sizes(path).map_or(1, |sizes| sizes.len().max(1))
    } else {
        1
    };
    pages.min(MAX_FRAMES)
}

/// Page `page` (from 1) of a multi-page TIFF with its ICC profile, or the next size of an ICO.
pub fn page_of<'a>(file: impl ImageBytes<'a>, page: usize) -> Result<Page, String> {
    let _job = start_decode_job();
    match has_ext(file.path(), &["ico"]) {
        true => Ok((ico_size(file, page)?, None)),
        false => tiff_page(file, page),
    }
}

/// Page `page` of `file` whole, with its ICC profile: the image itself turned upright by EXIF
/// if `rotate` for the first page, as the view shows it, and `page_of` for a later one. It is
/// the one way a copy or an exported frame reads a page. Before, a later page of a TIFF was
/// read without its profile, so its copy and exported frame lost their colours.
pub fn load_page<'a>(file: impl ImageBytes<'a>, page: usize, rotate: bool) -> Result<Page, String> {
    let _job = start_decode_job();
    match page {
        0 => load_icc(file, rotate),
        page => page_of(file, page),
    }
}

/// An ICO's images, as their directory entries, in the order they are shown. The first is the
/// one the image crate shows first (most bits per pixel, then the largest, and the last of
/// equals), and the others follow in the same way. It reads only the directory.
fn ico_sizes<'a>(file: impl ImageBytes<'a>) -> Result<Vec<[u8; 16]>, String> {
    // It reads as many entries as the count can say (1 MB at most), because the image crate
    // picks among all of them.
    let head = (file.first_bytes(6 + 16 * u64::from(u16::MAX))).map_err(|e| e.to_string())?;
    let le16 = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    if head.len() < 6 || le16(&head, 0) != 0 || le16(&head, 2) != 1 {
        return Err("not an ICO".into());
    }
    let n = (le16(&head, 4) as usize).min((head.len() - 6) / 16);
    let mut sizes: Vec<(u16, u32, usize, [u8; 16])> = (0..n)
        .map(|k| {
            #[allow(clippy::unwrap_used)] // a slice of 16 bytes always makes an array of 16
            let e: [u8; 16] = head[6 + 16 * k..][..16].try_into().unwrap();
            let side = |b: u8| if b == 0 { 256 } else { b as u32 };
            (le16(&e, 6), side(e[0]) * side(e[1]), k, e)
        })
        .collect();
    sizes.sort_by_key(|&(bits, area, k, _)| std::cmp::Reverse((bits, area, k)));
    Ok(sizes.into_iter().map(|s| s.3).collect())
}

/// Image `k` of an ICO in the order of `ico_sizes`, given to the image crate (which shows only
/// one) as an ICO of that image alone.
fn ico_size<'a>(file: impl ImageBytes<'a>, k: usize) -> Result<image::DynamicImage, String> {
    use std::io::{Read, Seek};
    let mut entry = *ico_sizes(file)?.get(k).ok_or("no such size in the ICO")?;
    let le32 =
        |e: &[u8; 16], at: usize| u32::from_le_bytes([e[at], e[at + 1], e[at + 2], e[at + 3]]);
    let (len, offset) = (le32(&entry, 8), le32(&entry, 12));
    let mut file = file.open_reader().map_err(|e| e.to_string())?;
    let most = u64::from(len).min(1 << 26);
    let input = DecodeInput::reserve(22 + most)?;
    file.seek(std::io::SeekFrom::Start(offset.into()))
        .map_err(|e| e.to_string())?;
    let mut one = vec![0, 0, 1, 0, 1, 0];
    entry[12..].copy_from_slice(&22u32.to_le_bytes());
    one.extend_from_slice(&entry);
    // The length is the file's word, so it reads no more than is there, and no more than
    // 64 MB, which a 256×256 image is far from.
    #[allow(clippy::disallowed_methods)] // bounded by take
    file.take(most)
        .read_to_end(&mut one)
        .map_err(|e| e.to_string())?;
    from_memory(&input.holding(one)?)
}

/// The number of pages (images) in a TIFF file. It reads only their directories.
pub fn tiff_pages(path: &Path) -> usize {
    let Ok(file) = open_regular(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    // Each page's header can point at one shared table, which the tiff crate reads again for
    // every page, so 100,000 pages would be minutes of work on each decode.
    // shortcut: pages after the 1,000th are not shown; count them lazily if such TIFFs turn up.
    let mut pages = 1;
    while pages < 1000 && dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    pages
}

/// Page `page` of a TIFF. The image crate reads only the first page, so the tiff crate (which
/// it uses) decodes the others. It takes 1-, 2-, 4-, 8- and 16-bit grey and 8- and 16-bit
/// colour, which covers faxes and scanned documents. Grey of other kinds, as science writes
/// it (32-bit floats of a height model or a microscope, signed or 32-bit integers), is
/// stretched from its lowest value to its highest.
pub fn tiff_page<'a>(file: impl ImageBytes<'a>, page: usize) -> Result<Page, String> {
    use image::DynamicImage as D;
    use tiff::ColorType as C;
    use tiff::decoder::DecodingResult as R;
    let file = file.open_reader().map_err(|e| e.to_string())?;
    let mut dec = tiff::decoder::Decoder::new(file).map_err(|e| e.to_string())?;
    dec.seek_to_image(page).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let color = dec.colortype().map_err(|e| e.to_string())?;
    // The tiff crate allocates the page whole, as its header claims, so the page must fit first.
    let bytes = u64::from(color.bit_depth()).div_ceil(8) * u64::from(color.num_samples());
    let claimed = (u64::from(w) * u64::from(h)).saturating_mul(bytes);
    if !fits_decode_limit(w, h, bytes) {
        return Err(format!("image too large ({})", human_size(claimed)));
    }
    // The page as read and as changed into 8 bits (`unpacked`, `stretched`) are held at once.
    let _reserved = reserve_from_budget(2 * claimed)?;
    let icc = dec.get_tag_u8_vec(tiff::tags::Tag::IccProfile).ok();
    #[allow(clippy::disallowed_methods)] // the one place, after `fits_decode_limit`
    let data = dec.read_image().map_err(|e| e.to_string())?;
    let img = match (color, data) {
        (C::Gray(8), R::U8(v)) => image::GrayImage::from_raw(w, h, v).map(D::ImageLuma8),
        (C::GrayA(8), R::U8(v)) => image::GrayAlphaImage::from_raw(w, h, v).map(D::ImageLumaA8),
        (C::RGB(8), R::U8(v)) => image::RgbImage::from_raw(w, h, v).map(D::ImageRgb8),
        (C::RGBA(8), R::U8(v)) => image::RgbaImage::from_raw(w, h, v).map(D::ImageRgba8),
        (C::Gray(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageLuma16),
        (C::RGB(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgb16),
        (C::RGBA(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgba16),
        (C::Gray(bits @ (1 | 2 | 4)), R::U8(v)) => {
            image::GrayImage::from_raw(w, h, unpacked(&v, w, bits)).map(D::ImageLuma8)
        }
        (C::Gray(_), v) => {
            stretched(v).and_then(|v| image::GrayImage::from_raw(w, h, v).map(D::ImageLuma8))
        }
        _ => None,
    };
    let img =
        img.ok_or_else(|| format!("page {} is in a colour format Glimt cannot show", page + 1));
    Ok((img?, icc))
}

/// Grey of 1, 2 or 4 bits a pixel, as fax and scanned documents keep it, as 8 bits a pixel.
/// Each row of `packed` starts on a byte. The tiff crate has already turned the values of a
/// WhiteIsZero page, where 1 is black, the other way.
fn unpacked(packed: &[u8], width: u32, bits: u8) -> Vec<u8> {
    let (width, bits) = (width as usize, bits as usize);
    let stride = (width * bits).div_ceil(8).max(1);
    let max = (1u16 << bits) - 1;
    (packed.chunks(stride))
        .flat_map(|row| {
            (0..width).map(move |x| {
                let at = x * bits;
                let byte = row.get(at / 8).copied().unwrap_or(0);
                let value = u16::from(byte >> (8 - bits - at % 8)) & max;
                (value * 255 / max) as u8
            })
        })
        .collect()
}

/// Grey values of any kind as 0 to 255, from the lowest finite value to the highest. Returns
/// `None` for the kinds the arms above take.
// shortcut: lowest to highest, so one stray value can leave the rest dark; stretch between
// percentiles if such files turn up.
fn stretched(values: tiff::decoder::DecodingResult) -> Option<Vec<u8>> {
    use tiff::decoder::DecodingResult as R;
    fn stretch<T: Copy>(v: &[T], f: impl Fn(T) -> f64) -> Vec<u8> {
        let finite = || v.iter().map(|&x| f(x)).filter(|x| x.is_finite());
        let low = finite().fold(f64::INFINITY, f64::min);
        let high = finite().fold(f64::NEG_INFINITY, f64::max);
        let scale = if high > low {
            255.0 / (high - low)
        } else {
            0.0
        };
        v.iter()
            .map(|&x| f(x))
            .map(|x| {
                if x.is_finite() {
                    ((x - low) * scale).round() as u8
                } else {
                    0
                }
            })
            .collect()
    }
    Some(match values {
        R::F32(v) => stretch(&v, f64::from),
        R::F64(v) => stretch(&v, |x| x),
        R::I8(v) => stretch(&v, f64::from),
        R::I16(v) => stretch(&v, f64::from),
        R::I32(v) => stretch(&v, f64::from),
        R::U32(v) => stretch(&v, f64::from),
        R::I64(v) => stretch(&v, |x| x as f64),
        R::U64(v) => stretch(&v, |x| x as f64),
        _ => return None,
    })
}

/// Linear light of any brightness as an sRGB image. It uses Reinhard's global operator, which
/// makes the average brightness middle grey and rolls the highlights off instead of cutting
/// them.
fn tone_mapped(img: image::Rgb32FImage) -> image::DynamicImage {
    let luminance = |p: &image::Rgb<f32>| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let n = (img.pixels().len().max(1)) as f32;
    let log_mean = (img.pixels())
        .map(|p| (1e-4 + luminance(p).max(0.0)).ln())
        .filter(|l| l.is_finite())
        .sum::<f32>()
        / n;
    let exposure = 0.18 / log_mean.exp();
    let encode = |v: f32| {
        let v = v * exposure;
        let v = (v / (1.0 + v)).clamp(0.0, 1.0);
        let srgb = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    };
    let (w, h) = img.dimensions();
    let pixels = img.pixels().flat_map(|p| p.0.map(encode)).collect();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, pixels).unwrap_or_default())
}

/// The frames of a GIF, an animated WebP or an animated PNG (APNG), decoded as they are taken, and
/// its size. Returns `None` for any other file, a still WebP or PNG among them, and an error for
/// an animation too large (`too_large_animation`), whose frames are never made.
#[allow(clippy::disallowed_methods)] // the one place for into_frames, after too_large_animation
fn animated_frames(path: &Path) -> Option<Animated> {
    use image::{AnimationDecoder, ImageDecoder};
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let file = std::io::BufReader::new(open_regular(path).ok()?);
    // The decoders get their own limit on what they allocate (512 MB, as `MAX_DECODE_BYTES`), so a
    // damaged frame header that claims gigabytes is an error, not a failed allocation that ends
    // Glimt.
    let limits = image::Limits::default();
    // Two canvases are held at once (`too_large_animation`), and they are taken from the budget
    // for the decode job that called this, which holds them while it takes the frames.
    let too_large = |(w, h): (u32, u32)| match too_large_animation((w, h)) {
        true => Some(Err("image too large".to_string())),
        false => reserve_from_budget(u64::from(w) * u64::from(h) * 8)
            .err()
            .map(Err),
    };
    Some(match ext.as_str() {
        "gif" => {
            let mut dec = image::codecs::gif::GifDecoder::new(file).ok()?;
            dec.set_limits(limits).ok()?;
            let size = dec.dimensions();
            too_large(size).unwrap_or_else(|| Ok((dec.into_frames(), size)))
        }
        "webp" => {
            let mut dec = image::codecs::webp::WebPDecoder::new(file).ok()?;
            dec.set_limits(limits).ok()?;
            let size = dec.dimensions();
            if !dec.has_animation() {
                return None;
            }
            too_large(size).unwrap_or_else(|| Ok((dec.into_frames(), size)))
        }
        "png" | "apng" => {
            let dec = image::codecs::png::PngDecoder::with_limits(file, limits).ok()?;
            let size = dec.dimensions();
            if !dec.is_apng().ok()? {
                return None;
            }
            match too_large(size) {
                Some(err) => err,
                None => Ok((dec.apng().ok()?.into_frames(), size)),
            }
        }
        _ => return None,
    })
}

/// Whether an animation of this size is too large. Every frame is a whole canvas, made without
/// image's limit, and the decoder keeps the last one while it makes the next, so two of them
/// must fit.
fn too_large_animation((w, h): (u32, u32)) -> bool {
    !fits_decode_limit(w, h, 8)
}

/// Returns how many frames of an animation with a canvas of `w` by `h` are read: at most
/// `MAX_FRAMES`, and fewer of a large canvas, because each frame is drawn on a whole canvas
/// however little of it the frame covers.
fn most_frames((w, h): (u32, u32)) -> usize {
    let canvas = u64::from(w) * u64::from(h) * 4;
    (MAX_ANIMATION_CANVAS_BYTES / canvas.max(1)).clamp(1, MAX_FRAMES as u64) as usize
}

/// The frames of an animated GIF, WebP or PNG (APNG). Returns `None` for other files, which
/// `decode` decodes.
pub fn animation(path: &Path, max: [u32; 2]) -> Option<(Frames, [usize; 2])> {
    let _job = start_decode_job();
    // One too large is shown by `decode` as a still image, instead of asking for gigabytes here.
    let (frames, canvas) = animated_frames(path)?.ok()?;
    let most_frames = most_frames(canvas);
    let (mut out, mut orig, mut bytes) = (Vec::new(), [0, 0], 0);
    for frame in frames {
        // A GIF cut short in a download keeps the frames that came, as browsers show them.
        let Ok(frame) = frame else { break };
        let (n, d) = frame.delay().numer_denom_ms();
        // Browsers show delays under 20 ms as 100 ms, and many GIFs rely on it.
        let ms = Some(n / d.max(1)).filter(|&ms| ms >= 20).unwrap_or(100);
        let img = frame.into_buffer();
        orig = [img.width() as usize, img.height() as usize];
        let img = color_image(to_display(shrink(img.into(), max).ok()?, None));
        bytes += img.pixels.len() * 4;
        out.push((img, ms));
        // Tiny frames pass the memory budget in their thousands, and each becomes a texture
        // when shown, so there are 1,000 at most, and fewer of a large canvas.
        if bytes > MAX_ANIMATION_BYTES.load(Ordering::Relaxed) || out.len() >= most_frames {
            break;
        }
    }
    (!out.is_empty()).then_some((out, orig))
}

/// Gives every frame of an animation, or every page of a TIFF, at full size and in sRGB, one at
/// a time to `each` (its number from 0 and the image), or only frame `only`. Returns how many
/// were given. The first page is turned upright by EXIF if `rotate`, as the view shows it. It
/// is for File → Save This Frame and Export Frames.
pub fn each_frame(
    path: &Path,
    only: Option<usize>,
    rotate: bool,
    mut each: impl FnMut(usize, image::DynamicImage) -> Result<(), String>,
) -> Result<usize, String> {
    let _job = start_decode_job();
    let wanted = |k: usize| only.is_none_or(|o| o == k);
    let mut done = 0;
    // Each frame is decoded once, as it is given, rather than all first to learn that the
    // image is animated. An animation too large for its frames may still have its first frame
    // read alone, as a still image by `load_icc`.
    let animated = animated_frames(path);
    if let Some(Err(e)) = &animated
        && only != Some(0)
    {
        return Err(e.clone());
    }
    if let Some(Ok((frames, canvas))) = animated {
        // Only as many frames are given as the view keeps, so an animation of a million frames,
        // or of a thousand tiny frames on a large canvas, is not hours of work and gigabytes of
        // canvases.
        for (k, frame) in frames.enumerate().take(most_frames(canvas)) {
            if only.is_some_and(|o| k > o) {
                break;
            }
            let frame = frame.map_err(|e| e.to_string())?;
            if wanted(k) {
                each(k, frame.into_buffer().into())?;
                done += 1;
            }
        }
        return Ok(done);
    }
    let pages = pages(path);
    for k in (0..pages).filter(|&k| wanted(k)) {
        let (img, icc) = load_page(path, k, rotate)?;
        each(k, to_srgb(img, icc.as_deref()))?;
        done += 1;
    }
    Ok(done)
}

/// Decodes `path` no larger than `max`, with its ICC profile and its upright size before it was
/// shrunk. A JPEG is decoded at a fraction, and other images whole (HEIC, AVIF and video frames
/// at `max` from ffmpeg) and shrunk after, then turned (`shrunk_upright`), as the view does.
/// This is the one way for thumbnails, the print and its preview.
pub fn decode_shrunk(path: &Path, max: [u32; 2], rotate: bool) -> Result<ShrunkImage, String> {
    let _job = start_decode_job();
    let (img, icc, size) = match scaled_jpeg(path, max, rotate) {
        Some(found) => found,
        None => {
            let (img, icc, turn, size) = load_unturned(path, rotate, Some(max))?;
            let (img, size) = shrunk_upright(img, turn, size, max)?;
            (img, icc, size)
        }
    };
    Ok((shrink(img, max)?, icc, size))
}

/// Returns the ICC profile inside `path`, read by the same decoder the view reads it with, or
/// `None` when it has none. The info panel names it.
pub fn icc_profile(path: &Path) -> Option<Vec<u8>> {
    // A JPEG that the decoder reads whole counts in the budget, as a decode's does.
    let _job = start_decode_job();
    #[cfg(feature = "jxl")]
    if has_ext(path, crate::model::listing::JXL_EXTS) {
        let file = std::io::BufReader::new(open_regular(path).ok()?);
        let mut dec = jxl_oxide::integration::JxlDecoder::new(file).ok()?;
        return dec.icc_profile().ok().flatten();
    }
    if crate::files::external::is_heif(path) {
        return heif_icc(path);
    }
    image_reader(path)
        .ok()?
        .into_decoder()
        .ok()?
        .icc_profile()
        .ok()
        .flatten()
}

/// Returns the ICC profile of a HEIC or AVIF photo, from the first `colr` box of the kind
/// `prof` in its first MB, where the boxes that describe the image are. It is read from the
/// file, because ffmpeg's frame of a photo in tiles loses it where the tiles are put together.
/// Before, an iPhone's Display P3 photo was shown, thumbnailed and saved as if it were sRGB.
/// Returns `None` when there is none, or when what is found is no ICC profile.
// shortcut: the first profile is taken, which is the photo's own in phones' files; find the
// primary item's through iprp/ipma if a file turns up with another before it.
fn heif_icc<'a>(file: impl ImageBytes<'a>) -> Option<Vec<u8>> {
    let head = file.first_bytes(1 << 20).ok()?;
    let at = head.windows(8).position(|w| w == b"colrprof")?;
    let size = u32::from_be_bytes(head.get(at.checked_sub(4)?..at)?.try_into().ok()?) as usize;
    let icc = head.get(at + 8..(at - 4).checked_add(size)?)?;
    (icc.get(36..40) == Some(&b"acsp"[..])).then(|| icc.to_vec())
}

/// The whole image (a preview frame for videos), turned upright by EXIF if `rotate`, and its
/// ICC profile if it has one.
pub fn load_icc<'a>(
    file: impl ImageBytes<'a>,
    rotate: bool,
) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    let _job = start_decode_job();
    let (mut img, icc, turn, _) = load_unturned(file, rotate, None)?;
    // The whole image is wanted (Save As, a copy, a turned save), so it is turned whole.
    img.apply_orientation(turn);
    Ok((img, icc))
}

/// `img` shrunk to fit `max` once it is turned by `turn`, and then turned, with its upright
/// size (`size` where it came smaller than it is). It is shrunk first, because a 100 MP photo
/// with orientation 6 turned whole would need a second 400 MB buffer for a moment. It is the
/// one way the view and the thumbnails turn a decoded image.
fn shrunk_upright(
    img: image::DynamicImage,
    turn: Orientation,
    size: Option<[usize; 2]>,
    max: [u32; 2],
) -> Result<(image::DynamicImage, [usize; 2]), String> {
    let quarter = is_quarter_turn(turn);
    let [w, h] = [img.width() as usize, img.height() as usize];
    let orig = size.unwrap_or(if quarter { [h, w] } else { [w, h] });
    let mut img = shrink(img, if quarter { [max[1], max[0]] } else { max })?;
    img.apply_orientation(turn);
    Ok((img, orig))
}

/// The image of `file` with its ICC profile, and the EXIF turn returned rather than done, so a
/// caller that shrinks it turns the small image, not a copy of the whole one. The size is given
/// only where the image came smaller than it is (ffmpeg's formats and SVG at `max`).
fn load_unturned<'a>(
    file: impl ImageBytes<'a>,
    rotate: bool,
    max: Option<[u32; 2]>,
) -> Result<Unturned, String> {
    let path = file.path();
    let upright = Orientation::NoTransforms;
    // Only ffmpeg's formats and SVG take `max`, so without them it has no use.
    #[cfg(not(any(feature = "video", feature = "heic", feature = "avif", feature = "svg")))]
    let _ = max;
    #[cfg(feature = "video")]
    if crate::files::external::is_video(path) {
        let (img, size) = crate::images::video::preview(path, max)?;
        return Ok((img, None, upright, size));
    }
    if has_ext(path, raw::EXTS) {
        let (img, turn) = file.with_counted(raw::unturned)?;
        return Ok((img, None, if rotate { turn } else { upright }, None));
    }
    #[cfg(any(feature = "heic", feature = "avif"))]
    if crate::files::external::is_heif(path) {
        let (img, size) = crate::files::external::decode_heif(path, max)?;
        return Ok((img, heif_icc(file), upright, size));
    }
    #[cfg(feature = "svg")]
    if has_ext(path, crate::model::listing::SVG_EXTS) {
        let (img, size) = crate::images::svg::load(path, max)?;
        return Ok((img, None, upright, size));
    }
    #[cfg(feature = "jxl")]
    if has_ext(path, crate::model::listing::JXL_EXTS) {
        let reader = file.open_reader().map_err(|e| e.to_string())?;
        let mut dec = jxl_oxide::integration::JxlDecoder::new(reader).map_err(|e| e.to_string())?;
        // jxl-oxide starts with no limit on what it allocates. A lossy JPEG XL needs some 24
        // bytes a pixel while it renders, so a camera photo of 24 MP takes about 600 MB. Four
        // times a decode's limit lets such photos through and stops a render of some 90 MP or
        // more. A header that claims a larger picture than a decode may hold is refused before
        // that, by its decode (`ImageBytes::decode`).
        // The file that a copy holds (`Source`) leaves less of the budget for the render, so
        // jxl-oxide's limit is lowered by it too.
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(room_beside_input(4 * MAX_DECODE_BYTES));
        dec.set_limits(limits).map_err(|e| e.to_string())?;
        // The render is taken from the budget before it starts, for the job that called this.
        let (w, h) = dec.dimensions();
        let _reserved = reserve_from_budget(jxl_render_reservation(w, h))?;
        // jxl-oxide already renders the image upright.
        let icc = dec.icc_profile().ok().flatten();
        let img = file.decode(dec)?;
        return Ok((img, icc, upright, None));
    }
    if let Some(added) = crate::images::added_formats::for_path(path) {
        return Ok((added.decode(path)?, None, upright, None));
    }
    // The image crate holds a JPEG's bytes while it decodes, and they count in this job beside
    // its pixels: `image_reader` counts a file's, and a `Source` is counted already.
    let reader = file.image_reader().map_err(|e| e.to_string())?;
    let dec = reader.into_decoder().map_err(|e| e.to_string());
    // Grey TIFFs that the image crate does not take (floats, signed) go through the tiff crate.
    let mut dec = match dec {
        Err(e) if has_ext(path, crate::model::listing::TIFF_LIKE) => {
            return tiff_page(file, 0)
                .map(|(img, icc)| (img, icc, upright, None))
                .map_err(|_| e);
        }
        dec => dec?,
    };
    // One EXIF block, as the info panel reads it, gives both the turn and the colour space.
    // A block that cannot be read shows the image as stored, unturned.
    let exif = exif::block_from_decoder(file, &mut dec).ok().flatten();
    let turn = match rotate {
        true => exif::stored_orientation(&mut dec, exif.as_deref()),
        false => upright,
    };
    // Without a profile, the EXIF may say Adobe RGB (a camera set to it).
    let icc = dec.icc_profile().ok().flatten().or_else(|| {
        let adobe = exif::is_adobe_rgb(exif.as_deref()?);
        adobe.then(|| ADOBE_RGB.clone())
    });
    let mut img = file.decode(dec)?;
    // Radiance HDR holds light as it was, far brighter than a screen, so it is brought into the
    // screen's range.
    if has_ext(path, &["hdr"]) {
        img = tone_mapped(img.into_rgb32f());
    }
    Ok((img, icc, turn, None))
}

/// A JPEG, or the preview in a RAW file, decoded at 1/2, 1/4 or 1/8 of its size when that still
/// covers `max`. It is turned upright by EXIF if `rotate`, and comes with its ICC profile (or
/// Adobe RGB from EXIF) and its full upright size. Returns `None` where the whole image is
/// needed or jpeg-decoder cannot help (CMYK, an error), and then it is decoded whole, as other
/// images are.
pub fn scaled_jpeg(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
) -> Option<(image::DynamicImage, Option<Vec<u8>>, [usize; 2])> {
    if crate::model::listing::is_jpeg(path) {
        return scaled(&DecodeInput::read(path).ok()?, max, rotate, None);
    }
    if has_ext(path, raw::EXTS) {
        // As in raw::load, the orientation comes from a TIFF-based RAW's own header, and there
        // is no profile.
        let from = |data: &[u8]| {
            let (img, _, orig) = scaled(raw::preview(data)?, max, rotate, exif::orientation(data))?;
            Some((img, None, orig))
        };
        // The first 8 MB are read first. Cameras put the preview before the sensor data, so a
        // folder of 50 MB RAW files on a share is not read whole for its thumbnails.
        let head = DecodeInput::read_head(path, 8 << 20).ok()?;
        let covers = |p: &[u8]| raw::jpeg_size(p).is_some_and(|(w, h)| w >= max[0] || h >= max[1]);
        if raw::preview(&head).is_some_and(|p| covers(p) && raw::is_whole_jpeg(p)) {
            if let Some(shown) = from(&head[..]) {
                return Some(shown);
            }
            // When it cannot be decoded at a fraction (shown larger than half the preview), it
            // is decoded whole from the head, which holds all of it, rather than from the whole
            // file read after all.
            if let Ok(img) = raw::load_data(&head, rotate) {
                let orig = [img.width() as usize, img.height() as usize];
                return Some((img, None, orig));
            }
        }
        drop(head);
        let data = DecodeInput::read(path).ok()?;
        // When it cannot be decoded at a fraction (shown larger than half the preview), it is
        // decoded whole from what was read, rather than read a second time.
        return from(&data[..]).or_else(|| {
            let img = raw::load_data(&data, rotate).ok()?;
            let orig = [img.width() as usize, img.height() as usize];
            Some((img, None, orig))
        });
    }
    None
}

/// The first `max` bytes of `path`, if it is a file (`open_regular_with_metadata`).
pub(crate) fn read_head(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut head = Vec::new();
    #[allow(clippy::disallowed_methods)] // bounded by take
    open_regular_with_metadata(path)?
        .0
        .take(max)
        .read_to_end(&mut head)?;
    Ok(head)
}

/// `scaled_jpeg` of the JPEG in `data`. `orientation` overrides the one in its EXIF.
pub fn scaled(
    data: &[u8],
    max: [u32; 2],
    rotate: bool,
    orientation: Option<image::metadata::Orientation>,
) -> Option<(image::DynamicImage, Option<Vec<u8>>, [usize; 2])> {
    use jpeg_decoder::PixelFormat;
    is_counted_input(data).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    dec.read_info().ok()?;
    let info = dec.info()?;
    if !matches!(info.pixel_format, PixelFormat::RGB24 | PixelFormat::L8) {
        return None;
    }
    // jpeg-decoder has no limit of its own. A progressive JPEG keeps a coefficient (2 bytes) of
    // every pixel and channel at full size while it decodes, so a file of a few KB that claims
    // 65535×65535 would ask for 25 GB. Any JPEG that does not fit goes to image's decoder,
    // which refuses it.
    let bytes = info.pixel_format.pixel_bytes() as u64;
    let progressive = info.coding_process == jpeg_decoder::CodingProcess::DctProgressive;
    if progressive && !fits_decode_limit(info.width.into(), info.height.into(), 2 * bytes) {
        return None;
    }
    let orientation = (orientation.or_else(|| dec.exif_data().and_then(exif::orientation)))
        .filter(|_| rotate)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let (w, h) = (info.width as f64, info.height as f64);
    // Upright size, which is what has to fit.
    let (uw, uh) = if is_quarter_turn(orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let scale = (max[0] as f64 / uw).min(max[1] as f64 / uh);
    if scale > 0.5 {
        return None;
    }
    // jpeg-decoder takes the smallest of 1/8, 1/4, 1/2 and 1/1 at least this large.
    let (want_w, want_h) = ((w * scale).ceil() as u16, (h * scale).ceil() as u16);
    let (sw, sh) = dec.scale(want_w, want_h).ok()?;
    // A baseline JPEG is decoded at the scaled size, into a plane for each channel and then the
    // pixels, so twice the pixels' bytes. 40000×40000 shown at 1/2 would take 2.4 GB.
    if !fits_decode_limit(sw.into(), sh.into(), 2 * bytes) {
        return None;
    }
    // A progressive JPEG's coefficients at full size are held beside the scaled planes.
    let full = u64::from(info.width) * u64::from(info.height) * 2 * bytes;
    let scaled_planes = u64::from(sw) * u64::from(sh) * 2 * bytes;
    let _reserved = reserve_from_budget(scaled_planes + if progressive { full } else { 0 }).ok()?;
    #[allow(clippy::disallowed_methods)] // the one place, after `fits_decode_limit`
    let pixels = dec.decode().ok()?;
    let (sw, sh) = (sw as u32, sh as u32);
    let mut img: image::DynamicImage = match info.pixel_format {
        PixelFormat::L8 => image::GrayImage::from_raw(sw, sh, pixels)?.into(),
        _ => image::RgbImage::from_raw(sw, sh, pixels)?.into(),
    };
    img.apply_orientation(orientation);
    let icc = dec.icc_profile().or_else(|| {
        let exif = dec.exif_data()?;
        exif::is_adobe_rgb(exif).then(|| ADOBE_RGB.clone())
    });
    Some((img, icc, [uw as usize, uh as usize]))
}

/// Makes `mb` megabytes the most an animation's decoded frames may take, from the setting
/// `animation_mb`, at start and when it changes in Preferences.
pub fn set_animation_limit(mb: usize) {
    #[cfg(test)]
    crate::support::test_support::shared_setting_set(
        MAX_ANIMATION_BYTES.load(Ordering::Relaxed) != mb << 20,
    );
    MAX_ANIMATION_BYTES.store(mb << 20, Ordering::Relaxed);
}

/// Returns whether a picture of `w`×`h` with `bytes` bytes for each pixel fits in what one decode
/// may allocate (`MAX_DECODE_BYTES`). Every decoder asks it before it allocates from a size the
/// file claims, so a header of a few bytes that claims gigabytes is refused, not allocated.
pub fn fits_decode_limit(w: u32, h: u32, bytes: u64) -> bool {
    (u64::from(w) * u64::from(h)).saturating_mul(bytes) <= MAX_DECODE_BYTES
}

/// Decodes the image `dec` reads, after the size limit that `ImageReader` applies. The image
/// crate's own `from_decoder` allocates whatever the header claims, so a few bytes that claim
/// a huge image would make Glimt allocate gigabytes, which aborts the process. This is the one
/// way a decoder made directly is decoded; clippy refuses the image crate's.
///
/// A decoder over bytes in memory (a `Cursor`) is refused, because only `from_memory`, which
/// is given the bytes, can tell that the budget counts all of them. A RAW's preview decoded from
/// a `Vec` that `read_limited` gave outside a decode job passed before, and later a decoder
/// over more bytes than this thread had counted.
pub fn from_decoder<D: ImageDecoder>(dec: D) -> Result<image::DynamicImage, String> {
    // shortcut: the reader is told by the decoder's type name, which Rust does not promise to
    // keep; `whole_reads_of_images_are_counted` fails if it changes.
    if std::any::type_name::<D>().contains("Cursor<") {
        return Err(NOT_COUNTED.into());
    }
    decode_within_limits(dec)
}

/// Decodes the image `dec` reads, after `fits_decode_limit`, with its pixels taken from the
/// budget. `from_decoder` and `from_memory` call it once they know where its bytes lie.
fn decode_within_limits<D: ImageDecoder>(dec: D) -> Result<image::DynamicImage, String> {
    let (w, h) = dec.dimensions();
    if !fits_decode_limit(w, h, dec.color_type().bytes_per_pixel().into()) {
        return Err(format!(
            "image too large ({})",
            human_size(dec.total_bytes())
        ));
    }
    let _reserved = reserve_from_budget(dec.total_bytes())?;
    #[allow(clippy::disallowed_methods)] // the one place, after `fits_decode_limit`
    image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())
}

/// Decodes an image held in memory, of the format its bytes tell, after `fits_decode_limit`. It is
/// the one way bytes are decoded (an added format's or ffmpeg's output, an ICO's image, a cached
/// thumbnail). Clippy refuses the image crate's `load_from_memory`. The bytes must lie in a
/// `DecodeInput`, so that the budget counts them (`is_counted_input`).
pub fn from_memory(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    is_counted_input(bytes)?;
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format();
    decode_within_limits(
        reader
            .map_err(|e| e.to_string())?
            .into_decoder()
            .map_err(|e| e.to_string())?,
    )
}

/// Returns an error unless `bytes` lie in a `DecodeInput` of this thread, whose bytes the
/// budget counts beside the pixels made of them. The decoders of bytes ask it, so that a new
/// way in that reads its input outside the budget fails in the tests.
fn is_counted_input(bytes: &[u8]) -> Result<(), String> {
    match bytes.len() as u64 <= INPUT_HOLDS.get() {
        true => Ok(()),
        false => Err(NOT_COUNTED.into()),
    }
}

/// Takes `bytes` that are about to be read whole from `path` from the budget, as input of this
/// thread's decode job, which counts them until it ends. Returns whether they were counted.
/// Outside a decode job, a whole read of an image's file is refused, so that a pass over many
/// files (the sort by date, the "exif:" search, Copy for Sharing) never holds one outside the
/// budget. Before, only the decodes counted what they read, and a RAW or HEIC whose EXIF was
/// not near its start was read whole, up to 512 MB, in each thread of the EXIF pass.
fn count_whole_read(path: &Path, bytes: u64) -> std::io::Result<bool> {
    if JOB_HOLDS.get().is_none() {
        return match crate::model::listing::built_in_format(path) {
            true => Err(std::io::Error::other(NOT_COUNTED)),
            false => Ok(false),
        };
    }
    reserve_from_budget(bytes).map_err(std::io::Error::other)?;
    INPUT_HOLDS.set(INPUT_HOLDS.get() + bytes);
    JOB_INPUT.set(JOB_INPUT.get() + bytes);
    Ok(true)
}

/// Decodes an image built into Glimt itself, its logo, on the window's thread. It takes nothing
/// from the decodes' budget, because a large decode that holds the budget would keep the window
/// waiting, and the image's size is known.
pub fn decode_glimts_own_image(bytes: &'static [u8]) -> Result<image::DynamicImage, String> {
    #[allow(clippy::disallowed_methods)] // Glimt's own image, whose size is known
    image::load_from_memory(bytes).map_err(|e| e.to_string())
}

/// Checks that Glimt can decode `path`, for Check Images. The file is decoded small, as for the
/// list's thumbnails. A JPEG is also looked at for its end marker, which a file cut off in copying
/// lacks, though it may still decode with grey at the bottom. Returns why not, if it cannot.
pub fn check_decodes(path: &Path) -> Result<(), String> {
    catch_panic_as_error(|| decode(path, [64, 64], false, 0).map(drop))?;
    if crate::model::listing::is_jpeg(path) && !is_whole_jpeg_file(path)? {
        return Err("cut off: the JPEG has no end".into());
    }
    Ok(())
}

/// Returns what the failure of a decode of `path` with `error` says about the file. This is the
/// one rule for it, which Check Images and the thumbnails' failure marks follow. A program
/// that is not there may be installed later. A file that could not be read says so in the
/// system's words, "(os error N)", which a file that is no picture never gives, and a program
/// that read from a share that failed says it in its own words.
pub fn failure_of(path: &Path, error: &str) -> DecodeFailure {
    if error.contains(crate::files::external::TIMED_OUT) {
        return DecodeFailure::TimedOut;
    }
    let away = [
        "Input/output error",
        "Transport endpoint is not connected",
        "Host is down",
        NOT_LOOKED_AT,
        CHANGED_WHILE_READ,
        NOT_COUNTED,
    ];
    if (needs_program(path) && program_missing(error))
        || error.contains("(os error")
        || away.iter().any(|m| error.contains(m))
    {
        return DecodeFailure::NotOfTheFile;
    }
    // An added format's error that it ran out of memory is told in the user's language, with
    // the program's own words after it.
    let added_format = crate::images::added_formats::for_path(path).is_some();
    let too_large = ["too large", "too much memory"]
        .iter()
        .any(|m| error.contains(m))
        || (added_format && crate::images::added_formats::ran_out_of_memory(error));
    match too_large {
        true => DecodeFailure::TooLarge,
        false => DecodeFailure::OfTheFile,
    }
}

/// Returns whether `path` is read by another program: ffmpeg (HEIC, AVIF, video) or an added
/// format.
fn needs_program(path: &Path) -> bool {
    crate::files::external::ffmpeg_decodes(path)
        || crate::images::added_formats::for_path(path).is_some()
}

/// Returns whether `error` says that the program, or a part it needs, was not found or may not
/// run, not that the file is bad. A shell says it in its own words: dash and busybox write
/// "exec: convert: not found", bash "command not found". A Python script without a module it
/// needs, as the examples' scripts are, says "ModuleNotFoundError" on its last line.
pub(crate) fn program_missing(error: &str) -> bool {
    let missing = [
        "No such file or directory",
        "command not found",
        "Permission denied",
        "cannot execute",
        "ModuleNotFoundError",
    ];
    missing.iter().any(|m| error.contains(m)) || error.trim_end().ends_with(": not found")
}

/// Returns whether a JPEG file's image data reaches its end marker (FF D9), which the data itself
/// never holds (an FF in it is followed by 00 or a restart marker). The marker is looked for from
/// the image data on, not in the last bytes, because phones put a video of megabytes after it in a
/// motion photo.
// shortcut: reads the whole file, as Check Images does each JPEG once; read from the image
// data's start in blocks if motion photos of hundreds of MB make it slow.
fn is_whole_jpeg_file(path: &Path) -> Result<bool, String> {
    let data = DecodeInput::read(path).map_err(|e| e.to_string())?;
    Ok(crate::images::raw::is_whole_jpeg(&data))
}

/// Returns the bytes of the budget that this thread's decode job holds, or `None` outside a
/// job, for the tests of other modules.
#[cfg(test)]
pub(crate) fn held_by_this_job() -> Option<u64> {
    JOB_HOLDS.get()
}

/// Returns a function that gives the thread it is called on this thread's budget, for a test
/// of another module whose threads must share its test's budget.
#[cfg(test)]
pub(crate) fn share_this_threads_budget() -> impl Fn() + Send + Clone + 'static {
    let budget = budget();
    move || TEST_BUDGET.set(Some(budget))
}

/// Returns the bytes free in this thread's budget, for a test of another module.
#[cfg(test)]
pub(crate) fn free_in_this_threads_budget() -> u64 {
    lock_even_if_poisoned(&budget().state).free
}

/// Returns whether the EXIF orientation turns the image a quarter, so its width and height swap. It
/// is the one list of those orientations, for the view, the JPEG scaling and the save.
pub fn is_quarter_turn(o: Orientation) -> bool {
    use Orientation::{Rotate90, Rotate90FlipH, Rotate270, Rotate270FlipH};
    matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
}

/// Runs `work`, and returns an error if it panics. A decoder crashing on a broken file must not
/// end the worker thread, because that would leave every later image or thumbnail waiting for
/// ever.
pub fn catch_panic_as_error<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    crate::support::threads::catch_panic(work)
        .unwrap_or_else(|| Err("the decoder crashed on this file".into()))
}

/// As `read_limited`, as UTF-8 text.
pub fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    String::from_utf8(read_limited(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The whole of `path`, read only up to `MAX_DECODE_BYTES` bytes. A larger file (a sparse one, or
/// one unpacked from a small zip) is refused before it fills memory, rather than read whole first.
/// So is what is not a file (a pipe would block for good, and /dev/zero would fill memory), also
/// through a link. This is the one way Glimt reads a file whole, whoever wrote it, and clippy
/// (clippy.toml) refuses every other read.
///
/// In a decode job the bytes are counted in the decodes' budget until the job ends, and an
/// image's file is read only in one (`count_whole_read`). A file that grows past what was
/// counted while it is read is refused then.
pub fn read_limited(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    let path = path.as_ref();
    let (file, meta) = open_regular_with_metadata(path)?;
    let length = meta.len();
    if length > MAX_DECODE_BYTES {
        return Err(file_too_large());
    }
    let counted = count_whole_read(path, length)?;
    let data = read_open_file((file, meta))?;
    if counted && data.len() as u64 > length {
        return Err(std::io::Error::other(CHANGED_WHILE_READ));
    }
    Ok(data)
}

/// `read_limited` of `path` when it is no link. Returns an error whose `raw_os_error` is
/// `ELOOP` when it is one. The open itself refuses the link (O_NOFOLLOW), so a link swapped in
/// after a look at the name is never read through.
pub(crate) fn read_unlinked(path: &Path) -> std::io::Result<Vec<u8>> {
    read_open_file(open_regular_with_flags(path, libc::O_NOFOLLOW)?)
}

/// The whole of a file opened by `open_regular_with_metadata`, up to `MAX_DECODE_BYTES` bytes.
fn read_open_file((file, meta): (std::fs::File, std::fs::Metadata)) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    // The size is looked at first, so a large file is not read at all. `take` is for a file
    // that grows meanwhile.
    if meta.len() > MAX_DECODE_BYTES {
        return Err(file_too_large());
    }
    let mut data = Vec::new();
    #[allow(clippy::disallowed_methods)] // bounded by take
    file.take(MAX_DECODE_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_DECODE_BYTES {
        return Err(file_too_large());
    }
    Ok(data)
}

/// Reads from `reader` into `line` up to and with the next line break, but at most `most`
/// bytes. Returns how many bytes were read, 0 at the end. This is the one way Glimt reads a
/// line, because a socket or a program that never sends a line break would otherwise fill the
/// memory, and clippy (clippy.toml) refuses `read_line`, `read_until`, `lines` and `split`.
pub(crate) fn read_line_limited(
    reader: &mut impl std::io::BufRead,
    most: u64,
    line: &mut Vec<u8>,
) -> std::io::Result<usize> {
    read_until_limited(reader, most, b'\n', line)
}

/// Reads from `reader` into `part` up to and with the next byte `end`, but at most `most` bytes,
/// as `read_line_limited` reads a line. Returns how many bytes were read, 0 at the end. A job
/// log's fields each end in a NUL byte, and are read this way.
pub(crate) fn read_until_limited(
    reader: &mut impl std::io::BufRead,
    most: u64,
    end: u8,
    part: &mut Vec<u8>,
) -> std::io::Result<usize> {
    use std::io::BufRead;
    #[allow(clippy::disallowed_methods)] // bounded by take
    std::io::Read::take(reader, most).read_until(end, part)
}

/// Copies from `from` to `to` until its end, but at most `most` bytes. Returns how many bytes
/// were copied. This is the one way Glimt copies from a reader, because clippy (clippy.toml)
/// refuses `io::copy`, which into a `Vec` fills the memory from a reader with no end.
pub(crate) fn copy_limited<W: std::io::Write + ?Sized>(
    from: &mut impl std::io::Read,
    to: &mut W,
    most: u64,
) -> std::io::Result<u64> {
    #[allow(clippy::disallowed_methods)] // bounded by take
    std::io::copy(&mut std::io::Read::take(from, most), to)
}

/// The error of a file larger than a decode may read (`MAX_DECODE_BYTES`). Its kind is
/// `FileTooLarge`, by which a reader of metadata tells a file refused only by its size from one
/// that could not be read.
fn file_too_large() -> std::io::Error {
    let text = format!("file too large ({})", human_size(MAX_DECODE_BYTES));
    std::io::Error::new(std::io::ErrorKind::FileTooLarge, text)
}

/// Opens `path` to be read, if it is a regular file, and returns it with its metadata. It is opened
/// without waiting (O_NONBLOCK, which a file does not heed), and what is open is looked at rather
/// than the name, because a pipe put there between a look and an open would block that open.
pub(crate) fn open_regular_with_metadata(
    path: &Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    open_regular_with_flags(path, 0)
}

/// `open_regular_with_metadata` with the open's `flags` added.
fn open_regular_with_flags(
    path: &Path,
    flags: i32,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt;
    #[allow(clippy::disallowed_methods)] // the one opening of a file to read it whole
    let file = (std::fs::OpenOptions::new().read(true))
        .custom_flags(libc::O_NONBLOCK | flags)
        .open(path)?;
    #[cfg(test)]
    OPENS.with_borrow_mut(|opens| {
        if let Some((counted, n)) = opens
            && counted == path
        {
            *n += 1;
        }
    });
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

/// Opens `path` to be read, if it is a regular file (`open_regular_with_metadata`). This is the one
/// way Glimt opens a file, so a pipe or device in its place is refused, not waited on for good.
pub(crate) fn open_regular(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    open_regular_with_metadata(path.as_ref()).map(|(file, _)| file)
}

/// Returns an image's width and height as stored, read from its header where it can be: a
/// JPEG's from its first part, a camera RAW's from the preview it is shown by (its own header
/// gives the size of a small thumbnail), others from the decoder's header. An EXIF orientation
/// is not applied, so a photo turned upright reports the size the camera wrote. Returns `None`
/// for a file whose size cannot be read without decoding it (HEIC, AVIF, SVG, video).
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    // A file that is read whole for its size counts in the budget, as a decode's does.
    let _job = start_decode_job();
    if crate::model::listing::is_jpeg(path)
        && let Some(size) = jpeg_head_size(path)
    {
        return Some(size);
    }
    if has_ext(path, raw::EXTS) {
        // The head first, as for a thumbnail. Find Duplicates over a folder of 50 MB RAWs then
        // reads the whole of one only when its first part has no preview.
        if let Some(p) = raw::preview(&read_head(path, 8 << 20).ok()?) {
            return raw::jpeg_size(p);
        }
        let data = DecodeInput::read(path).ok()?;
        return raw::jpeg_size(raw::preview(&data)?);
    }
    image_reader(path).ok()?.into_dimensions().ok()
}

/// Returns a JPEG's width and height from its first 256 KB, where its frame header is, because
/// the decoder would read the whole file. Returns `None` if it is not there, so the decoder is
/// asked after all.
fn jpeg_head_size(path: &Path) -> Option<(u32, u32)> {
    let head = read_head(path, 1 << 18).ok()?;
    head.starts_with(&[0xff, 0xd8])
        .then(|| raw::jpeg_size(&head))?
}

/// An image crate reader on `path`, opened with `open_regular` (no pipe or device). Its format is
/// guessed from the file's first bytes, and from the extension when they say nothing, as
/// `image::ImageReader::open` and `with_guessed_format` do. Clippy refuses `open`, because the
/// image crate opens the file itself, where clippy cannot look.
///
/// The image crate reads a JPEG whole before it decodes it, so a JPEG's length is counted as
/// a read of the whole file (`count_whole_read`): in a decode job only, until the job ends.
pub(crate) fn image_reader(
    path: impl AsRef<Path>,
) -> std::io::Result<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    let path = path.as_ref();
    let (reader, whole_read) = image_reader_and_whole_read(path)?;
    if whole_read > 0 {
        count_whole_read(path, whole_read)?;
    }
    Ok(reader)
}

/// Returns the format of the image at `path`, as `image_reader` guesses it, without decoding
/// it. Nothing is read whole, so nothing is counted in the budget.
pub(crate) fn image_format(path: &Path) -> std::io::Result<Option<image::ImageFormat>> {
    image_reader_and_whole_read(path).map(|(reader, _)| reader.format())
}

/// `image_reader` of `path`, and the bytes that the image crate reads whole before it decodes
/// (the file's length for a JPEG, else 0), which a decode counts as its input.
fn image_reader_and_whole_read(
    path: &Path,
) -> std::io::Result<(image::ImageReader<std::io::BufReader<std::fs::File>>, u64)> {
    let (file, meta) = open_regular_with_metadata(path)?;
    let reader = image::ImageReader::new(std::io::BufReader::new(file));
    let reader = with_format_guessed(reader, path)?;
    // The image crate's JPEG decoder reads the whole file before any of its limits apply, so a
    // JPEG larger than a decode may take is refused here, as read_limited refuses it. The format
    // counts as the bytes tell it, so a JPEG named .png is refused too. Other formats are read by
    // seeking, so a large TIFF or PNG still opens. A file of no known format is refused as well.
    let is_jpeg = reader.format() == Some(image::ImageFormat::Jpeg);
    if (is_jpeg || reader.format().is_none()) && meta.len() > MAX_DECODE_BYTES {
        return Err(file_too_large());
    }
    // A file of no known format is never read: its decoder fails first. Before, its length was
    // counted, up to 512 MB of the budget waited for and given back for nothing.
    Ok((reader, if is_jpeg { meta.len() } else { 0 }))
}

/// Gives `reader`, of the image at `path`, the format that the bytes tell, or the extension
/// when they tell nothing, as `image::ImageReader::open` and `with_guessed_format` do.
fn with_format_guessed<R: std::io::BufRead + std::io::Seek>(
    mut reader: image::ImageReader<R>,
    path: &Path,
) -> std::io::Result<image::ImageReader<R>> {
    if let Ok(format) = image::ImageFormat::from_path(path) {
        reader.set_format(format);
    }
    reader.with_guessed_format()
}

/// Scale down to fit within `max`.
pub fn shrink(img: image::DynamicImage, max: [u32; 2]) -> Result<image::DynamicImage, String> {
    if img.width() > max[0] || img.height() > max[1] {
        let scale = (max[0] as f64 / img.width() as f64).min(max[1] as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).max(1);
        let h = ((img.height() as f64 * scale).round() as u32).max(1);
        let mut small = image::DynamicImage::new(w, h, img.color());
        // This uses SIMD downscaling, because image's thumbnail took about 150–330 ms on 12 MP.
        let opts = fast_image_resize::ResizeOptions::new().resize_alg(
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear),
        );
        fast_image_resize::Resizer::new()
            .resize(&img, &mut small, &opts)
            .map_err(|e| e.to_string())?;
        return Ok(small);
    }
    Ok(img)
}

pub fn color_image(img: image::DynamicImage) -> egui::ColorImage {
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

#[cfg(test)]
mod tests {
    use crate::support::threads::lock_even_if_poisoned;
    /// Two decodes that each need 60 % of the budget run one after the other, never together,
    /// while small decodes go through at once beside a large one that runs, so a folder of
    /// ordinary images does not wait. One larger than the whole budget is refused rather than
    /// waiting.
    #[test]
    fn decodes_share_one_budget() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let budget = Arc::new(super::DecodeBudget::new(1000));
        let (running, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let start_large = |job| {
            let (budget, running, most) = (budget.clone(), running.clone(), most.clone());
            std::thread::spawn(move || {
                budget.take(job, 600, false).unwrap();
                most.fetch_max(running.fetch_add(1, SeqCst) + 1, SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                running.fetch_sub(1, SeqCst);
                budget.give_back(job);
            })
        };
        let first = start_large(1);
        std::thread::sleep(Duration::from_millis(20));
        // One large decode holds 600 now. Thirty small ones of 10 still pass without waiting.
        let (done, small_done) = mpsc::channel();
        let small = budget.clone();
        std::thread::spawn(move || {
            for _ in 0..30 {
                small.take(3, 10, false).unwrap();
                small.give_back(3);
            }
            done.send(()).unwrap();
        });
        assert!(small_done.recv_timeout(Duration::from_millis(250)).is_ok());
        let second = start_large(2);
        for thread in [first, second] {
            thread.join().unwrap();
        }
        assert_eq!(most.load(SeqCst), 1);
        assert!(budget.take(4, 1001, false).is_err());
        assert_eq!(lock_even_if_poisoned(&budget.state).free, 1000);
    }

    /// A decode that waits for room is not passed by a smaller one that asks after it, and a
    /// decode of the shown image goes before both. Before, any decode that fit took its bytes,
    /// so a large one waited as long as smaller ones kept coming, and the shown image waited
    /// behind the thumbnails.
    #[test]
    fn decodes_take_their_bytes_in_turn() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;
        let budget = Arc::new(super::DecodeBudget::new(1000));
        budget.take(0, 600, false).unwrap();
        let taken = Arc::new(Mutex::new(Vec::new()));
        let start = |name: &'static str, bytes: u64, goes_first: bool| {
            let (budget, taken) = (budget.clone(), taken.clone());
            let thread = std::thread::spawn(move || {
                budget.take(bytes, bytes, goes_first).unwrap();
                lock_even_if_poisoned(&taken).push(name);
            });
            std::thread::sleep(Duration::from_millis(50));
            thread
        };
        let threads = [
            start("large", 600, false),
            start("small", 10, false),
            start("shown", 500, true),
        ];
        // 400 are free: the small decode would fit, but it waits behind the large one.
        assert!(lock_even_if_poisoned(&taken).is_empty());
        budget.give_back(0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*lock_even_if_poisoned(&taken), ["shown"]);
        budget.give_back(500);
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(lock_even_if_poisoned(&taken).len(), 3);
        assert_eq!(lock_even_if_poisoned(&budget.state).free, 1000 - 610);
    }

    /// Glimt's logo is decoded while other decodes hold the whole budget, so About does not
    /// wait for them on the window's thread, as it did.
    #[test]
    fn glimts_own_image_takes_nothing_from_the_budget() {
        let whole = super::reserve_from_budget(super::DECODE_BUDGET).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // The decode runs in a thread of its own, with the budget this test holds whole.
        let held = super::budget();
        std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(held));
            let logo = include_bytes!("../../assets/glimt-256.png");
            tx.send(super::decode_glimts_own_image(logo).is_ok())
        });
        let decoded = rx.recv_timeout(std::time::Duration::from_secs(2));
        drop(whole);
        assert_eq!(decoded, Ok(true));
    }

    /// Two tests never wait for each other's decodes, because each test's thread has a budget
    /// of its own. Before, the tests shared Glimt's one budget, and a test that asked for the
    /// whole of it waited first in line while an SVG test's drawing child held a quarter for
    /// seconds. Every decode of the other tests waited behind it, and the file operations'
    /// contract timed out now and then on "save frame over b".
    #[test]
    fn tests_never_wait_for_each_others_decode_budget() {
        let whole = super::reserve_from_budget(super::DECODE_BUDGET).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let other_test = std::thread::Builder::new().name("other::tests::decodes".into());
        other_test
            .spawn(move || tx.send(super::reserve_from_budget(1).is_ok()))
            .unwrap();
        let taken = rx.recv_timeout(std::time::Duration::from_secs(2));
        drop(whole);
        assert_eq!(
            taken,
            Ok(true),
            "a test waited for another test's decode budget"
        );
    }

    /// A second reservation outside a decode job, while the first is held, does not wait for
    /// the first: together they need more than the budget. Before, it waited for good.
    #[test]
    fn a_reservation_outside_a_job_never_waits_for_itself() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(super::own_budget()));
            let first = super::reserve_from_budget(super::DECODE_BUDGET / 2 + 1).unwrap();
            let second = super::reserve_from_budget(super::DECODE_BUDGET / 2 + 2).unwrap();
            drop((first, second));
            tx.send(super::JOB_HOLDS.get()).unwrap();
        });
        let ended = rx.recv_timeout(std::time::Duration::from_secs(60));
        assert_eq!(ended, Ok(None));
    }

    /// `bytes` held as a decode's input, which the budget counts, as `from_memory` and
    /// `scaled` want them.
    /// A decode stuck with its bytes, such as a read from a share that stopped answering, is
    /// given them up once another decode has waited for room long enough, and gives back
    /// nothing more when it wakes. Before, two large images on such a share held the whole
    /// budget, and no other image was decoded until a restart.
    #[test]
    fn a_decode_stuck_with_its_bytes_lets_the_others_go_on() {
        use std::sync::mpsc;
        use std::time::Duration;
        let total = super::DECODE_BUDGET;
        let budget = super::budget();
        let (taken, stuck_holds) = mpsc::channel();
        let (wake, stuck) = mpsc::channel::<()>();
        let stuck_one = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total).unwrap();
            taken.send(()).unwrap();
            stuck.recv().unwrap();
            drop(held);
        });
        stuck_holds.recv().unwrap();
        let (done, waited) = mpsc::channel();
        let waiting = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total / 2).unwrap();
            done.send(()).unwrap();
            drop(held);
        });
        let went_on = waited.recv_timeout(Duration::from_secs(5));
        wake.send(()).unwrap();
        stuck_one.join().unwrap();
        waiting.join().unwrap();
        assert!(went_on.is_ok(), "the stuck decode held the budget");
        assert_eq!(lock_even_if_poisoned(&budget.state).free, total);
    }

    /// A decode that is slow, not stuck, keeps its bytes while another waits for room. It uses
    /// the processor all the while, as a large image decoded on a slow computer does. Before,
    /// its bytes were taken back after `HELD_TOO_LONG` while its pixels were still in memory,
    /// and the decodes together took about twice the budget.
    #[test]
    fn a_slow_decode_keeps_its_bytes() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let total = super::DECODE_BUDGET;
        let budget = super::budget();
        let (taken, slow_holds) = mpsc::channel();
        let (done, waited) = mpsc::channel();
        let done_too = done.clone();
        let slow_one = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total).unwrap();
            taken.send(()).unwrap();
            let start = Instant::now();
            while start.elapsed() < super::HELD_TOO_LONG * 3 {
                std::hint::black_box(start.elapsed());
            }
            done.send("slow").unwrap();
            drop(held);
        });
        slow_holds.recv().unwrap();
        let waiting = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total / 2).unwrap();
            done_too.send("waiting").unwrap();
            drop(held);
        });
        let first = waited.recv_timeout(Duration::from_secs(30));
        slow_one.join().unwrap();
        waiting.join().unwrap();
        assert_eq!(first, Ok("slow"), "a slow decode lost its bytes");
        assert_eq!(lock_even_if_poisoned(&budget.state).free, total);
    }

    /// A worker of a pool that waits for room in the budget is not given up, as it is not
    /// stuck: the slow decode that holds the room is. Without the mark of `WaitingForRoom`, it
    /// used no processor time while it waited, so the pool gave it up and started another
    /// worker in its place, one more thread for each wait. The pool here has one place, so a
    /// worker given up shows as the next job done while the first still waits.
    #[test]
    fn a_worker_that_waits_for_room_is_not_stuck() {
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;
        let total = super::DECODE_BUDGET;
        let budget = super::budget();
        let stuck_after = Duration::from_millis(200);
        let stop = Arc::new(AtomicBool::new(false));
        let (taken, slow_holds) = mpsc::channel();
        let stop_spinning = stop.clone();
        let slow_one = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total).unwrap();
            taken.send(()).unwrap();
            while !stop_spinning.load(Ordering::SeqCst) {
                std::hint::black_box(0);
            }
            drop(held);
        });
        slow_holds.recv().unwrap();
        let (jobs, rx) = mpsc::channel::<PathBuf>();
        let (done_tx, done) = mpsc::channel();
        let work = move |job: PathBuf| {
            if job.ends_with("waits.jpg") {
                super::TEST_BUDGET.set(Some(budget));
                drop(super::reserve_from_budget(total / 2).unwrap());
            }
            done_tx.send(job).unwrap();
        };
        crate::support::threads::spawn_worker_pool(1, stuck_after, rx, |_| {}, |_| None, work);
        jobs.send(PathBuf::from("/folder/waits.jpg")).unwrap();
        jobs.send(PathBuf::from("/folder/next.jpg")).unwrap();
        let while_waiting = done.recv_timeout(stuck_after * 5);
        stop.store(true, Ordering::SeqCst);
        slow_one.join().unwrap();
        assert!(while_waiting.is_err(), "the waiting worker was given up");
        let in_order: Vec<PathBuf> = (0..2)
            .map(|_| done.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(
            in_order,
            ["/folder/waits.jpg", "/folder/next.jpg"].map(PathBuf::from)
        );
        assert_eq!(lock_even_if_poisoned(&budget.state).free, total);
    }

    /// A decode that runs on the processor for ever, as one sent into an endless loop by a
    /// crafted file does, loses its bytes after `Progress::CEILING` times `HELD_TOO_LONG`.
    /// Before, it counted as slow, not stuck, and held them until a restart.
    #[test]
    fn a_decode_that_never_ends_loses_its_bytes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let total = super::DECODE_BUDGET;
        let budget = super::budget();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_spinning = stop.clone();
        let (taken, endless_holds) = mpsc::channel();
        let endless = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total).unwrap();
            taken.send(()).unwrap();
            while !stop_spinning.load(Ordering::SeqCst) {
                std::hint::black_box(0);
            }
            drop(held);
        });
        endless_holds.recv().unwrap();
        let (done, went_on) = mpsc::channel();
        let waiting = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total / 2).unwrap();
            done.send(()).unwrap();
            drop(held);
        });
        let ceiling = super::HELD_TOO_LONG * crate::support::threads::Progress::CEILING;
        let got_room = went_on.recv_timeout(ceiling * 2);
        stop.store(true, Ordering::SeqCst);
        endless.join().unwrap();
        waiting.join().unwrap();
        assert!(got_room.is_ok(), "the endless decode kept its bytes");
        assert_eq!(lock_even_if_poisoned(&budget.state).free, total);
    }

    /// An added format's output that grows takes no more of the budget than the most that the
    /// program may write. Before, its room doubled past that cap, nearly twice 512 MB, and
    /// other decodes waited for room that was never used.
    #[test]
    fn a_growing_input_takes_no_more_than_its_cap() {
        let mut input = super::DecodeInput::reserve(0).unwrap();
        input.grow_to(600, 1000).unwrap();
        input.grow_to(601, 1000).unwrap();
        assert_eq!(input.reserved, 1000);
    }

    /// The render of a large JPEG XL fits beside the file that a copy holds. Before, it asked
    /// for the whole budget on top of the file, and Save As of a 61 MP photo was refused.
    #[cfg(feature = "jxl")]
    #[test]
    fn a_jpeg_xl_render_fits_beside_the_file_held() {
        let _file = counted(vec![0; 1 << 20]);
        assert!(super::reserve_from_budget(super::jxl_render_reservation(9504, 6336)).is_ok());
    }

    fn counted(bytes: Vec<u8>) -> super::DecodeInput {
        let input = super::DecodeInput::reserve(bytes.len() as u64).unwrap();
        input.holding(bytes).unwrap()
    }

    /// A decode counts the file it read in its share of the budget, beside the pixels it made
    /// of it, scaled or whole. Before, a small JPEG followed by 4 MB of zeros took only its
    /// pixels, and a folder of such files held gigabytes outside the budget.
    #[test]
    fn a_decode_counts_the_file_it_read_beside_its_pixels() {
        let dir = std::env::temp_dir().join(format!("glimt-test-padded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("padded.jpg");
        let mut jpeg = Vec::new();
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 7]));
        (img.write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        ))
        .unwrap();
        jpeg.extend(vec![0; 4 << 20]);
        std::fs::write(&file, &jpeg).unwrap();
        let read = jpeg.len() as u64;
        // A thumbnail, scaled while it is decoded.
        {
            let _job = super::start_decode_job();
            super::decode_shrunk(&file, [8, 8], false).unwrap();
            assert!(super::JOB_HOLDS.get().unwrap() > read);
        }
        // Shown at its size, decoded whole by the image crate.
        {
            let _job = super::start_decode_job();
            super::decode(&file, [64, 64], false, 0).unwrap();
            assert!(super::JOB_HOLDS.get().unwrap() > read);
        }
        assert_eq!(super::INPUT_HOLDS.get(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A decode that holds its input and needs more for its pixels takes them before a decode
    /// that asked earlier, so that no other reads its input in between. Before, it waited at
    /// the end of the line with its input in memory, and every thread could do the same.
    #[test]
    fn a_decode_that_holds_its_input_goes_before_the_others() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        let total = super::DECODE_BUDGET;
        let budget = super::budget();
        let order = Arc::new(Mutex::new(Vec::new()));
        let input = super::DecodeInput::reserve(total * 4 / 10).unwrap();
        // Another decode holds half, until it is told to let go.
        let (taken, other_holds) = mpsc::channel();
        let (let_go, told) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let held = super::reserve_from_budget(total / 2).unwrap();
            taken.send(()).unwrap();
            told.recv().unwrap();
            drop(held);
        });
        other_holds.recv().unwrap();
        // A third asks for half and waits in line, with a tenth free.
        let in_line = order.clone();
        let third = std::thread::spawn(move || {
            super::TEST_BUDGET.set(Some(budget));
            let _held = super::reserve_from_budget(total / 2).unwrap();
            lock_even_if_poisoned(&in_line).push("asked first");
        });
        std::thread::sleep(Duration::from_millis(100));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let_go.send(()).unwrap();
        });
        // The pixels on top of the input: 6 tenths, which wait for the half held.
        let pixels = super::reserve_from_budget(total / 5).unwrap();
        lock_even_if_poisoned(&order).push("holds its input");
        drop((pixels, input));
        third.join().unwrap();
        assert_eq!(
            *lock_even_if_poisoned(&order),
            ["holds its input", "asked first"]
        );
    }

    /// A decode job holds the most that one of its decodes asked for, not their sum, joins an
    /// outer job rather than giving back early, and gives everything back when it ends.
    #[test]
    fn a_decode_job_holds_its_largest_decode() {
        let holds = || super::JOB_HOLDS.get();
        assert_eq!(holds(), None);
        {
            let _job = super::start_decode_job();
            drop(super::reserve_from_budget(100).unwrap());
            drop(super::reserve_from_budget(50).unwrap());
            assert_eq!(holds(), Some(100));
            {
                let _inner = super::start_decode_job();
                drop(super::reserve_from_budget(200).unwrap());
            }
            assert_eq!(holds(), Some(200));
            assert!(super::reserve_from_budget(super::DECODE_BUDGET + 1).is_err());
        }
        assert_eq!(holds(), None);
    }

    /// A whole read of an image's file counts in its decode job until the job ends, and is
    /// refused outside one, also the image crate's whole read of a JPEG. A decoder over bytes
    /// in memory is refused, also while the thread counts some, and those bytes are decoded
    /// through `from_memory` only when they are counted. A file of no known format counts
    /// nothing, as it is never read. Before, `read_limited` read up to 512 MB outside the
    /// budget for the EXIF pass, a decoder over such bytes passed, and later one passed while
    /// the thread counted any bytes at all.
    #[test]
    fn whole_reads_of_images_are_counted() {
        use image::codecs::jpeg::JpegDecoder;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-whole-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, named_png) = (dir.join("a.jpg"), dir.join("b.png"));
        let mut jpeg = Vec::new();
        (image::RgbImage::new(8, 8).write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        ))
        .unwrap();
        std::fs::write(&file, &jpeg).unwrap();
        std::fs::write(&named_png, &jpeg).unwrap();
        let unknown = dir.join("c.heic");
        std::fs::write(&unknown, vec![b'x'; 1 << 20]).unwrap();
        let length = jpeg.len() as u64;
        assert!(super::read_limited(&file).is_err());
        assert!(super::image_reader(&named_png).is_err());
        assert!(
            super::from_decoder(JpegDecoder::new(std::io::Cursor::new(&jpeg)).unwrap()).is_err()
        );
        {
            let _job = super::start_decode_job();
            let bytes = super::read_limited(&file).unwrap();
            assert!(super::JOB_HOLDS.get().unwrap() >= length);
            let uncounted = jpeg.repeat(2);
            let over = JpegDecoder::new(std::io::Cursor::new(&uncounted)).unwrap();
            assert!(super::from_decoder(over).is_err());
            assert!(super::from_memory(&uncounted).is_err());
            assert_eq!(super::from_memory(&bytes).unwrap().width(), 8);
            // A file of no known format is not read, so it counts nothing.
            let held = super::JOB_HOLDS.get();
            super::image_reader(&unknown).unwrap();
            assert_eq!(super::JOB_HOLDS.get(), held);
            // With the bytes read, the image crate's own read of the other is on top.
            super::image_reader(&named_png).unwrap();
            assert!(super::JOB_HOLDS.get().unwrap() >= 2 * length);
        }
        assert_eq!(
            (super::JOB_HOLDS.get(), super::INPUT_HOLDS.get()),
            (None, 0)
        );
        // A file that is no image is read outside a job, uncounted.
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, b"text").unwrap();
        assert_eq!(super::read_limited(&notes).unwrap(), b"text");
        assert_eq!(
            super::image_format(&named_png).unwrap(),
            Some(image::ImageFormat::Jpeg)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What is no file is refused before it is read, a pipe without blocking on it. That every
    /// whole-file read goes through `read_limited` is clippy's to hold (clippy.toml).
    #[test]
    fn read_limited_refuses_what_is_no_file() {
        // What is no file is refused, a pipe without blocking on it.
        let refused = super::read_limited("/dev/zero").unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        let fifo = std::env::temp_dir().join(format!("glimt-test-fifo-{}", std::process::id()));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let refused = super::read_limited(&fifo).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        // Not through the image crate either, because a collection naming a pipe x.png held
        // Find Duplicates.
        let fifo_png = fifo.with_extension("png");
        std::fs::rename(&fifo, &fifo_png).unwrap();
        assert!(super::image_reader(&fifo_png).is_err());
        let fifo = fifo_png;
        std::fs::remove_file(fifo).unwrap();
    }

    /// The histogram counts alpha too, which tells a transparent image from an opaque one.
    #[test]
    fn counts_alpha() {
        let mut img = eframe::egui::ColorImage::new([2, 1], vec![eframe::egui::Color32::BLACK; 2]);
        assert_eq!(super::histogram(&img)[4][255], 2);
        img.pixels[1] = eframe::egui::Color32::TRANSPARENT;
        let h = super::histogram(&img);
        assert_eq!((h[4][0], h[4][255]), (1, 1));
    }

    /// A damaged JPEG is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_jpegs() {
        let mut jpeg = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        for bytes in crate::support::test_support::broken(&jpeg) {
            let _ = super::scaled(&counted(bytes), [8, 8], true, None);
        }
    }

    /// A damaged TIFF or ICO is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_tiffs() {
        let dir = std::env::temp_dir().join(format!("glimt-test-btiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tiff = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut tiff),
                image::ImageFormat::Tiff,
            )
            .unwrap();
        let file = dir.join("a.tif");
        for bytes in crate::support::test_support::broken(&tiff) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::tiff_pages(&file);
            let _ = super::tiff_page(&file, 0);
        }
        let file = dir.join("a.ico");
        for bytes in crate::support::test_support::broken(&two_sizes_ico()) {
            std::fs::write(&file, &bytes).unwrap();
            for k in 0..super::pages(&file) {
                let _ = super::page_of(&file, k);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An ICO of a red 32×32 image and a blue 16×16.
    fn two_sizes_ico() -> Vec<u8> {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let frame = |side: u32, rgba: [u8; 4]| {
            let img = image::RgbaImage::from_pixel(side, side, image::Rgba(rgba));
            IcoFrame::as_png(&img, side, side, image::ExtendedColorType::Rgba8).unwrap()
        };
        let mut out = Vec::new();
        IcoEncoder::new(&mut out)
            .encode_images(&[frame(32, [255, 0, 0, 255]), frame(16, [0, 0, 255, 255])])
            .unwrap();
        out
    }

    /// An ICO of more than 256 sizes, with the best one last, shows it first, and each size is
    /// a page.
    #[test]
    fn reads_every_size_of_a_large_ico() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let frame = |side: u32| {
            let img = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 255, 255]));
            IcoFrame::as_png(&img, side, side, image::ExtendedColorType::Rgba8).unwrap()
        };
        let mut frames: Vec<_> = (0..256).map(|_| frame(16)).collect();
        frames.push(frame(32));
        let mut out = Vec::new();
        IcoEncoder::new(&mut out).encode_images(&frames).unwrap();
        let dir = std::env::temp_dir().join(format!("glimt-test-bigico-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ico = dir.join("many.ico");
        std::fs::write(&ico, out).unwrap();
        assert_eq!(super::pages(&ico), 257);
        assert_eq!(super::page_of(&ico, 0).unwrap().0.width(), 32);
        assert_eq!(super::page_of(&ico, 256).unwrap().0.width(), 16);
        // A directory that claims 2,000 sizes has as many pages as an animation keeps frames.
        let mut claims = vec![0, 0, 1, 0];
        claims.extend(2000u16.to_le_bytes());
        claims.resize(6 + 16 * 2000, 0);
        std::fs::write(&ico, claims).unwrap();
        assert_eq!(super::pages(&ico), super::MAX_FRAMES);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A damaged GIF is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_gifs() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bgif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gif = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut gif);
            for k in 0..2u8 {
                let frame = image::RgbaImage::from_pixel(4, 4, image::Rgba([k * 100, 0, 0, 255]));
                enc.encode_frame(image::Frame::new(frame)).unwrap();
            }
        }
        let file = dir.join("a.gif");
        for bytes in crate::support::test_support::broken(&gif) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::animation(&file, [8, 8]);
        }
        // One cut off inside its last frame keeps the frames before it.
        let mut cut = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut cut);
            for k in 0..3u32 {
                let noise =
                    (0..32 * 32 * 4).map(|i: u32| (i.wrapping_mul(2654435761 + k) >> 24) as u8);
                let frame = image::RgbaImage::from_raw(32, 32, noise.collect()).unwrap();
                enc.encode_frame(image::Frame::new(frame)).unwrap();
            }
        }
        cut.truncate(cut.len() - 100);
        std::fs::write(&file, &cut).unwrap();
        let (frames, _) = super::animation(&file, [32, 32]).unwrap();
        assert_eq!(frames.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// There is one small decode for thumbnails and prints. It fits within the box, brings the
    /// profile along, and gives the size as the image stands upright, before it was shrunk.
    #[test]
    fn small_decodes_within_the_box() {
        let dir = std::env::temp_dir().join(format!("glimt-test-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        image::RgbImage::new(400, 300).save(&png).unwrap();
        let (img, icc, size) = super::decode_shrunk(&png, [100, 100], true).unwrap();
        assert!(img.width() <= 100 && img.height() <= 100 && icc.is_none());
        assert_eq!(size, [400, 300]);
        let jpeg = dir.join("b.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        crate::metadata::exif::write_orientation(&jpeg, 1, false, true).unwrap();
        let (img, _, size) = super::decode_shrunk(&jpeg, [100, 100], true).unwrap();
        assert_eq!((img.width(), img.height(), size), (20, 40, [20, 40]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo turned by EXIF is shown turned, at its turned size, when it is shrunk before the
    /// turn. For orientation 6, the left of the stored pixels is at the top.
    #[test]
    fn shows_turned_photos_shrunk_first() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::support::test_support::change_shared_settings();
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-turned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // 400×200, red on the left and blue on the right, with EXIF orientation 6.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";
        let img = image::RgbImage::from_fn(400, 200, |x, _| match x < 200 {
            true => image::Rgb([255, 0, 0]),
            false => image::Rgb([0, 0, 255]),
        });
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        img.write_with_encoder(enc).unwrap();
        let (frames, orig, _) = super::decode(&file, [50, 50], true, 0).unwrap();
        let shown = &frames[0].0;
        assert_eq!(orig, [200, 400]);
        assert_eq!(shown.size, [25, 50]);
        let (top, bottom) = (shown.pixels[0], shown.pixels[shown.pixels.len() - 1]);
        assert!(top.r() > 200 && top.b() < 50, "{top:?}");
        assert!(bottom.b() > 200 && bottom.r() < 50, "{bottom:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A later page of a multi-page TIFF in 1 bit a pixel, as a fax or a scanned letter keeps
    /// it, is shown, with a WhiteIsZero page's 1 as black. Before, such a page could not be
    /// shown, copied or exported.
    #[test]
    fn shows_a_bilevel_page() {
        // A page as a little-endian TIFF directory with every value in its entry, and the
        // offset of the next one.
        let ifd = |w: u16, h: u16, bits: u16, white_is_zero: bool, data: u32, len: u32, next| {
            let photometric = if white_is_zero { 0 } else { 1 };
            let entries: [(u16, u16, u32); 9] = [
                (256, 3, w.into()),
                (257, 3, h.into()),
                (258, 3, bits.into()),
                (259, 3, 1),
                (262, 3, photometric),
                (273, 4, data),
                (277, 3, 1),
                (278, 3, h.into()),
                (279, 4, len),
            ];
            let mut out = 9u16.to_le_bytes().to_vec();
            for (tag, kind, value) in entries {
                out.extend(tag.to_le_bytes());
                out.extend(kind.to_le_bytes());
                out.extend(1u32.to_le_bytes());
                out.extend(value.to_le_bytes());
            }
            out.extend(u32::to_le_bytes(next));
            out
        };
        // The first page is one grey pixel, and the second ten pixels across and two down,
        // whose first and last are black.
        let mut file = b"II*\0\x08\0\0\0".to_vec();
        file.extend(ifd(1, 1, 8, false, 122, 1, 124));
        file.extend([128, 0]);
        file.extend(ifd(10, 2, 1, true, 238, 4, 0));
        file.extend([0b1000_0000, 0b0100_0000, 0, 0]);
        let dir = std::env::temp_dir().join(format!("glimt-test-fax-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fax.tif");
        std::fs::write(&path, &file).unwrap();
        assert_eq!(super::pages(&path), 2);
        let page = super::page_of(&path, 1).unwrap().0.to_luma8();
        let row = |y| (0..10).map(|x| page.get_pixel(x, y)[0]).collect::<Vec<_>>();
        assert_eq!(row(0), [0, 255, 255, 255, 255, 255, 255, 255, 255, 0]);
        assert_eq!(row(1), [255; 10]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A HEIC or AVIF photo's ICC profile is read from its `colr` box, for the view, the
    /// thumbnails, a copy and the colour line alike. Before, it was lost on the way through
    /// ffmpeg, and an iPhone's Display P3 photo was taken for sRGB.
    #[test]
    fn a_heif_photo_keeps_its_profile() {
        use crate::images::colour::ADOBE_RGB;
        let dir = std::env::temp_dir().join(format!("glimt-test-heif-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let colr = |kind: &[u8], body: &[u8]| {
            let size = (12 + body.len()) as u32;
            [&size.to_be_bytes()[..], b"colr", kind, body].concat()
        };
        let file = [
            &b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"[..],
            &colr(b"nclx", &[0, 1, 0, 13, 0, 6, 0x80]),
            &colr(b"prof", &ADOBE_RGB),
        ]
        .concat();
        for (ext, built) in [
            ("heic", cfg!(feature = "heic")),
            ("avif", cfg!(feature = "avif")),
        ] {
            let path = dir.join(format!("a.{ext}"));
            std::fs::write(&path, &file).unwrap();
            let icc = super::icc_profile(&path);
            assert_eq!(icc.as_deref(), built.then_some(&ADOBE_RGB[..]), "{ext}");
        }
        // A box cut short or holding no profile gives none.
        let path = dir.join("a.heic");
        std::fs::write(&path, colr(b"prof", b"not a profile at all, not one")).unwrap();
        assert_eq!(super::heif_icc(&path), None);
        std::fs::write(&path, &file[..file.len() - 10]).unwrap();
        assert_eq!(super::heif_icc(&path), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A damaged HEIC's profile is found or not, never a panic.
    #[test]
    fn survives_broken_heif_profiles() {
        let dir = std::env::temp_dir().join(format!("glimt-test-heif-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.heic");
        let seed = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\x34colrprof\0\0\0\x28\0\0\0\0\
            \0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0acsp";
        for bytes in crate::support::test_support::broken(seed) {
            std::fs::write(&path, &bytes).unwrap();
            let _ = super::heif_icc(&path);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A later page of a TIFF keeps its own ICC profile when it is shown, copied and exported,
    /// as the first page does. Before, only the first page's profile was read.
    #[test]
    fn a_later_page_keeps_its_profile() {
        use crate::images::colour::ADOBE_RGB;
        use image::ImageDecoder;
        use tiff::encoder::colortype::{Gray8, RGB8};
        let dir = std::env::temp_dir().join(format!("glimt-test-page-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pages.tif");
        let mut tiff =
            tiff::encoder::TiffEncoder::new(std::fs::File::create(&path).unwrap()).unwrap();
        tiff.write_image::<Gray8>(4, 4, &[0; 16]).unwrap();
        let green = [50u8, 200, 50].repeat(16);
        let mut page = tiff.new_image::<RGB8>(4, 4).unwrap();
        (page.encoder())
            .write_tag(tiff::tags::Tag::IccProfile, &ADOBE_RGB[..])
            .unwrap();
        page.write_data(&green).unwrap();
        drop(tiff);
        assert_eq!(
            super::page_of(&path, 1).unwrap().1.as_deref(),
            Some(&ADOBE_RGB[..])
        );
        // Exported in sRGB, Adobe RGB's green is not the same numbers.
        let mut exported = None;
        super::each_frame(&path, Some(1), true, |_, img| {
            exported = Some(img.to_rgb8().get_pixel(0, 0).0);
            Ok(())
        })
        .unwrap();
        assert_ne!(exported, Some([50, 200, 50]));
        // A copy of the page keeps the profile.
        let how = crate::images::save::CopyAs {
            read: None,
            page: 1,
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: None,
            long_side: None,
            format: Some(crate::images::save::Format::Png),
            jpeg_quality: 90,
            webp_quality: 90,
            strip: false,
        };
        let (png, _) = crate::images::save::copy_bytes(&path, &how).unwrap();
        let copy = image::ImageReader::new(std::io::Cursor::new(png)).with_guessed_format();
        let mut copy = copy.unwrap().into_decoder().unwrap();
        assert!(copy.icc_profile().unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every frame of a GIF and every page of a TIFF, one at a time, or only the one asked for.
    #[test]
    fn gives_each_frame() {
        let dir = std::env::temp_dir().join(format!("glimt-test-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gif = std::fs::File::create(dir.join("a.gif")).unwrap();
        let frame = |v| {
            let delay = image::Delay::from_numer_denom_ms(100, 1);
            image::Frame::from_parts(
                image::RgbaImage::from_pixel(8, 4, image::Rgba([v, 0, 0, 255])),
                0,
                0,
                delay,
            )
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(10), frame(120), frame(250)])
            .unwrap();
        let mut seen = Vec::new();
        let n = super::each_frame(&dir.join("a.gif"), None, true, |k, img| {
            seen.push((k, img.to_rgb8().get_pixel(0, 0)[0] / 100));
            Ok(())
        });
        assert_eq!((n, seen), (Ok(3), vec![(0, 0), (1, 1), (2, 2)]));
        let mut only = Vec::new();
        let n = super::each_frame(&dir.join("a.gif"), Some(1), true, |k, _| {
            only.push(k);
            Ok(())
        });
        assert_eq!((n, only), (Ok(1), vec![1]));
        // A TIFF of two pages.
        let file = std::fs::File::create(dir.join("b.tif")).unwrap();
        let mut tiff = tiff::encoder::TiffEncoder::new(file).unwrap();
        for v in [0u8, 200] {
            tiff.write_image::<tiff::encoder::colortype::Gray8>(4, 4, &[v; 16])
                .unwrap();
        }
        let mut pages = Vec::new();
        let n = super::each_frame(&dir.join("b.tif"), None, true, |k, img| {
            pages.push((k, img.to_luma8().get_pixel(0, 0)[0]));
            Ok(())
        });
        assert_eq!((n, pages), (Ok(2), vec![(0, 0), (1, 200)]));
        // An ICO of two sizes shows the largest first, as the image crate shows it, and then
        // the other.
        let ico = dir.join("b.ico");
        std::fs::write(&ico, two_sizes_ico()).unwrap();
        assert_eq!(super::pages(&ico), 2);
        let mut sizes = Vec::new();
        let n = super::each_frame(&ico, None, true, |k, img| {
            sizes.push((k, img.width(), img.to_rgba8().get_pixel(0, 0).0));
            Ok(())
        });
        assert_eq!(
            (n, sizes),
            (
                Ok(2),
                vec![(0, 32, [255, 0, 0, 255]), (1, 16, [0, 0, 255, 255])]
            )
        );
        let (_, orig, pages) = super::decode(&ico, [100, 100], true, 1).unwrap();
        assert_eq!((orig, pages), ([16, 16], 2));
        // A picture with EXIF orientation 6 is turned upright with Auto Rotate on, and stays as
        // it is with it off.
        use image::ImageEncoder;
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";
        let turned = dir.join("c.png");
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&turned).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::new(4, 2).write_with_encoder(enc).unwrap();
        for (rotate, size) in [(true, (2, 4)), (false, (4, 2))] {
            let mut got = (0, 0);
            super::each_frame(&turned, Some(0), rotate, |_, img| {
                got = (img.width(), img.height());
                Ok(())
            })
            .unwrap();
            assert_eq!(got, size, "rotate {rotate}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// A PNG whose orientation is only in its EXIF text chunk is shown turned by it, as the
    /// info panel reads it, and a turn saved starts from what the view showed. Before, the view
    /// took the eXIf chunk alone, so it showed the photo unturned while the panel said "Rotate
    /// 90 CW", and a quarter turn saved made it lie down.
    #[test]
    fn a_png_is_turned_by_the_orientation_in_its_text() {
        use crate::support::test_support::{CANON_EXIF, png_with_raw_profile};
        let dir = std::env::temp_dir().join(format!("glimt-test-pngturn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // 8×4 pixels with orientation 6.
        png_with_raw_profile(&file, "zTXt", "Raw profile type exif", CANON_EXIF);
        let size = |rotate| super::decode(&file, [100, 100], rotate, 0).unwrap().1;
        assert_eq!((size(true), size(false)), ([4, 8], [8, 4]));
        let (img, _) = super::load_icc(&file, true).unwrap();
        assert_eq!((img.width(), img.height()), (4, 8));
        crate::images::save::save_rotated(&file, true, 1, false, 90).unwrap();
        assert_eq!(image::image_dimensions(&file).unwrap(), (8, 4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A PNG whose EXIF, with ColorSpace Adobe RGB, is only in a text chunk is shown as Adobe
    /// RGB, and the colour line names it so. Before, both read the eXIf chunk alone, while the
    /// orientation came from the text chunk.
    #[test]
    fn a_png_is_adobe_rgb_by_the_exif_in_its_text() {
        use crate::support::test_support::png_with_raw_profile;
        let _held = crate::support::test_support::change_shared_settings();
        let dir = std::env::temp_dir().join(format!("glimt-test-pngadobe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // IFD0 points to an EXIF IFD that holds ColorSpace 2.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x87\x69\0\x04\0\0\0\x01\0\0\0\x1a\0\0\0\0\
            \0\x01\xa0\x01\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0";
        png_with_raw_profile(&file, "zTXt", "Raw profile type exif", exif);
        for rotate in [true, false] {
            let (_, icc) = super::load_icc(&file, rotate).unwrap();
            assert!(icc.as_ref() == Some(&*super::ADOBE_RGB), "rotate {rotate}");
        }
        let named = crate::images::colour::colour_source(&file);
        assert!(named.contains("Adobe RGB"), "{named}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A WebP whose EXIF chunk starts with "Exif\0\0", as some programs write it, is shown
    /// turned by its orientation. The image crate finds no orientation in such a chunk, so the
    /// view showed it unturned while the info panel said "Rotate 90 CW".
    #[test]
    fn a_webp_is_turned_by_a_prefixed_exif_chunk() {
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-webpturn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.webp");
        let out = std::fs::File::create(&file).unwrap();
        let mut enc = image::codecs::webp::WebPEncoder::new_lossless(out);
        let exif = [
            b"Exif\0\0".as_slice(),
            crate::support::test_support::CANON_EXIF,
        ]
        .concat();
        enc.set_exif_metadata(exif).unwrap();
        image::RgbImage::new(8, 4).write_with_encoder(enc).unwrap();
        let size = |rotate| super::decode(&file, [100, 100], rotate, 0).unwrap().1;
        assert_eq!((size(true), size(false)), ([4, 8], [8, 4]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A small GIF of many tiny frames keeps 1,000 of them, not one texture each, and Export
    /// Frames gives the same 1,000.
    #[test]
    fn keeps_a_thousand_frames() {
        let file =
            std::env::temp_dir().join(format!("glimt-test-frames-{}.gif", std::process::id()));
        let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&file).unwrap());
        let frames = (0..1200).map(|_| image::Frame::new(image::RgbaImage::new(1, 1)));
        enc.encode_frames(frames).unwrap();
        drop(enc);
        let (frames, _) = super::animation(&file, [100, 100]).unwrap();
        assert_eq!(frames.len(), super::MAX_FRAMES);
        // Export Frames gives as many, not every frame of the file.
        let given = super::each_frame(&file, None, true, |_, _| Ok(())).unwrap();
        assert_eq!(given, super::MAX_FRAMES);
        std::fs::remove_file(file).unwrap();
    }

    /// A small GIF with a large canvas and many tiny frames is read no further than so many
    /// canvases' bytes, though what is kept of each frame at a small size is tiny. Before, a GIF
    /// of a few KB with an 8000 by 8000 canvas made 1,000 canvases of 256 MB in Check Images.
    #[test]
    fn a_large_canvas_of_many_tiny_frames_is_cut_short() {
        let file =
            std::env::temp_dir().join(format!("glimt-test-canvas-{}.gif", std::process::id()));
        // A canvas of 2000 by 2000, two colours, and 1,000 frames of one pixel.
        let mut gif = b"GIF89a".to_vec();
        gif.extend(2000u16.to_le_bytes());
        gif.extend(2000u16.to_le_bytes());
        gif.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
        for _ in 0..1000 {
            gif.extend([0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 1, 0]);
        }
        gif.push(0x3b);
        std::fs::write(&file, gif).unwrap();
        let (frames, _) = super::animation(&file, [64, 64]).unwrap();
        // Save This Frame and Export Frames read no more of them than the view.
        let given = super::each_frame(&file, None, false, |_, _| Ok(())).unwrap();
        std::fs::remove_file(file).unwrap();
        let canvas = 2000 * 2000 * 4;
        let most = super::MAX_ANIMATION_CANVAS_BYTES / canvas;
        assert_eq!(frames.len() as u64, most);
        assert_eq!(given as u64, most);
    }

    /// A JPEG far larger than the window is decoded scaled, turned upright by its EXIF, with
    /// its full upright size. One that needs more than half its pixels is left to image.
    #[test]
    fn decodes_jpeg_scaled_and_upright() {
        let dir = std::env::temp_dir().join(format!("glimt-scaled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("wide.jpg");
        // Red on the left half, blue on the right.
        let img = image::RgbImage::from_fn(400, 200, |x, _| match x < 200 {
            true => image::Rgb([255, 0, 0]),
            false => image::Rgb([0, 0, 255]),
        });
        img.save(&file).unwrap();
        // With a quarter turn clockwise (EXIF orientation 6), the left half ends up on top.
        crate::metadata::exif::write_orientation(&file, 1, false, true).unwrap();
        let (img, _, orig) = super::scaled_jpeg(&file, [60, 100], true).unwrap();
        assert_eq!(orig, [200, 400]);
        assert_eq!((img.width(), img.height()), (50, 100));
        let rgb = img.to_rgb8();
        let (top, bottom) = (rgb.get_pixel(25, 10), rgb.get_pixel(25, 90));
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        // Not turned, it has the stored size, and at more than half there is no gain, so None.
        let (_, _, orig) = super::scaled_jpeg(&file, [100, 60], false).unwrap();
        assert_eq!(orig, [400, 200]);
        assert!(super::scaled_jpeg(&file, [300, 300], true).is_none());
        assert!(super::scaled_jpeg(&dir.join("none.jpg"), [10, 10], true).is_none());
        // A RAW file's preview, scaled the same way.
        let mut raw = b"FUJIFILMCCD-RAW ".to_vec();
        raw.extend(std::fs::read(&file).unwrap());
        std::fs::write(dir.join("a.raf"), raw).unwrap();
        let (img, icc, orig) = super::scaled_jpeg(&dir.join("a.raf"), [60, 100], true).unwrap();
        assert_eq!((orig, img.width(), img.height()), ([200, 400], 50, 100));
        assert!(icc.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Grey TIFFs of floats, as science writes them, are shown stretched from the lowest value
    /// to the highest, and a float RGB one as the image crate reads it.
    #[test]
    fn shows_float_tiff() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::support::test_support::change_shared_settings();
        use tiff::encoder::{TiffEncoder, colortype};
        let dir = std::env::temp_dir().join(format!("glimt-test-float-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gray: Vec<f32> = (0..60 * 30).map(|i| i as f32 * 3.7 - 500.0).collect();
        gray[1] = f32::NAN;
        let file = std::fs::File::create(dir.join("gray.tif")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray32Float>(60, 30, &gray)
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("gray.tif"), [100, 100], true, 0).unwrap();
        let px = &frames[0].0.pixels;
        assert_eq!(orig, [60, 30]);
        assert_eq!((px[0].r(), px[1].r(), px[px.len() - 1].r()), (0, 0, 255));
        let rgb: Vec<f32> = (0..60 * 30 * 3).map(|i| (i % 3) as f32 * 0.5).collect();
        let file = std::fs::File::create(dir.join("rgb.tif")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::RGB32Float>(60, 30, &rgb)
            .unwrap();
        assert!(super::decode(&dir.join("rgb.tif"), [100, 100], true, 0).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Radiance HDR far brighter than a screen is shown with its shadows dark and its
    /// highlights rolled off, and nothing but the brightest is cut to white.
    #[test]
    fn tone_maps_radiance_hdr() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::support::test_support::change_shared_settings();
        let dir = std::env::temp_dir().join(format!("glimt-test-hdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::Rgb32FImage::from_fn(100, 10, |x, _| {
            let v = 0.01 * 1.1f32.powi(x as i32); // 0.01 up to about 125
            image::Rgb([v, v, v])
        });
        image::DynamicImage::ImageRgb32F(img)
            .save(dir.join("a.hdr"))
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("a.hdr"), [100, 100], true, 0).unwrap();
        let px = &frames[0].0.pixels;
        assert_eq!(orig, [100, 10]);
        let (dark, mid, bright) = (px[0].r(), px[50].r(), px[99].r());
        assert!(
            dark < 40 && (60..230).contains(&mid) && bright > 230,
            "{dark} {mid} {bright}"
        );
        assert!(px[90].r() < bright);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// TIFFs under other names, as microscopes (Zeiss .lsm) and gel scanners (.gel) write them,
    /// are listed and shown, also grey floats.
    #[test]
    fn shows_lsm_and_gel() {
        use tiff::encoder::{TiffEncoder, colortype};
        let dir = std::env::temp_dir().join(format!("glimt-test-lsm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let grey: Vec<u16> = (0..60 * 30).map(|i| i as u16 * 30).collect();
        let file = std::fs::File::create(dir.join("a.gel")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray16>(60, 30, &grey)
            .unwrap();
        let floats: Vec<f32> = (0..60 * 30).map(|i| i as f32).collect();
        let file = std::fs::File::create(dir.join("b.lsm")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray32Float>(60, 30, &floats)
            .unwrap();
        let types = crate::model::listing::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (_, images) = crate::model::listing::list_folder(&dir, types).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(images, ["a.gel", "b.lsm"].map(std::ffi::OsString::from));
        for name in images {
            let (frames, orig, _) = super::decode(&dir.join(name), [100, 100], true, 0).unwrap();
            assert_eq!((frames[0].0.size, orig), ([60, 30], [60, 30]));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Check Images passes a whole photo. One cut off in copying, a picture that is no picture
    /// and an empty file are named broken.
    #[test]
    fn checks_images() {
        let dir = std::env::temp_dir().join(format!("glimt-test-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut jpeg = Vec::new();
        let img = image::RgbImage::from_fn(300, 200, |x, y| image::Rgb([x as u8, y as u8, 7]));
        (img.write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        ))
        .unwrap();
        std::fs::write(dir.join("whole.jpg"), &jpeg).unwrap();
        std::fs::write(dir.join("cut.jpg"), &jpeg[..jpeg.len() * 2 / 3]).unwrap();
        // A motion photo has a video after the JPEG's end, longer than 64 KB and without FF D9.
        let motion = [&jpeg[..], &vec![0x42; 200_000]].concat();
        std::fs::write(dir.join("motion.jpg"), motion).unwrap();
        assert_eq!(super::check_decodes(&dir.join("motion.jpg")), Ok(()));
        std::fs::write(dir.join("text.png"), b"not a picture").unwrap();
        std::fs::write(dir.join("empty.webp"), b"").unwrap();
        assert_eq!(super::check_decodes(&dir.join("whole.jpg")), Ok(()));
        for broken in ["cut.jpg", "text.png", "empty.webp"] {
            assert!(super::check_decodes(&dir.join(broken)).is_err(), "{broken}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A failure says what it says about the file: ffmpeg not installed, a share away, a time
    /// limit and Glimt's own size limit are not the file's fault, and a decoder's error is.
    /// Check Images and the thumbnails' failure marks both follow this one rule.
    #[test]
    fn tells_what_a_failure_says_about_the_file() {
        use super::DecodeFailure::{NotOfTheFile, OfTheFile, TimedOut, TooLarge};
        use std::path::Path;
        let (heic, jpg) = (Path::new("/a.heic"), Path::new("/a.jpg"));
        let missing = "video, HEIC and AVIF need ffmpeg: No such file or directory (os error 2)";
        assert_eq!(super::failure_of(heic, missing), NotOfTheFile);
        let away = std::io::Error::from_raw_os_error(5).to_string();
        assert_eq!(super::failure_of(jpg, &away), NotOfTheFile);
        assert_eq!(super::failure_of(jpg, super::NOT_LOOKED_AT), NotOfTheFile);
        let slow = format!("ffmpeg {}", crate::files::external::TIMED_OUT);
        assert_eq!(super::failure_of(heic, &slow), TimedOut);
        assert_eq!(super::failure_of(jpg, "image too large (1.1 GB)"), TooLarge);
        let bad = "Format error decoding Jpeg: invalid marker";
        assert_eq!(super::failure_of(jpg, bad), OfTheFile);
        let invalid = "Invalid data found when processing input";
        assert_eq!(super::failure_of(heic, invalid), OfTheFile);
    }

    /// A file whose look failed, as on a share that dropped for a moment, fails to decode with
    /// an error that is not the file's fault. Before, it was "not a regular file", and Check
    /// Images listed it as broken.
    #[test]
    fn a_file_that_could_not_be_looked_at_is_not_the_files_fault() {
        let dir = std::env::temp_dir().join(format!("glimt-test-look-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("away.png");
        image::RgbImage::new(4, 4).save(&file).unwrap();
        let looks_fail = &crate::files::file_io::LOOKS_FAIL;
        lock_even_if_poisoned(looks_fail).push(file.clone());
        let why = super::check_decodes(&file).unwrap_err();
        lock_even_if_poisoned(looks_fail).retain(|p| *p != file);
        let failure = super::failure_of(&file, &why);
        assert_eq!(failure, super::DecodeFailure::NotOfTheFile);
        assert_eq!(super::check_decodes(&file), Ok(()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// BMP, ICO, QOI and PNM are listed and shown as the other everyday formats.
    #[test]
    fn lists_and_decodes_more_formats() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["c.bmp", "d.ico", "e.qoi"] {
            image::RgbaImage::new(60, 30).save(dir.join(name)).unwrap();
        }
        image::RgbImage::new(60, 30)
            .save(dir.join("f.ppm"))
            .unwrap();
        let types = crate::model::listing::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (_, images) = crate::model::listing::list_folder(&dir, types).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(
            images,
            ["c.bmp", "d.ico", "e.qoi", "f.ppm"].map(std::ffi::OsString::from)
        );
        for name in images {
            let (frames, orig, _) = super::decode(&dir.join(name), [100, 100], true, 0).unwrap();
            assert_eq!((frames[0].0.size, orig), ([60, 30], [60, 30]));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every extension of `EXTS` is read by a decoder built in: a small image of each, in that
    /// format, comes back at its size. A format listed whose `image` feature was left out of
    /// Cargo.toml would be listed and never shown. The Netpbm forms that `image` writes only
    /// as PPM (PBM, PGM, PAM) are written by hand.
    #[test]
    fn every_listed_extension_is_decoded() {
        use crate::model::listing::{EXTS, TIFF_LIKE};
        let dir = std::env::temp_dir().join(format!("glimt-test-exts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pam_head = b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 3\nMAXVAL 255\nTUPLTYPE RGB\nENDHDR\n";
        let by_hand: [(&str, Vec<u8>); 3] = [
            ("pbm", [&b"P1\n6 4\n"[..], &b"0 1 "[..].repeat(12)].concat()),
            ("pgm", [&b"P5\n6 4\n255\n"[..], &[128; 24][..]].concat()),
            ("pam", [&pam_head[..], &[200; 72][..]].concat()),
        ];
        for ext in EXTS {
            let path = dir.join(format!("a.{ext}"));
            let format = match TIFF_LIKE.contains(ext) {
                true => Some(image::ImageFormat::Tiff),
                false => image::ImageFormat::from_extension(ext),
            };
            let format = format.unwrap_or_else(|| panic!("{ext}: no format of image's"));
            match by_hand.iter().find(|(e, _)| e == ext) {
                Some((_, bytes)) => std::fs::write(&path, bytes).unwrap(),
                None if format == image::ImageFormat::Hdr => {
                    image::Rgb32FImage::new(6, 4).save(&path).unwrap()
                }
                // Each writer takes one of these two: the JPEG one only RGB.
                None => (image::RgbaImage::new(6, 4).save_with_format(&path, format))
                    .or_else(|_| image::RgbImage::new(6, 4).save_with_format(&path, format))
                    .unwrap_or_else(|e| panic!("{ext}: {e}")),
            }
            let decoded = super::decode(&path, [100, 100], true, 0);
            let (frames, size, _) = decoded.unwrap_or_else(|e| panic!("{ext}: {e}"));
            assert_eq!((frames[0].0.size, size), ([6, 4], [6, 4]), "{ext}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join(format!("glimt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".b.png"), "").unwrap();
        let mut all = crate::model::listing::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let (dirs, images) = crate::model::listing::list_folder(&dir, all).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        // Hidden files and folders, when shown.
        all.hidden = true;
        let (dirs, images) = crate::model::listing::list_folder(&dir, all).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(images, [".b.png", "a.png"].map(std::ffi::OsString::from));
        assert_eq!(dirs, vec![dir.join(".hidden"), dir.join("sub")]);
        std::fs::remove_file(dir.join(".b.png")).unwrap();
        let (frames, orig, _) = super::decode(&dir.join("a.png"), [100, 100], true, 0).unwrap();
        assert_eq!(
            (frames.len(), frames[0].0.size, orig),
            (1, [100, 50], [400, 200])
        );
        // An animated GIF gives every frame with its delay (5 ms counts as 100 ms).
        let gif = std::fs::File::create(dir.join("b.gif")).unwrap();
        let frame = |ms| {
            let delay = image::Delay::from_numer_denom_ms(ms, 1);
            image::Frame::from_parts(image::RgbaImage::new(40, 20), 0, 0, delay)
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(50), frame(5)])
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("b.gif"), [100, 100], true, 0).unwrap();
        let delays: Vec<u32> = frames.iter().map(|f| f.1).collect();
        assert_eq!((delays, orig), (vec![50, 100], [40, 20]));
        // A GIF whose canvas claims 65535×65535 is refused before a frame is made, because each
        // frame would be a canvas of 17 GB.
        let huge = dir.join("huge.gif");
        let mut gif = b"GIF89a\xff\xff\xff\xff\0\0\0".to_vec();
        gif.extend(b",\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0;");
        std::fs::write(&huge, gif).unwrap();
        assert!(super::animation(&huge, [100, 100]).is_none());
        assert!(super::decode(&huge, [100, 100], true, 0).is_err());
        std::fs::remove_file(huge).unwrap();
        // In the tree, a folder reached again through a link is read once.
        std::fs::copy(dir.join("a.png"), dir.join("sub/c.png")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/back")).unwrap();
        let types = crate::model::listing::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let (own, files, _, _) = crate::model::listing::list_tree(
            &dir,
            types,
            &Default::default(),
            &Default::default(),
            |_| {},
            |_| {},
        )
        .unwrap();
        let files: Vec<std::ffi::OsString> = files.into_iter().map(|f| f.0).collect();
        assert_eq!(
            (own, files),
            (
                vec![dir.join("sub")],
                ["a.png", "b.gif", "sub/c.png"].map(Into::into).to_vec()
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A header that claims 40000×40000 is refused by every way a file is decoded, before
    /// anything of that size is allocated: the JPEG scaled to 1/2 for a view zoomed on a large
    /// screen, the image crate, an animation, and the bytes of an added format or ffmpeg.
    #[test]
    fn refuses_a_header_that_claims_too_much() {
        let mut jpeg = Vec::new();
        image::RgbImage::new(16, 16)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // The baseline frame header holds the height and then the width, 40000 being 0x9C40.
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        jpeg[sof + 5..sof + 9].copy_from_slice(&[0x9C, 0x40, 0x9C, 0x40]);
        let input = counted(jpeg.clone());
        assert!(super::scaled(&input, [16384, 16384], false, None).is_none());
        drop(input);
        let file =
            std::env::temp_dir().join(format!("glimt-test-claims-{}.jpg", std::process::id()));
        std::fs::write(&file, &jpeg).unwrap();
        // Each error must be the size refused. A header with no pixels after it fails in the
        // decoder too, for the missing data, so an error alone would pass without
        // `fits_decode_limit`.
        let refused = |e: Option<String>| e.is_some_and(|e| e.starts_with("image too large"));
        assert!(refused(
            super::decode(&file, [16384, 16384], false, 0).err()
        ));
        assert!(refused(
            super::decode_shrunk(&file, [16384, 16384], false).err()
        ));
        std::fs::remove_file(&file).unwrap();
        let claims = counted(b"P6\n40000 40000\n255\n".to_vec());
        assert!(refused(super::from_memory(&claims).err()));
        drop(claims);
        // A grey TIFF page of 40000×40000, read by the tiff crate for its later pages.
        let mut tiff = b"II*\0\x08\0\0\0\x09\0".to_vec();
        for (tag, kind, value) in [
            (256u16, 4u16, 40000u32),
            (257, 4, 40000),
            (258, 3, 8),
            (259, 3, 1),
            (262, 3, 1),
            (273, 4, 122),
            (277, 3, 1),
            (278, 4, 40000),
            (279, 4, 0),
        ] {
            tiff.extend(tag.to_le_bytes());
            tiff.extend(kind.to_le_bytes());
            tiff.extend(1u32.to_le_bytes());
            tiff.extend(value.to_le_bytes());
        }
        tiff.extend([0; 4]);
        let tif = file.with_extension("tif");
        std::fs::write(&tif, &tiff).unwrap();
        assert!(refused(super::tiff_page(&tif, 0).err()));
        std::fs::remove_file(&tif).unwrap();
        assert!(super::too_large_animation((40000, 40000)));
        // A GIF whose screen claims 40,000 pixels square gets no frames made at all.
        let gif = file.with_extension("gif");
        let mut head = b"GIF89a".to_vec();
        head.extend(40000u16.to_le_bytes().repeat(2));
        head.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
        head.extend([0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 1, 0, 0x3b]);
        std::fs::write(&gif, &head).unwrap();
        assert!(matches!(super::animated_frames(&gif), Some(Err(_))));
        std::fs::remove_file(&gif).unwrap();
    }

    /// A file larger than Glimt decodes is refused before it is read whole, so a sparse one of
    /// 8 GB takes no memory.
    #[test]
    fn refuses_huge_files() {
        let file = std::env::temp_dir().join(format!("glimt-test-huge-{}.jpg", std::process::id()));
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(8 << 30).unwrap();
        assert!(super::read_limited(&file).is_err());
        // Check Images and the EXIF of a RAW read through it too, never the whole 8 GB.
        assert!(super::is_whole_jpeg_file(&file).is_err());
        // And every decode through the image crate, whose JPEG decoder reads the file whole.
        assert!(super::image_reader(&file).is_err());
        // A TIFF is read by seeking, so a large one still opens.
        let tif = file.with_extension("tif");
        std::fs::hard_link(&file, &tif).unwrap();
        assert!(super::image_reader(&tif).is_ok());
        std::fs::remove_file(&tif).unwrap();
        // A JPEG named .png is told by its bytes, and refused as a JPEG.
        let png = file.with_extension("png");
        let mut g = std::fs::File::create(&png).unwrap();
        std::io::Write::write_all(&mut g, &[0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        g.set_len(8 << 30).unwrap();
        assert!(super::image_reader(&png).is_err());
        std::fs::remove_file(&png).unwrap();
        let raf = file.with_extension("raf");
        std::fs::hard_link(&file, &raf).unwrap();
        assert!(crate::images::raw::exif(&raf).is_err());
        std::fs::remove_file(&raf).unwrap();
        f.set_len(10).unwrap();
        let _job = super::start_decode_job();
        assert_eq!(super::read_limited(&file).unwrap().len(), 10);
        std::fs::remove_file(file).unwrap();
    }

    /// A JPEG's size comes from its frame header, width first. A camera RAW's size is its
    /// preview's, not that of the small thumbnail its TIFF header describes, else a folder of
    /// NEFs would be one group of the same dimensions in Find Duplicates. It is read from the
    /// head of the file, so one of 8 GB, too large to read whole, still has a size.
    #[test]
    fn reads_dimensions_from_headers() {
        let dir = std::env::temp_dir().join(format!("glimt-test-dims-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("e.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        assert_eq!(super::dimensions(&jpeg), Some((40, 20)));
        let encoded = |w, h, format| {
            let mut bytes = Vec::new();
            (image::RgbImage::new(w, h))
                .write_to(&mut std::io::Cursor::new(&mut bytes), format)
                .unwrap();
            bytes
        };
        let mut nef = encoded(16, 12, image::ImageFormat::Tiff);
        nef.extend(encoded(64, 48, image::ImageFormat::Jpeg));
        let raw = dir.join("f.nef");
        std::fs::write(&raw, nef).unwrap();
        assert_eq!(super::dimensions(&raw), Some((64, 48)));
        #[allow(clippy::disallowed_methods)] // a test's own file, made sparse
        let file = std::fs::File::options().write(true).open(&raw).unwrap();
        file.set_len(8 << 30).unwrap();
        assert_eq!(super::dimensions(&raw), Some((64, 48)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The profile inside a file is found for the info panel, and a file without one has none.
    #[test]
    fn finds_the_profile_inside() {
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (with, without) = (dir.join("with.png"), dir.join("without.png"));
        let mut out = Vec::new();
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        enc.set_icc_profile(super::ADOBE_RGB.to_vec()).unwrap();
        enc.write_image(&[0u8; 2 * 2 * 3], 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&with, out).unwrap();
        image::RgbImage::new(2, 2).save(&without).unwrap();
        assert_eq!(
            super::icc_profile(&with).as_deref(),
            Some(&super::ADOBE_RGB[..])
        );
        assert_eq!(super::icc_profile(&without), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn survives_a_panic() {
        assert_eq!(super::catch_panic_as_error(|| Ok::<_, String>(1)), Ok(1));
        assert!(
            super::catch_panic_as_error(|| -> Result<(), String> { panic!("broken file") })
                .is_err()
        );
    }
}
