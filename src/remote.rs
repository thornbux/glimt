//! Remote control, as `geeqie --remote`: `glimt --remote next` sends a command to the running
//! Glimt over a Unix socket in `$XDG_RUNTIME_DIR`, which only the user can reach.

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

fn socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("glimt.sock"))
}

/// Send `command` to a running Glimt; false if none is listening.
pub fn send(command: &str) -> bool {
    let Some(path) = socket() else { return false };
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    writeln!(stream, "{command}").is_ok()
}

/// Listen for commands in the background and pass each line to `tx`. If another Glimt already
/// listens, this one does not; a socket left by one that crashed is replaced.
pub fn listen(tx: mpsc::Sender<String>, ctx: eframe::egui::Context) {
    let Some(path) = socket() else { return };
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(_) if UnixStream::connect(&path).is_ok() => return,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            let Ok(l) = UnixListener::bind(&path) else {
                return;
            };
            l
        }
    };
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
