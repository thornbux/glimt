//! Remote control, as `geeqie --remote`: `glimt --remote next` sends a command to the Glimt
//! window used last, `glimt --remote-all quit` to every one. Each Glimt listens on its own Unix
//! socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach, and the one that gets
//! the focus writes its process ID to `focused` there. One started with `--name left` listens
//! as `left` instead, and `glimt --name left --remote next` reaches only it. In a Flatpak the folder is in the app's
//! own runtime folder, the part of `$XDG_RUNTIME_DIR` its instances share.

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
    ("slideshow", "Start or stop the slideshow"),
    ("random", "Turn random order on or off"),
    ("fullscreen", "Enter or leave full screen"),
    (
        "open PATH",
        "Show a file, folder or collection (starts Glimt if it is not running)",
    ),
    ("quit", "Close Glimt"),
];

fn dir() -> Option<PathBuf> {
    let mut dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if let Some(id) = std::env::var_os("FLATPAK_ID") {
        dir = dir.join("app").join(id);
    }
    Some(dir.join("glimt"))
}

/// This Glimt's name: its process ID, and in a Flatpak, where every instance can have the same
/// process ID, the Flatpak instance ID too (a new window from Ctrl+N shares the instance).
/// Found once, as every new thumbnail asks.
pub fn me() -> &'static str {
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
pub fn name() -> Option<&'static str> {
    NAME.get().map(String::as_str)
}

/// A name that cannot be taken for a process ID or a path: letters, digits, - and _.
pub fn valid_name(name: &str) -> bool {
    let ok = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    name.chars().all(ok) && !name.chars().all(|c| c.is_ascii_digit())
}

/// The socket's name: the --name, or the process ID.
fn id() -> &'static str {
    name().unwrap_or_else(me)
}

fn socket(id: &str) -> Option<PathBuf> {
    dir().map(|d| d.join(format!("glimt-{id}.sock")))
}

fn deliver(path: &std::path::Path, command: &str) -> bool {
    UnixStream::connect(path).is_ok_and(|mut s| writeln!(s, "{command}").is_ok())
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

/// Listen for commands in the background and pass each line to `tx`.
pub fn listen(tx: mpsc::Sender<String>, ctx: eframe::egui::Context) {
    let Some(path) = socket(id()) else {
        return;
    };
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let Some(listener) = claim(&path) else {
        eprintln!(
            "A Glimt named {} is running already: --remote goes to that one",
            id()
        );
        return;
    };
    LISTENING.store(true, std::sync::atomic::Ordering::Relaxed);
    focused();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                if tx.send(line).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        }
    });
}

/// This Glimt listens on its socket, so it removes it when it closes.
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The socket at `path` for this Glimt: one left by a Glimt that did not close is replaced, but
/// not one that a running Glimt (of the same `--name`) answers on.
fn claim(path: &std::path::Path) -> Option<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return None;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).ok()
}

/// This Glimt got the focus: `--remote` goes to it from now on.
pub fn focused() {
    if let Some(d) = dir() {
        let _ = std::fs::write(d.join("focused"), id());
    }
}

/// Glimt is closing: remove its socket.
pub fn stop() {
    if !LISTENING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(path) = socket(id()) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    /// A socket that a Glimt answers on is kept; one it left behind is taken over.
    #[test]
    fn claims_only_a_left_socket() {
        let path = std::env::temp_dir().join(format!("glimt-claim-{}.sock", std::process::id()));
        let first = super::claim(&path).unwrap();
        assert!(super::claim(&path).is_none());
        drop(first);
        assert!(super::claim(&path).is_some());
        std::fs::remove_file(path).unwrap();
    }
}
