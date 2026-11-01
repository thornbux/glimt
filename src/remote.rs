//! Remote control, as `geeqie --remote`: `glimt --remote next` sends a command to the Glimt
//! window used last, `glimt --remote-all quit` to every one. Each Glimt listens on its own Unix
//! socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach, and the one that gets
//! the focus writes its process ID to `focused` there. One started with `--name left` listens
//! as `left` instead, and `glimt --name left --remote next` reaches only it. In a Flatpak the
//! folder is in the app's own runtime folder, the part of `$XDG_RUNTIME_DIR` its instances
//! share. Both ends are here: `client` sends, `listen` receives.

use crate::Action;
#[cfg(feature = "test-hooks")]
use crate::hooks;
use eframe::egui;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;

/// The commands, for `glimt --help`.
pub const COMMANDS: &[(&str, &str)] = &[
    ("next", "Show the next image"),
    ("prev", "Show the previous image"),
    ("first", "Show the first image"),
    ("last", "Show the last image"),
    (
        "slideshow [on|off]",
        "Start or stop the slideshow; without the word, the other of the two",
    ),
    (
        "random [on|off]",
        "Random order on or off; without the word, the other",
    ),
    (
        "fullscreen [on|off]",
        "Enter or leave full screen; without the word, the other",
    ),
    (
        "subfolders on|off",
        "Include the images of the subfolders in the list, or not (View → Include Subfolders)",
    ),
    (
        "open PATH",
        "Show a file, folder or collection (starts Glimt if it is not running)",
    ),
    (
        "title [TEXT…]",
        "This window's title, a template as in Preferences (%name%, %path%, %folder%, %app%); without the text, the one in Preferences",
    ),
    ("quit", "Close Glimt"),
    (
        "current",
        "Print the path of the image shown, ending in a NUL byte",
    ),
    (
        "selection",
        "Print the selected files (or the one shown), each ending in a NUL byte",
    ),
    (
        "select PATH…",
        "Select these files of the list and show the first; prints those not in it",
    ),
    #[cfg(feature = "test-hooks")]
    (
        "where LABEL",
        "Print where the menu item, button or choice with that text is, as x y (the release tests' build only)",
    ),
];

/// At most this many files for `select`: the window finds them in its own thread.
pub const MAX_SELECT: usize = 100_000;

/// Where the window sends a query's answer: paths, each ending in a NUL byte, as `find -print0`
/// writes them, since a file name can hold a line break.
pub type Reply = mpsc::Sender<Vec<u8>>;

/// Commands that answer. None of them changes a file: `select` only selects, and what is done
/// with the selection is the user's to do in the window.
fn is_query(command: &str) -> bool {
    matches!(command.trim(), "current" | "selection")
        || command.starts_with("select ")
        || command.starts_with("where ")
}

/// How long a command and its answer may take: a client that sends nothing, or a window that
/// does not answer, holds up the others no longer than this.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Glimt's runtime folder, which only the user can reach: sockets, and the sidecar locks.
pub(crate) fn dir() -> Option<PathBuf> {
    let mut dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if let Some(id) = std::env::var_os("FLATPAK_ID") {
        dir = dir.join("app").join(id);
    }
    let dir = dir.join("glimt");
    // Another user's folder of this name, where $XDG_RUNTIME_DIR is not private as it should be
    // (/tmp): its sockets would be theirs to answer and its locks theirs to hold.
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    own_or_none(&dir, uid).then_some(dir)
}

/// Whether `dir` is a folder of user `uid`, not a link, or not there yet (made by `listen`).
fn own_or_none(dir: &std::path::Path, uid: u32) -> bool {
    match std::fs::symlink_metadata(dir) {
        Ok(m) => m.is_dir() && m.uid() == uid,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// This Glimt's name: its process ID, and in a Flatpak, where every instance can have the same
/// process ID, the Flatpak instance ID too (a new window from Ctrl+N shares the instance).
/// Found once, as every new thumbnail asks.
pub fn instance_id() -> &'static str {
    static ME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        let info = std::env::var_os("FLATPAK_ID")
            .and_then(|_| crate::decode::read_text("/.flatpak-info").ok());
        let instance = info
            .as_deref()
            .and_then(|i| i.lines().find_map(|l| l.strip_prefix("instance-id=")));
        let pid = std::process::id();
        instance.map_or_else(|| pid.to_string(), |i| format!("{i}-{pid}"))
    })
}

/// The name from `--name`, set before the window opens.
static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Listen as `name` instead of the process ID.
pub fn set_name(name: String) {
    let _ = NAME.set(name);
}

/// The `--name` given, if any.
pub fn given_name() -> Option<&'static str> {
    NAME.get().map(String::as_str)
}

/// A name that cannot be taken for a process ID or a path: letters, digits, - and _, short
/// enough for the socket's path (at most 108 bytes).
pub fn valid_name(name: &str) -> bool {
    let ok = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    name.chars().all(ok) && !name.chars().all(|c| c.is_ascii_digit()) && name.len() <= 64
}

/// Whether the socket of `--name name` fits in the 107 bytes a socket's path may have here: in
/// a Flatpak its folder alone takes some 60.
pub fn name_fits(name: &str) -> bool {
    socket(name).is_none_or(|p| p.as_os_str().len() <= 107)
}

/// One of `COMMANDS`, with a path where it takes one and none where it does not: `open` takes
/// one path, `select` any number of files, and a choice in brackets ([on|off]) may be left out,
/// as may `title`'s words.
pub fn known(words: &[String]) -> bool {
    (COMMANDS.iter()).any(|(c, _)| {
        let mut parts = c.split(' ');
        if parts.next() != words.first().map(String::as_str) {
            return false;
        }
        match parts.next() {
            // A choice of words ("on|off") takes one of them, alone; in brackets, or none.
            Some(choices) if choices.contains('|') => {
                let optional = choices.starts_with('[');
                let choices = choices.trim_matches(|c| c == '[' || c == ']');
                (optional && words.len() == 1)
                    || (words.len() == 2 && choices.split('|').any(|w| w == words[1]))
            }
            Some(many) if many.trim_end_matches(']').ends_with('…') => {
                words.len() >= 2 || many.starts_with('[')
            }
            Some(_) => words.len() == 2,
            None => words.len() == 1,
        }
    })
}

/// A window took a command but gave no answer in time.
static UNANSWERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a window took a command but gave no answer within `WAIT`.
pub fn unanswered() -> bool {
    UNANSWERED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The socket's name: the --name, or the process ID.
fn socket_id() -> &'static str {
    given_name().unwrap_or_else(instance_id)
}

fn socket(id: &str) -> Option<PathBuf> {
    dir().map(|d| d.join(format!("glimt-{id}.sock")))
}

/// Send `command` to the socket at `path`, and print what it answers to standard output.
fn deliver(path: &std::path::Path, command: &str) -> bool {
    use std::io::Read;
    let Ok(mut s) = UnixStream::connect(path) else {
        return false;
    };
    if writeln!(s, "{command}").is_err() {
        return false;
    }
    // Done sending: the window answers, then closes.
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(WAIT));
    let mut answer = Vec::new();
    #[allow(clippy::disallowed_methods)] // a reply on Glimt's own private socket, with a time limit
    let read = s.read_to_end(&mut answer);
    if let Err(e) = read {
        eprintln!("glimt: no answer from {}: {e}", path.display());
        UNANSWERED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = std::io::stdout().write_all(&answer);
    true
}

/// The sockets of the running Glimts; those left by one that crashed are removed.
fn live() -> Vec<PathBuf> {
    let entries = dir().and_then(|d| std::fs::read_dir(d).ok());
    entries
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter(|p| {
            let alive = UnixStream::connect(p).is_ok();
            if !alive {
                let _ = std::fs::remove_file(p);
            }
            alive
        })
        .collect()
}

/// Send `command` to the Glimt used last, or else to another one; false if none runs.
pub fn send(command: &str) -> bool {
    let last = dir()
        .and_then(|d| crate::decode::read_text(d.join("focused")).ok())
        .and_then(|id| socket(id.trim()));
    if last.is_some_and(|p| deliver(&p, command)) {
        return true;
    }
    live().iter().any(|p| deliver(p, command))
}

/// Send `command` to the Glimt started with `--name name`; false if none runs.
pub fn send_to(name: &str, command: &str) -> bool {
    socket(name).is_some_and(|p| deliver(&p, command))
}

/// Send `command` to every running Glimt; how many got it.
pub fn send_all(command: &str) -> usize {
    live().iter().filter(|p| deliver(p, command)).count()
}

/// `glimt --remote` and `--remote-all`: `words` (the command and its arguments) sent to the
/// Glimt used last, to the one of `name`, or to every one; `paths` are `select`'s files, whole.
/// `None` once sent, its answer printed; the path to open when `open` finds no Glimt running.
/// Exits with 2 for a command that cannot be sent, and with 1 when no Glimt takes it.
pub fn client(
    words: &[String],
    paths: &[std::ffi::OsString],
    name: Option<&str>,
    to_all: bool,
) -> Option<PathBuf> {
    if !known(words) {
        eprintln!("glimt: no such command, or not with these arguments; glimt --help lists them");
        std::process::exit(2);
    }
    let mut command = words.join(" ");
    // select: whole paths after a NUL each, as names may hold spaces; a line ends the command.
    if words.first().is_some_and(|w| w == "select") {
        if paths.len() > MAX_SELECT {
            eprintln!("glimt: select takes at most {} files", MAX_SELECT);
            std::process::exit(2);
        }
        let mut sent = Vec::new();
        for p in paths {
            // As the window has its folder: with links followed, as ~/Pictures may be one;
            // not the file's own, which the list shows under its own name.
            let p = std::path::Path::new(p);
            let folder = (p.parent()).filter(|f| !f.as_os_str().is_empty());
            let real = std::fs::canonicalize(folder.unwrap_or(std::path::Path::new("."))).ok();
            let p = (real
                .zip(p.file_name())
                .map(|(folder, name)| folder.join(name)))
            .or_else(|| std::path::absolute(p).ok())
            .unwrap_or_else(|| p.into());
            match p.to_str().filter(|t| !t.contains('\n')) {
                Some(text) => sent.push(text.to_owned()),
                None => {
                    eprintln!("glimt: cannot send {p:?}: not UTF-8, or a line break in it");
                    std::process::exit(2);
                }
            }
        }
        command = format!("select {}", sent.join("\0"));
    }
    // A name that is not UTF-8 cannot go in the text sent; made lossy, it would name no file.
    // With no Glimt to send it to, this one opens it, by its own name.
    if words.first().is_some_and(|w| w == "open")
        && let Some(path) = paths.iter().find(|p| p.to_str().is_none())
    {
        let running = match name {
            Some(name) => socket(name).is_some_and(|p| UnixStream::connect(p).is_ok()),
            None => !live().is_empty(),
        };
        if !running {
            return Some(path.into());
        }
        eprintln!("glimt: cannot send {path:?}: not UTF-8");
        std::process::exit(2);
    }
    // The running Glimt has another working folder: send the full path.
    if let Some(path) = command.strip_prefix("open ") {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.into());
        command = format!("open {}", path.display());
    }
    // A line ends a command at the window: one in a file name would be read as a command
    // of its own, such as quit.
    if command.contains('\n') {
        eprintln!("glimt: cannot send a line break in a command or file name");
        std::process::exit(2);
    }
    let sent = match (name, to_all) {
        (_, true) => send_all(&command) > 0,
        (Some(name), false) => send_to(name, &command),
        (None, false) => send(&command),
    };
    if unanswered() {
        std::process::exit(1);
    }
    if sent {
        return None;
    }
    match command.strip_prefix("open ") {
        Some(path) => Some(path.into()),
        None => {
            match name {
                Some(name) => eprintln!("No Glimt named {name} is running"),
                None => eprintln!("Glimt is not running"),
            }
            std::process::exit(1);
        }
    }
}

/// Listen for commands in the background and pass each line to `tx`, with where to answer for
/// a query.
pub fn listen(tx: mpsc::Sender<(String, Option<Reply>)>, ctx: eframe::egui::Context) {
    let Some(path) = socket(socket_id()) else {
        return match std::env::var_os("XDG_RUNTIME_DIR") {
            None => eprintln!("No remote control: $XDG_RUNTIME_DIR is not set"),
            Some(_) => eprintln!("No remote control: its folder there is not this user's own"),
        };
    };
    // Only the user may connect, even where $XDG_RUNTIME_DIR is not private as it should be.
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let dir = path.parent().unwrap_or(&path);
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir);
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // Made by another user meanwhile, between the look in `dir` and here: not used.
        if dir_of_another(dir) {
            return eprintln!("No remote control: its folder there is not this user's own");
        }
    }
    let listener = match claim(&path) {
        Ok(listener) => listener,
        Err(None) => {
            return eprintln!(
                "A Glimt named {} is running already: --remote goes to that one",
                socket_id()
            );
        }
        Err(Some(e)) => return eprintln!("No remote control: {}: {e}", path.display()),
    };
    LISTENING.store(true, std::sync::atomic::Ordering::Relaxed);
    // The accept thread below keeps the listener, so its number stays valid for `queued`.
    LISTENER.store(
        std::os::fd::AsRawFd::as_raw_fd(&listener),
        std::sync::atomic::Ordering::Relaxed,
    );
    mark_focused();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // Each client in a thread of its own: one waiting for the window (a folder of
            // 500,000 files still being read, a dialog open) holds up no other. The window's
            // answer is waited for as long as it takes; the client has the one time limit, so
            // there is one outcome when it runs out: "no answer", exit 1, never an empty
            // answer taken for "all found".
            let (tx, ctx) = (tx.clone(), ctx.clone());
            // Counted from here, before its query is read: a client that came just before the
            // socket went is waited for at closing too.
            let client = Answering::new();
            std::thread::spawn(move || {
                serve(stream, &tx, &ctx);
                drop(client);
            });
        }
    });
}

/// One client's commands, each passed to the window, and its answer to a query sent back.
fn serve(
    stream: UnixStream,
    tx: &mpsc::Sender<(String, Option<Reply>)>,
    ctx: &eframe::egui::Context,
) {
    let _ = stream.set_read_timeout(Some(WAIT));
    let mut out = stream.try_clone().ok();
    // As much as `select` may send, no more: a client that sends without end does not fill the
    // memory.
    let most = MAX_SELECT as u64 * 4096 + 64;
    for line in std::io::BufReader::new(std::io::Read::take(stream, most))
        .lines()
        .map_while(Result::ok)
    {
        let (reply, answer) = match is_query(&line) {
            true => {
                let (reply, answer) = mpsc::channel();
                (Some(reply), Some(answer))
            }
            false => (None, None),
        };
        // Counted until its answer is written: `stop` waits for that at closing.
        let answering = answer.is_some().then(Answering::new);
        if tx.send((line, reply)).is_err() {
            return;
        }
        ctx.request_repaint();
        if let (Some(answer), Some(out)) = (answer, out.as_mut()) {
            match answer.recv() {
                Ok(bytes) => drop(out.write_all(&bytes)),
                // Dropped by the window (too many waiting, or closing): closed now, the client
                // would read nothing, "all found"; held open, its time limit says "no answer".
                Err(_) => {
                    drop(answering);
                    return std::thread::sleep(WAIT + std::time::Duration::from_secs(1));
                }
            }
        }
        drop(answering);
    }
}

/// Whether `dir` is there and not the user's own folder.
fn dir_of_another(dir: &std::path::Path) -> bool {
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid());
    uid.is_ok_and(|uid| !own_or_none(dir, uid))
}

/// Whether this Glimt listens for --remote commands.
pub fn listening() -> bool {
    LISTENING.load(std::sync::atomic::Ordering::Relaxed)
}

/// This Glimt listens on its socket, so it removes it when it closes.
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The socket at `path` for this Glimt: one left by a Glimt that did not close is replaced, but
/// not one that a running Glimt (of the same `--name`) answers on.
/// `Err(None)` when a running Glimt answers on it; `Err(Some(why))` when it cannot be made.
fn claim(path: &std::path::Path) -> Result<UnixListener, Option<std::io::Error>> {
    // Bound first, which fails if the socket is there: two Glimt of one name started at once
    // cannot both find it free and then remove each other's.
    let first = match UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(e) => e,
    };
    if first.kind() != std::io::ErrorKind::AddrInUse {
        return Err(Some(first));
    }
    // One at a time from here: another Glimt of this name could take the left socket over
    // between our check and our remove, and lose its new one to us.
    #[allow(clippy::disallowed_methods)] // an empty lock file in the private runtime folder
    let lock = std::fs::File::create(path.with_extension("lock")).map_err(Some)?;
    lock.lock().map_err(Some)?;
    if UnixStream::connect(path).is_ok() {
        return Err(None);
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).map_err(Some)
}

/// This Glimt got the focus: `--remote` goes to it from now on.
pub fn mark_focused() {
    if let Some(d) = dir() {
        // Whole, never truncated first: a `--remote` reading it meanwhile got an empty name.
        let _ = crate::config::replace_file(&d.join("focused"), socket_id());
    }
}

/// Glimt is closing, its socket removed by `close` before the last answers: the clients still
/// connected waited for, with `answer` answering the queries they send meanwhile. Not removed
/// again here: a new Glimt of the same name may have it by now.
pub fn stop(answer: impl FnMut()) {
    if LISTENING.load(std::sync::atomic::Ordering::Relaxed) {
        wait_answered(answer, std::time::Duration::from_secs(1));
    }
}

/// Until no client is left or `limit` has passed, `answer` called again and again: a query that
/// came after the window last looked is answered too, and an answer is written whole before the
/// process ends. A client left without one would read an empty answer, for select "all found".
/// Also a client still queued for accept: it connected before `close` removed the socket.
/// Quiet twice, 10 ms apart, so one the accept thread has just taken from the queue is counted.
fn wait_answered(mut answer: impl FnMut(), limit: std::time::Duration) {
    let until = std::time::Instant::now() + limit;
    let busy = || {
        ANSWERING.load(std::sync::atomic::Ordering::Relaxed) > 0
            || queued(LISTENER.load(std::sync::atomic::Ordering::Relaxed))
    };
    let mut quiet = 0;
    while quiet < 2 && std::time::Instant::now() < until {
        quiet = if busy() { 0 } else { quiet + 1 };
        answer();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Whether a client waits to be accepted on the listening socket `fd` (-1: none).
fn queued(fd: i32) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, n: std::ffi::c_ulong, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 1;
    let mut p = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd, and no wait; a closed or negative fd is only reported, not used.
    fd >= 0 && unsafe { poll(&mut p, 1, 0) } == 1 && p.revents & POLLIN != 0
}

/// The listening socket's file number, -1 when this Glimt does not listen.
static LISTENER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// No new client: this Glimt's socket removed, as it closes.
pub fn close() {
    if LISTENING.load(std::sync::atomic::Ordering::Relaxed)
        && let Some(path) = socket(socket_id())
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Clients connected, and queries sent to the window whose answers are not written yet.
static ANSWERING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One of `ANSWERING` while it lives.
struct Answering;

impl Answering {
    fn new() -> Self {
        ANSWERING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Answering
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        ANSWERING.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl crate::App {
    // ---- Remote control ----

    /// `glimt --remote` commands and queries; a `select` waits while the folder is read or a
    /// dialog asks about the selection.
    pub(crate) fn receive_remote(&mut self, ctx: &egui::Context) {
        // After `open`, the folder is still being read: its files are not all listed. A dialog
        // asking about the selection (Delete, Rename) is answered for the one it asked about, not
        // one a script put in its place meanwhile.
        let busy = |app: &Self| app.listing_rx.is_some() || app.asking_about_selection();
        // Those held back, in the order they came, until one (an `open`) makes the window busy
        // again: each gets its own answer, from the list as it was when it was sent.
        while !self.waiting_select.is_empty() && !busy(self) {
            let (command, reply) = self.waiting_select.remove(0);
            self.remote_one(ctx, &command, reply);
        }
        while let Ok((command, reply)) = self.remote_rx.try_recv() {
            let select = reply.is_some() && command.starts_with("select ");
            // Behind a select held back, what can change the list it is answered for waits its
            // turn: another script's `open`, subfolders, a slideshow with its subfolders. Other
            // queries and commands are done at once, as without it.
            let lists = ["open ", "subfolders", "slideshow"];
            let relists = lists.iter().any(|c| command.starts_with(c));
            let wait = (relists && !self.waiting_select.is_empty()) || (select && busy(self));
            if !wait {
                self.remote_one(ctx, &command, reply);
            } else if self.waiting_select.len() < 64 {
                self.waiting_select.push((command, reply));
            }
            // More than that waiting: the answer dropped, which the client is told as "no
            // answer" (serve), so the script tries again.
        }
    }

    /// At closing: each select still held back is answered with all its paths, none of them
    /// selected. Left without an answer, the closed socket would read as "all found".
    /// Also those sent just before, not yet taken from the channel: a query is answered as
    /// usual (current, selection).
    pub(crate) fn answer_held_selects(&mut self) {
        let sent: Vec<_> = self.remote_rx.try_iter().collect();
        let held = std::mem::take(&mut self.waiting_select);
        let ctx = self.ctx.clone();
        for (command, reply) in held.into_iter().chain(sent) {
            let Some(reply) = reply else { continue };
            match command.strip_prefix("select ") {
                Some(paths) => {
                    let paths = paths.split('\0').filter(|p| !p.is_empty());
                    let _ = reply.send(paths.flat_map(|p| p.bytes().chain([0])).collect());
                }
                // Answered as at any other time (`where` too, in the release tests' build).
                None => self.remote_one(&ctx, &command, Some(reply)),
            }
        }
    }

    /// One command from `glimt --remote`, with the answer to send if it asks for one.
    pub(crate) fn remote_one(&mut self, ctx: &egui::Context, command: &str, reply: Option<Reply>) {
        match reply {
            Some(reply) if command.starts_with("select ") => {
                let _ = reply.send(self.remote_select(ctx, command));
            }
            #[cfg(feature = "test-hooks")]
            Some(reply) if command.starts_with("where ") => {
                let _ = reply.send(hooks::position(command.trim_start_matches("where ")));
            }
            Some(reply) => drop(reply.send(self.remote_query(command))),
            None => self.remote_command(ctx, command),
        }
    }

    /// A command from `glimt --remote`: next, prev, first, last, slideshow, random and
    /// fullscreen (each with on or off, or without to go to the other), subfolders on|off, quit,
    /// open PATH, or title with this window's own title, or without to have the one in Preferences.
    pub(crate) fn remote_command(&mut self, ctx: &egui::Context, command: &str) {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        let action = match command.trim() {
            "next" => Some(Action::Next),
            "prev" => Some(Action::Prev),
            "first" => Some(Action::First),
            "last" => Some(Action::Last),
            // With on or off, a state a script can count on: nothing when it is so already.
            "slideshow on" if self.slideshow.is_some() => None,
            "slideshow off" if self.slideshow.is_none() => None,
            "slideshow" | "slideshow on" | "slideshow off" => Some(Action::Slideshow),
            "random" => Some(Action::RandomOrder),
            "random on" => return self.set_random_order(ctx, true),
            "random off" => return self.set_random_order(ctx, false),
            "fullscreen on" if fullscreen => None,
            "fullscreen off" if !fullscreen => None,
            "fullscreen" | "fullscreen on" | "fullscreen off" => Some(Action::FullScreen),
            "quit" => {
                // The only way out of a kiosk.
                self.kiosk_quit = true;
                Some(Action::Quit)
            }
            "subfolders on" => return self.set_subfolders(ctx, true),
            "subfolders off" => return self.set_subfolders(ctx, false),
            "title" => return self.set_window_title(ctx, ""),
            other if other.starts_with("title ") => {
                return self.set_window_title(ctx, other["title ".len()..].trim());
            }
            other if other.starts_with("open ") => {
                // The path as it was sent, not trimmed: a folder's name may end in a space.
                if let Some(path) = command.trim_start().strip_prefix("open ") {
                    self.visit(ctx, PathBuf::from(path));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                None
            }
            _ => None,
        };
        if let Some(action) = action {
            self.run(ctx, action);
        }
    }

    /// The answer to `glimt --remote current` or `selection`: the paths, each ending in a NUL
    /// byte. Nothing else is told, and nothing changes.
    // shortcut: the paths of a selection made in the window's thread, some 50 ms for 500,000
    // selected; in a thread of its own if scripts ask for large selections often.
    pub(crate) fn remote_query(&self, command: &str) -> Vec<u8> {
        use std::os::unix::ffi::OsStringExt;
        let files = match command.trim() {
            "current" => vec![self.list.cur],
            _ => self.targets(),
        };
        (files.into_iter().filter(|&i| i < self.list.files.len()))
            .flat_map(|i| {
                let path = self.path(i).into_os_string().into_vec();
                path.into_iter().chain([0])
            })
            .collect()
    }

    /// `--remote select PATH…` (the paths after a NUL each): select those of the list and show
    /// the first. The answer: the paths that are not in the list, each ending in a NUL byte.
    /// Only the selection changes; deleting or moving it is left to the user.
    pub(crate) fn remote_select(&mut self, ctx: &egui::Context, command: &str) -> Vec<u8> {
        let paths = command.strip_prefix("select ").unwrap_or_default();
        let paths: Vec<&str> = paths.split('\0').filter(|p| !p.is_empty()).collect();
        // The list's name of `p`: relative to the folder shown.
        fn name<'a>(dir: &Path, p: &'a str) -> Option<&'a std::ffi::OsStr> {
            Some(Path::new(p).strip_prefix(dir).ok()?.as_os_str())
        }
        let dir = self.dir.clone();
        let names: HashSet<&std::ffi::OsStr> = paths.iter().filter_map(|p| name(&dir, p)).collect();
        let found: std::collections::BTreeSet<usize> = (self.list.files.iter().enumerate())
            .filter(|(_, f)| names.contains(f.as_os_str()))
            .map(|(i, _)| i)
            .collect();
        let listed: HashSet<&std::ffi::OsStr> = found
            .iter()
            .map(|&i| self.list.files[i].as_os_str())
            .collect();
        let missing: Vec<u8> = (paths.iter())
            .filter(|p| !name(&dir, p).is_some_and(|n| listed.contains(n)))
            .flat_map(|p| p.bytes().chain([0]))
            .collect();
        if let Some(&first) = found.first() {
            self.go(ctx, first);
            self.list.selected = found;
            self.list.anchor = None;
            self.selection_changed(ctx);
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    /// Only the commands Glimt knows go out, with a path only where one is taken.
    #[test]
    fn knows_its_commands() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(super::known(&words("next")));
        assert!(super::known(&["open".into(), "/a b.jpg".into()]));
        // Two words are two paths, not one with a space: the shell joins no words.
        assert!(!super::known(&words("open /a.jpg /b.jpg")));
        assert!(super::known(&words("select /a.jpg /b.jpg")));
        assert!(!super::known(&words("nxet")));
        assert!(!super::known(&words("open")));
        assert!(!super::known(&words("next 2")));
        assert!(super::known(&words("subfolders on")));
        assert!(super::known(&words("title")));
        assert!(super::known(&words("title Left: %name%")));
        assert!(!super::known(&words("subfolders")));
        assert!(super::known(&words("subfolders off")));
        assert!(super::known(&words("slideshow")) && super::known(&words("slideshow off")));
        assert!(!super::known(&words("slideshow yes")) && !super::known(&words("random on off")));
        assert!(
            !super::known(&words("subfolders yes")) && !super::known(&words("subfolders on off"))
        );
        assert!(!super::known(&[]));
    }

    /// The runtime folder is used when it is the user's own folder or not there yet; not when
    /// it is another user's, a link, or a file.
    #[test]
    fn uses_only_its_own_runtime_folder() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        assert!(super::own_or_none(&dir, uid));
        assert!(!super::own_or_none(&dir, uid + 1));
        assert!(super::own_or_none(&dir.join("none"), uid));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!super::own_or_none(&link, uid));
        std::fs::write(dir.join("file"), "").unwrap();
        assert!(!super::own_or_none(&dir.join("file"), uid));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// At closing, a client that sends its query after the window last answered is answered
    /// too: the window is asked again until no client is left, not only waited for.
    #[test]
    fn answers_a_query_that_comes_while_closing() {
        let mut client = Some(super::Answering::new());
        let (query, queries) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = query.send(());
        });
        let mut answered = 0;
        let answer = || {
            for () in queries.try_iter() {
                answered += 1;
                client = None;
            }
        };
        super::wait_answered(answer, std::time::Duration::from_secs(30));
        assert_eq!(answered, 1);
        assert!(client.is_none());
    }

    /// A client that connected before the socket went, but that the accept thread has not
    /// taken yet, is seen as waiting: closing waits for it too.
    #[test]
    fn sees_a_client_queued_for_accept() {
        let path = std::env::temp_dir().join(format!("glimt-queued-{}.sock", std::process::id()));
        let listener = super::claim(&path).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
        assert!(!super::queued(fd));
        let _client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(super::queued(fd));
        let _served = listener.accept().unwrap();
        assert!(!super::queued(fd));
        assert!(!super::queued(-1));
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    /// A query whose answer the window drops (too many waiting) is not closed at once, which the
    /// client would read as an empty answer, "all found": it is left to time out, "no answer".
    #[test]
    fn a_dropped_answer_is_no_answer() {
        use std::io::{Read, Write};
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || super::serve(server, &tx, &eframe::egui::Context::default()));
        writeln!(client, "select /a.jpg\0").unwrap();
        let (_, reply) = rx.recv().unwrap();
        drop(reply);
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .unwrap();
        let mut got = Vec::new();
        let read = client.read_to_end(&mut got);
        assert!(read.is_err() && got.is_empty(), "{read:?} {got:?}");
    }

    /// A socket that a Glimt answers on is kept; one it left behind is taken over.
    #[test]
    fn claims_only_a_left_socket() {
        let path = std::env::temp_dir().join(format!("glimt-claim-{}.sock", std::process::id()));
        let first = super::claim(&path).unwrap();
        assert!(super::claim(&path).is_err_and(|e| e.is_none()));
        drop(first);
        // A process another test starts holds a copy of the socket until it runs its program,
        // so it can answer a moment longer.
        let taken = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            super::claim(&path).is_ok()
        });
        assert!(taken);
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
