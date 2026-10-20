//! Remote control, as `geeqie --remote`: `glimt --remote next` sends a command to the Glimt
//! window used last, `glimt --remote-all quit` to every one. Each Glimt listens on its own Unix
//! socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach, and the one that gets
//! the focus writes its process ID to `focused` there. One started with `--name left` listens
//! as `left` instead, and `glimt --name left --remote next` reaches only it. In a Flatpak the
//! folder is in the app's own runtime folder, the part of `$XDG_RUNTIME_DIR` its instances
//! share. Both ends are here: `client` sends, `listen` receives.

use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
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
    Some(dir.join("glimt"))
}

/// This Glimt's name: its process ID, and in a Flatpak, where every instance can have the same
/// process ID, the Flatpak instance ID too (a new window from Ctrl+N shares the instance).
/// Found once, as every new thumbnail asks.
pub fn instance_id() -> &'static str {
    static ME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        let info = std::env::var_os("FLATPAK_ID")
            .and_then(|_| std::fs::read_to_string("/.flatpak-info").ok());
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
/// one path, `select` any number of files, and a choice in brackets ([on|off]) may be left out.
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
            Some(many) if many.ends_with('…') => words.len() >= 2,
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
    if let Err(e) = s.read_to_end(&mut answer) {
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
        .and_then(|d| std::fs::read_to_string(d.join("focused")).ok())
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
        return eprintln!("No remote control: $XDG_RUNTIME_DIR is not set");
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
    mark_focused();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // Each client in a thread of its own: one waiting for the window (a folder of
            // 500,000 files still being read, a dialog open) holds up no other. The window's
            // answer is waited for as long as it takes; the client has the one time limit, so
            // there is one outcome when it runs out: "no answer", exit 1, never an empty
            // answer taken for "all found".
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(WAIT));
                let mut out = stream.try_clone().ok();
                for line in std::io::BufReader::new(stream)
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
                    if tx.send((line, reply)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                    if let (Some(answer), Some(out)) = (answer, out.as_mut())
                        && let Ok(bytes) = answer.recv()
                    {
                        let _ = out.write_all(&bytes);
                    }
                }
            });
        }
    });
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
        let _ = std::fs::write(d.join("focused"), socket_id());
    }
}

/// Glimt is closing: remove its socket.
pub fn stop() {
    if !LISTENING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(path) = socket(socket_id()) {
        let _ = std::fs::remove_file(path);
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
        assert!(!super::known(&words("subfolders")));
        assert!(super::known(&words("subfolders off")));
        assert!(super::known(&words("slideshow")) && super::known(&words("slideshow off")));
        assert!(!super::known(&words("slideshow yes")) && !super::known(&words("random on off")));
        assert!(
            !super::known(&words("subfolders yes")) && !super::known(&words("subfolders on off"))
        );
        assert!(!super::known(&[]));
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
