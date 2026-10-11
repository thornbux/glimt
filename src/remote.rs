//! Remote control, as `geeqie --remote`: `glimt --remote next` sends a command to the Glimt
//! window used last, `glimt --remote-all quit` to every one. Each Glimt listens on its own Unix
//! socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach, and the one that gets
//! the focus writes its process ID to `focused` there.

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
    ("fullscreen", "Enter or leave full screen"),
    (
        "open PATH",
        "Show a file or folder (starts Glimt if it is not running)",
    ),
    ("quit", "Close Glimt"),
];

fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("glimt"))
}

fn socket(pid: u32) -> Option<PathBuf> {
    dir().map(|d| d.join(format!("glimt-{pid}.sock")))
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
        .and_then(|pid| socket(pid.trim().parse().ok()?));
    if last.is_some_and(|p| deliver(&p, command)) {
        return true;
    }
    live().iter().any(|p| deliver(p, command))
}

/// Send `command` to every running Glimt; how many got it.
pub fn send_all(command: &str) -> usize {
    live().iter().filter(|p| deliver(p, command)).count()
}

/// Listen for commands in the background and pass each line to `tx`.
pub fn listen(tx: mpsc::Sender<String>, ctx: eframe::egui::Context) {
    let Some(path) = socket(std::process::id()) else {
        return;
    };
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        return;
    };
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

/// This Glimt got the focus: `--remote` goes to it from now on.
pub fn focused() {
    if let Some(d) = dir() {
        let _ = std::fs::write(d.join("focused"), std::process::id().to_string());
    }
}

/// Glimt is closing: remove its socket.
pub fn stop() {
    if let Some(path) = socket(std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}
