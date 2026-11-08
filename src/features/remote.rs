//! Remote control, as with `geeqie --remote`. `glimt --remote next` sends a command to the
//! Glimt window used last, and `glimt --remote-all quit` sends one to every window. Each Glimt
//! listens on its own Unix socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach,
//! and the one that gets the focus writes its process ID to `focused` there. A Glimt started
//! with `--name left` listens as `left` instead, and `glimt --name left --remote next` reaches
//! only it. In a Flatpak the folder is in the app's own runtime folder, the part of
//! `$XDG_RUNTIME_DIR` its instances share. Both ends are here: `client` sends, and `listen`
//! receives.

use crate::features::remote;
#[cfg(feature = "test-hooks")]
use crate::support::test_hooks;
use crate::ui::actions::Action;
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
    ("previous", "Show the previous image"),
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
        "This window's title, a template as in Preferences (%name%, %path%, %folder%, %app%, %id%); without the text, the one in Preferences",
    ),
    (
        "identify",
        "Show this window's ID (%id%: its --name or process ID) large over the image for a few seconds",
    ),
    ("quit", "Close Glimt"),
    (
        "run ACTION",
        "Do an action by its name, one of those glimt --help lists, which change no file, sidecar, mark or collection",
    ),
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

/// The commands renamed in 0.74.0, as (old word, new word). Scripts may send the old word too,
/// forever, and `glimt --help` lists only the new one. The client sends the word as it was
/// given, so an older Glimt still takes the old word.
pub const RENAMED_COMMANDS: [(&str, &str); 1] = [("prev", "previous")];

/// The options that send a command, to the Glimt used last or to every one.
pub const REMOTE: &str = "--remote";
pub const REMOTE_ALL: &str = "--remote-all";

/// The exit code of `glimt --remote` when a command cannot be sent: unknown, with the wrong
/// arguments, or with a path that cannot go in the text sent.
pub const NOT_SENT: i32 = 2;

/// The exit code of `glimt --remote` when no Glimt takes the command, or none answers it.
pub const NOT_TAKEN: i32 = 1;

/// The exit code of `glimt --remote` when a window took the command but did not do it: a `run`
/// refused while a dialog is open or a text field has the focus, or a command that window does
/// not know (an older Glimt than the client). With `--remote-all` it is enough that one window
/// did not do it.
pub const NOT_DONE: i32 = 3;

/// The start of the window's answer to a command it did not do, before the reason. Every other
/// answer of a command is empty, and that of a query is paths, which start with "/".
const NOT_DONE_ANSWER: &str = "not done: ";

/// How long `--remote identify` shows the window's ID.
pub(crate) const IDENTIFY_TIME: std::time::Duration = std::time::Duration::from_secs(3);

/// The file in the runtime folder that holds the name of the Glimt that had the focus last.
pub const FOCUSED_FILE: &str = "focused";

/// The most files `select` takes, because the window finds them in its own thread.
pub const MAX_SELECT: usize = 100_000;

/// The longest line of a command other than `select`, in bytes: room for a path or a title.
/// Only `select` may be as long as its files take.
const MOST_LINE: u64 = 64 * 1024;

/// The most clients served at once, each by a thread of its own. Another waits in the socket's
/// queue until one is done, or until its time limit tells it "no answer".
const MOST_CLIENTS: usize = 16;

/// How long a command and its answer may take. A client that sends nothing, or a window that
/// does not answer, holds up the others no longer than this.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The name from `--name`, set before the window opens.
static GIVEN_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The ID of the socket this Glimt listens on. It is `socket_id()`, or `NAME.ID` for a window
/// that shares its --name with one that listens as `NAME` already.
static LISTENING_AS: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// A window took a command but gave no answer in time.
static TIMED_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A window answered that it did not do the command.
static WAS_NOT_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This Glimt listens on its socket, so it removes it when it closes.
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The listening socket's file number, -1 when this Glimt does not listen.
static LISTENER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Clients connected, and queries sent to the window whose answers are not written yet.
static UNFINISHED_ANSWERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Where the window sends a query's answer. It is paths, each ending in a NUL byte, as
/// `find -print0` writes them, because a file name can hold a line break.
pub type Reply = mpsc::Sender<Vec<u8>>;

/// One of `UNFINISHED_ANSWERS` while it lives.
struct UnfinishedAnswer;

impl UnfinishedAnswer {
    fn new() -> Self {
        UNFINISHED_ANSWERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        UnfinishedAnswer
    }
}

impl Drop for UnfinishedAnswer {
    fn drop(&mut self) {
        UNFINISHED_ANSWERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Commands from --remote, the selects that wait until the list is read, and how long the
/// window's ID shows.
pub(crate) struct Listener {
    /// Commands from `glimt --remote`.
    rx: mpsc::Receiver<(String, Option<remote::Reply>)>,
    /// A `--remote select` that came while the folder was being read or while Delete or Rename
    /// asked, and every command after it. They are done in the order they came, once neither
    /// is the case.
    waiting_selects: Vec<(String, Option<remote::Reply>)>,
    /// The time until which `--remote identify` shows the window's ID over the image.
    identify_until: Option<std::time::Instant>,
}

impl Listener {
    /// Listening for `--remote` if `listen`.
    pub(crate) fn new(ctx: &egui::Context, listen: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        if listen {
            self::listen(tx, ctx.clone());
        }
        Listener {
            rx,
            waiting_selects: Vec::new(),
            identify_until: None,
        }
    }

    /// The time that the window's ID still shows for, after `--remote identify`. It is `None`
    /// when that time has run out, or when no window was asked to show it.
    pub(crate) fn identify_time_left(&self) -> Option<std::time::Duration> {
        let now = std::time::Instant::now();
        self.identify_until
            .and_then(|until| until.checked_duration_since(now))
    }
}

impl crate::App {
    // ---- Remote control ----

    /// Receives `glimt --remote` commands and queries. A `select` waits while the folder is
    /// read or a dialog asks about the selection.
    pub(crate) fn receive_remote(&mut self, ctx: &egui::Context) {
        // After `open`, the folder is still being read, so its files are not all listed. A
        // dialog asking about the selection (Delete, Rename) is answered for the files it asked
        // about, not for a selection a script put in its place meanwhile.
        let busy = |app: &Self| app.folder.is_listing() || app.asking_about_selection();
        // A hidden window runs no `ui`, which takes the listing, so a waiting select takes it
        // here. Otherwise the select would wait until the window is shown again.
        if !self.remote.waiting_selects.is_empty() {
            self.receive_listing(ctx, std::time::Duration::ZERO);
        }
        // The waiting ones are done in the order they came, until one (an `open`) makes the
        // window busy again. Each gets its own answer, from the list as it was when it was sent.
        while !self.remote.waiting_selects.is_empty() && !busy(self) {
            let (command, reply) = self.remote.waiting_selects.remove(0);
            self.handle_remote(ctx, &command, reply);
        }
        while let Ok((command, reply)) = self.remote.rx.try_recv() {
            let select = reply.is_some() && command.starts_with("select ");
            let wait = (may_relist(&command) && !self.remote.waiting_selects.is_empty())
                || (select && busy(self));
            if !wait {
                self.handle_remote(ctx, &command, reply);
            } else if self.remote.waiting_selects.len() < 64 {
                self.remote.waiting_selects.push((command, reply));
            }
            // When more than that are waiting, the answer is dropped, and the client is told
            // "no answer" (serve), so the script tries again.
        }
    }

    /// Answers each select still waiting at closing, with all its paths and none of them
    /// selected. Left without an answer, the closed socket would read as "all found". It also
    /// answers those sent just before and not yet taken from the channel, and a query is
    /// answered as usual (current, selection).
    pub(crate) fn answer_waiting_selects(&mut self) {
        let sent: Vec<_> = self.remote.rx.try_iter().collect();
        let waiting = std::mem::take(&mut self.remote.waiting_selects);
        let ctx = self.ctx.clone();
        for (command, reply) in waiting.into_iter().chain(sent) {
            let Some(reply) = reply else { continue };
            match command.strip_prefix("select ") {
                Some(paths) => {
                    let paths = paths.split('\0').filter(|p| !p.is_empty());
                    let _ = reply.send(paths.flat_map(|p| p.bytes().chain([0])).collect());
                }
                // A command is not done as the window closes, and the client is told so.
                None if !is_query(&command) => {
                    let _ = reply.send(not_done_answer("this Glimt is closing"));
                }
                // Answered as at any other time (`where` too, in the release tests' build).
                None => self.handle_remote(&ctx, &command, Some(reply)),
            }
        }
    }

    /// One command from `glimt --remote`, with where to send its answer. A query is answered
    /// with what it asks for, and any other command with nothing when it was done, or with
    /// `NOT_DONE_ANSWER` and the reason when it was not.
    pub(crate) fn handle_remote(
        &mut self,
        ctx: &egui::Context,
        command: &str,
        reply: Option<Reply>,
    ) {
        match reply {
            Some(reply) if command.starts_with("select ") => {
                let _ = reply.send(self.remote_select(ctx, command));
            }
            #[cfg(feature = "test-hooks")]
            Some(reply) if command.starts_with("where ") => {
                let _ = reply.send(test_hooks::label_position(
                    command.trim_start_matches("where "),
                ));
            }
            Some(reply) if is_query(command) => drop(reply.send(self.remote_query(command))),
            Some(reply) => {
                let answer = match self.remote_command(ctx, command) {
                    Ok(()) => Vec::new(),
                    Err(why) => not_done_answer(&why),
                };
                let _ = reply.send(answer);
            }
            None => {
                let _ = self.remote_command(ctx, command);
            }
        }
    }

    /// Runs a command from `glimt --remote`. The commands are next, previous, first, last,
    /// slideshow, random and fullscreen (each with on or off, or without to go to the other),
    /// subfolders on|off, quit, open PATH, title with this window's own title, or without one
    /// to have the title in Preferences, and run ACTION with an action of `SCRIPT_ACTIONS`.
    /// Returns the reason when the command is not done: an action refused (`why_refused`, such
    /// as a kiosk's), or a command this window does not know, such as one of a newer Glimt.
    pub(crate) fn remote_command(
        &mut self,
        ctx: &egui::Context,
        command: &str,
    ) -> Result<(), String> {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        let command = command.trim_start();
        let action = match command.trim_end() {
            "next" => Some(Action::Next),
            "previous" => Some(Action::Previous),
            "first" => Some(Action::First),
            "last" => Some(Action::Last),
            // With on or off, a script can count on the state, and nothing happens when it is so
            // already.
            "slideshow on" if self.slideshow.is_running() => None,
            "slideshow off" if !self.slideshow.is_running() => None,
            "slideshow" | "slideshow on" | "slideshow off" => Some(Action::Slideshow),
            "random" => Some(Action::RandomOrder),
            "random on" => {
                self.set_random_order(ctx, true);
                None
            }
            "random off" => {
                self.set_random_order(ctx, false);
                None
            }
            "fullscreen on" if fullscreen => None,
            "fullscreen off" if !fullscreen => None,
            "fullscreen" | "fullscreen on" | "fullscreen off" => Some(Action::FullScreen),
            "quit" => {
                // The only way out of a kiosk.
                self.window.let_kiosk_quit();
                Some(Action::Quit)
            }
            "subfolders on" => {
                self.set_subfolders(ctx, true);
                None
            }
            "subfolders off" => {
                self.set_subfolders(ctx, false);
                None
            }
            "title" => {
                self.set_window_title(ctx, "");
                None
            }
            "identify" => {
                self.remote.identify_until = Some(std::time::Instant::now() + IDENTIFY_TIME);
                ctx.request_repaint();
                None
            }
            // Not while a dialog asks the user or a text field has the focus, as the keys are
            // not then either. An action could take the dialog's place (WindowTitle), or take
            // the focus from the field the user types in (FocusSearch). Only `run` waits, as it
            // stands for the keys. The named commands (next, open, select, …) are a remote
            // control's buttons, which must not stop because a field kept the focus.
            _ if command.trim().starts_with("run ")
                && (self.dialog_open() || ctx.text_edit_focused()) =>
            {
                return Err("a dialog is open or a text field has the focus".into());
            }
            other if other.starts_with("run ") => {
                let name = other["run ".len()..].trim();
                let action = Action::for_script(name);
                Some(action.ok_or_else(|| format!("{name} is not an action a script may run"))?)
            }
            other if other.starts_with("title ") => {
                self.set_window_title(ctx, other["title ".len()..].trim());
                None
            }
            other if other.starts_with("open ") => {
                // The path is taken as it was sent, not trimmed, because a folder's name may end
                // in a space.
                if let Some(path) = command.trim_start().strip_prefix("open ") {
                    self.visit(ctx, PathBuf::from(path));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                None
            }
            // The words of a newer Glimt, or a choice this one does not have.
            other => return Err(format!("this Glimt does not know the command “{other}”")),
        };
        if let Some(action) = action {
            if let Some(why) = self.why_refused(action) {
                return Err(why.into());
            }
            self.run(ctx, action);
        }
        Ok(())
    }

    /// The answer to `glimt --remote current` or `selection`, the paths, each ending in a NUL
    /// byte. Nothing else is told, and nothing changes.
    // shortcut: the paths of a selection made in the window's thread, some 50 ms for 500,000
    // selected; in a thread of its own if scripts ask for large selections often.
    pub(crate) fn remote_query(&self, command: &str) -> Vec<u8> {
        use std::os::unix::ffi::OsStringExt;
        let files = match command.trim() {
            "current" => vec![self.list.shown()],
            // When the search hides every selected file, the answer names none of them, and
            // not the shown image in their place.
            _ => self.targets_without_notice().unwrap_or_default(),
        };
        (files.into_iter().filter(|&i| i < self.list.files().len()))
            .flat_map(|i| {
                let path = self.path(i).into_os_string().into_vec();
                path.into_iter().chain([0])
            })
            .collect()
    }

    /// `--remote select PATH…` (the paths after a NUL each). It selects those of the list and
    /// shows the first. The answer is the paths that are not in the list, each ending in a NUL
    /// byte. Only the selection changes, and deleting or moving it is left to the user. When
    /// the search hides one of the files, nothing changes, and the answer is every path.
    pub(crate) fn remote_select(&mut self, ctx: &egui::Context, command: &str) -> Vec<u8> {
        let paths = command.strip_prefix("select ").unwrap_or_default();
        let paths: Vec<&str> = paths.split('\0').filter(|p| !p.is_empty()).collect();
        // The list's name of `p`, relative to the folder shown.
        fn name<'a>(dir: &Path, p: &'a str) -> Option<&'a std::ffi::OsStr> {
            Some(Path::new(p).strip_prefix(dir).ok()?.as_os_str())
        }
        let dir = self.folder.dir().to_path_buf();
        let names: HashSet<&std::ffi::OsStr> = paths.iter().filter_map(|p| name(&dir, p)).collect();
        let found: std::collections::BTreeSet<usize> = (self.list.files().iter().enumerate())
            .filter(|(_, f)| names.contains(f.as_os_str()))
            .map(|(i, _)| i)
            .collect();
        let listed: HashSet<&std::ffi::OsStr> = found
            .iter()
            .map(|&i| self.list.files()[i].as_os_str())
            .collect();
        let missing: Vec<u8> = (paths.iter())
            .filter(|p| !name(&dir, p).is_some_and(|n| listed.contains(n)))
            .flat_map(|p| p.bytes().chain([0]))
            .collect();
        if let Some(&first) = found.first() {
            // When the search hides one of them, none is selected, and the answer names every
            // path, as at closing.
            if !self.select_for_action(ctx, found) {
                return paths.iter().flat_map(|p| p.bytes().chain([0])).collect();
            }
            self.show_file(ctx, first);
            self.list.set_anchor(None);
        }
        missing
    }
}

/// Returns the window's answer to a command it did not do, with the reason `why`.
fn not_done_answer(why: &str) -> Vec<u8> {
    format!("{NOT_DONE_ANSWER}{why}\n").into_bytes()
}

/// Returns the reason in a window's answer `answer`, or `None` when the answer says that the
/// command was done, or is a query's.
fn not_done_reason(answer: &[u8]) -> Option<String> {
    let why = answer.strip_prefix(NOT_DONE_ANSWER.as_bytes())?;
    Some(String::from_utf8_lossy(why).trim_end().to_owned())
}

/// Whether `command` is a query, which answers with what it asks for. None of them changes a
/// file. `select` only selects, and what is done with the selection is the user's to do in the
/// window.
fn is_query(command: &str) -> bool {
    matches!(command.trim(), "current" | "selection")
        || command.starts_with("select ")
        || command.starts_with("where ")
}

/// Glimt's runtime folder, which only the user can reach. It holds the sockets and the sidecar
/// locks.
pub(crate) fn runtime_dir() -> Option<PathBuf> {
    let mut dir = runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR"))?;
    if let Some(id) = std::env::var_os("FLATPAK_ID") {
        dir = dir.join("app").join(id);
    }
    let dir = dir.join("glimt");
    // A folder of this name may be another user's, where $XDG_RUNTIME_DIR is not private as it
    // should be (/tmp). Its sockets would be theirs to answer, and its locks theirs to hold.
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    is_own_or_missing(&dir, uid).then_some(dir)
}

/// Returns `$XDG_RUNTIME_DIR` as given in `set`, or `None` when it is unset, empty or relative.
/// A relative one would put the sockets and locks in the current folder, and the XDG spec says
/// to ignore it, as `config::xdg_base` does for the other XDG folders.
fn runtime_dir_from(set: Option<std::ffi::OsString>) -> Option<PathBuf> {
    set.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Whether `dir` is a folder of user `uid`, not a link, or not there yet (made by `listen`).
fn is_own_or_missing(dir: &std::path::Path, uid: u32) -> bool {
    match std::fs::symlink_metadata(dir) {
        Ok(m) => m.is_dir() && m.uid() == uid,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// This Glimt's name. It is its process ID, and in a Flatpak, where every instance can have
/// the same process ID, the Flatpak instance ID too (a new window from Ctrl+N shares the
/// instance). It is found once, because every new thumbnail asks.
pub fn instance_id() -> &'static str {
    static INSTANCE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE_ID.get_or_init(|| {
        let info = (crate::files::external::in_flatpak())
            .then(|| crate::images::decode::read_text("/.flatpak-info").ok())
            .flatten();
        let instance = info
            .as_deref()
            .and_then(|i| i.lines().find_map(|l| l.strip_prefix("instance-id=")));
        let pid = std::process::id();
        instance.map_or_else(|| pid.to_string(), |i| format!("{i}-{pid}"))
    })
}

/// Listen as `name` instead of the process ID.
pub fn set_name(name: String) {
    let _ = GIVEN_NAME.set(name);
}

/// The `--name` given, if any.
pub fn given_name() -> Option<&'static str> {
    GIVEN_NAME.get().map(String::as_str)
}

/// Whether `name` is a process ID, which has only digits. With `--name` it reaches the Glimt of
/// that process ID with `--remote`, but a window never takes it as its own name, because it
/// could be another Glimt's.
pub fn is_process_id(name: &str) -> bool {
    !name.is_empty() && name.len() <= 10 && name.chars().all(|c| c.is_ascii_digit())
}

/// Whether `name` cannot be taken for a process ID or a path. It holds letters, digits, - and
/// _, and is short enough for the socket's path (at most 108 bytes).
pub fn valid_name(name: &str) -> bool {
    let ok = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    name.chars().all(ok) && !name.chars().all(|c| c.is_ascii_digit()) && name.len() <= 64
}

/// Whether the socket of `--name name` fits in the 107 bytes a socket's path may have here. In
/// a Flatpak its folder alone takes some 60.
pub fn name_fits(name: &str) -> bool {
    socket(name).is_none_or(|p| p.as_os_str().len() <= 107)
}

/// Returns whether `command` may read the folder again or open another, so that behind a waiting
/// select it waits its turn, and the select is answered for the list it was sent to: another
/// script's open, subfolders, a slideshow with its subfolders, and run, whose Up, Home, Back or
/// Refresh read a folder. Other queries and commands are done at once, as without it.
fn may_relist(command: &str) -> bool {
    ["open ", "subfolders", "slideshow", "run "]
        .iter()
        .any(|c| command.starts_with(c))
}

/// Whether `words` is one of `COMMANDS`, with a path where it takes one and none where it does
/// not. `open` takes one path, and `select` any number of files. A choice in brackets
/// ([on|off]) may be left out, as may `title`'s words.
pub fn is_known_command(words: &[String]) -> bool {
    (COMMANDS.iter()).any(|(c, _)| {
        let mut parts = c.split(' ');
        if parts.next() != words.first().map(String::as_str) {
            return false;
        }
        match parts.next() {
            // A choice of words ("on|off") takes one of them, alone. In brackets, it may take
            // none.
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

/// Returns `command` as it is named now: its first word becomes the new word when that word is
/// renamed in `RENAMED_COMMANDS`, and the words after it stay. Each command goes through this
/// once, where it comes in: in the client before any check (`words_now`), and in the window as
/// the listener reads it (`serve`). So no code after them reads an old word.
pub fn command_now(command: &str) -> String {
    let word = command.split(' ').next().unwrap_or_default();
    match RENAMED_COMMANDS.iter().find(|r| r.0 == word) {
        Some((_, new)) => format!("{new}{}", &command[word.len()..]),
        None => command.to_owned(),
    }
}

/// Returns `words`, the command and its arguments as given, with the command's word as it is
/// named now (`command_now`).
fn words_now(words: &[String]) -> Vec<String> {
    let mut now = words.to_vec();
    if let Some(word) = now.first_mut() {
        *word = command_now(word);
    }
    now
}

/// Whether a window took a command but gave no answer within `WAIT`.
pub fn timed_out() -> bool {
    TIMED_OUT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a window answered that it did not do the command.
fn was_not_done() -> bool {
    WAS_NOT_DONE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The ID that this Glimt listens as, which is its --name or else its process ID. The %id% of
/// the window title and `--remote identify` show it.
pub fn socket_id() -> &'static str {
    given_name().unwrap_or_else(instance_id)
}

/// The ID of the socket that this Glimt listens on, which `--remote` reaches this window alone
/// by. It is `socket_id()` until the socket is bound.
fn own_socket_id() -> &'static str {
    LISTENING_AS.get().map_or_else(socket_id, String::as_str)
}

/// Whether the socket at `path` is one of the windows of the --name `name`: the first one's
/// `glimt-NAME.sock`, or `glimt-NAME.ID.sock` of a window that shares the name. A name holds no
/// dot, so the part before the first dot is the name.
fn is_named(path: &std::path::Path, name: &str) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str());
    let id = stem.and_then(|s| s.strip_prefix("glimt-"));
    id.and_then(|id| id.split('.').next()) == Some(name)
}

fn socket(id: &str) -> Option<PathBuf> {
    runtime_dir().map(|d| d.join(socket_name(id)))
}

/// Returns the name of the socket of the Glimt named `id` (its --name or process ID) in the
/// runtime folder.
pub fn socket_name(id: &str) -> String {
    format!("glimt-{id}.sock")
}

/// Send `command` to the socket at `path`, and print what it answers to standard output. A
/// command that the window did not do has its reason printed on standard error instead. A
/// window of 0.75 or older closes without an answer to a command, which reads as done.
fn send_and_print_answer(path: &std::path::Path, command: &str) -> bool {
    use std::io::Read;
    let Ok(mut s) = UnixStream::connect(path) else {
        return false;
    };
    if writeln!(s, "{command}").is_err() {
        return false;
    }
    // Sending is done, so the window answers and then closes.
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(WAIT));
    let mut answer = Vec::new();
    #[allow(clippy::disallowed_methods)] // a reply on Glimt's own private socket, with a time limit
    let read = s.read_to_end(&mut answer);
    if let Err(e) = read {
        eprintln!("glimt: no answer from {}: {e}", path.display());
        TIMED_OUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    match not_done_reason(&answer) {
        Some(why) => {
            eprintln!("glimt: not done: {why}");
            WAS_NOT_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        None => drop(std::io::stdout().write_all(&answer)),
    }
    true
}

/// The sockets in `dir` that a Glimt answers on. One that does not answer is left alone,
/// because a Glimt that is starting refuses connections between binding its socket and
/// listening on it, and removing it then would leave that Glimt without remote control.
/// shortcut: the socket of a Glimt that crashed stays until the runtime folder is cleared at
/// logout, and costs one refused connection each time; remove such sockets at start, under
/// `bind_socket`'s lock, if they ever pile up.
fn live_sockets(dir: Option<PathBuf>) -> Vec<PathBuf> {
    let entries = dir.and_then(|d| std::fs::read_dir(d).ok());
    entries
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter(|p| UnixStream::connect(p).is_ok())
        .collect()
}

/// Sends `command` to the Glimt used last, or else to another one. Returns false if none runs.
pub fn send(command: &str) -> bool {
    let last = runtime_dir()
        .and_then(|d| crate::images::decode::read_text(d.join(FOCUSED_FILE)).ok())
        .and_then(|id| socket(id.trim()));
    if last.is_some_and(|p| send_and_print_answer(&p, command)) {
        return true;
    }
    live_sockets(runtime_dir())
        .iter()
        .any(|p| send_and_print_answer(p, command))
}

/// Sends `command` to every Glimt started with `--name name`, as several windows may share a
/// name. Returns false if none runs.
pub fn send_to(name: &str, command: &str) -> bool {
    let named = live_sockets(runtime_dir())
        .into_iter()
        .filter(|p| is_named(p, name));
    named.filter(|p| send_and_print_answer(p, command)).count() > 0
}

/// Send `command` to every running Glimt. Returns how many got it.
pub fn send_all(command: &str) -> usize {
    live_sockets(runtime_dir())
        .iter()
        .filter(|p| send_and_print_answer(p, command))
        .count()
}

/// `glimt --remote` and `--remote-all`. It sends `words` (the command and its arguments) to the
/// Glimt used last, to the one of `name`, or to every one, and `paths` are `select`'s files,
/// whole. Returns `None` once the command is done and its answer printed, or the path to open
/// when `open` finds no Glimt running. It exits with 2 for a command that cannot be sent, with
/// 1 when no Glimt takes it, and with 3 when a window did not do it.
pub fn client(
    words: &[String],
    paths: &[std::ffi::OsString],
    name: Option<&str>,
    to_all: bool,
) -> Option<PathBuf> {
    // The command is checked by its word now, and sent with the word as given, so an older
    // Glimt still takes an old word.
    let now = words_now(words);
    if !is_known_command(&now) {
        eprintln!("glimt: no such command, or not with these arguments; glimt --help lists them");
        std::process::exit(NOT_SENT);
    }
    if now[0] == "run" && Action::for_script(&now[1]).is_none() {
        eprintln!(
            "glimt: {} is not an action a script may run; glimt --help lists them",
            words[1]
        );
        std::process::exit(NOT_SENT);
    }
    let mut command = words.join(" ");
    // select sends whole paths after a NUL each, because names may hold spaces. A line ends
    // the command.
    if now[0] == "select" {
        if paths.len() > MAX_SELECT {
            eprintln!("glimt: select takes at most {} files", MAX_SELECT);
            std::process::exit(NOT_SENT);
        }
        let mut sent = Vec::new();
        for p in paths {
            // The path is made as the window has its folder, with links followed, because
            // ~/Pictures may be one.
            let p = crate::model::listing::real_path(std::path::Path::new(p));
            match p.to_str().filter(|t| !t.contains('\n')) {
                Some(text) => sent.push(text.to_owned()),
                None => {
                    eprintln!("glimt: cannot send {p:?}: not UTF-8, or a line break in it");
                    std::process::exit(NOT_SENT);
                }
            }
        }
        command = format!("{} {}", words[0], sent.join("\0"));
    }
    // A name that is not UTF-8 cannot go in the text sent, and made lossy it would name no
    // file. With no Glimt to send it to, this one opens it, by its own name.
    if now[0] == "open"
        && let Some(path) = paths.iter().find(|p| p.to_str().is_none())
    {
        let running = match name {
            Some(name) => socket(name).is_some_and(|p| UnixStream::connect(p).is_ok()),
            None => !live_sockets(runtime_dir()).is_empty(),
        };
        if !running {
            return Some(path.into());
        }
        eprintln!("glimt: cannot send {path:?}: not UTF-8");
        std::process::exit(NOT_SENT);
    }
    // The running Glimt has another working folder, so the full path is sent.
    let open_path = |command: &str| {
        let path = command.strip_prefix(words[0].as_str())?.strip_prefix(' ');
        path.filter(|_| now[0] == "open").map(str::to_owned)
    };
    if let Some(path) = open_path(&command) {
        let path = std::path::absolute(&path).unwrap_or_else(|_| path.into());
        command = format!("{} {}", words[0], path.display());
    }
    // A line ends a command at the window, so a line break in a file name would be read as a
    // command of its own, such as quit.
    if command.contains('\n') {
        eprintln!("glimt: cannot send a line break in a command or file name");
        std::process::exit(NOT_SENT);
    }
    let sent = match (name, to_all) {
        (_, true) => send_all(&command) > 0,
        (Some(name), false) => send_to(name, &command),
        (None, false) => send(&command),
    };
    if timed_out() {
        std::process::exit(NOT_TAKEN);
    }
    if was_not_done() {
        std::process::exit(NOT_DONE);
    }
    if sent {
        return None;
    }
    match open_path(&command) {
        Some(path) => Some(path.into()),
        None => {
            match name {
                Some(name) => eprintln!("No Glimt named {name} is running"),
                None => eprintln!("Glimt is not running"),
            }
            std::process::exit(NOT_TAKEN);
        }
    }
}

/// Listen for commands in the background and pass each line to `tx`, with where to answer for
/// a query.
pub fn listen(tx: mpsc::Sender<(String, Option<Reply>)>, ctx: eframe::egui::Context) {
    let Some(path) = socket(socket_id()) else {
        return match runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR")) {
            None => eprintln!("No remote control: $XDG_RUNTIME_DIR is not set to a full path"),
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
        #[allow(clippy::disallowed_methods)] // the runtime folder of the sockets
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // A folder made by another user meanwhile, between the look in `dir` and here, is not
        // used.
        if is_another_users_dir(dir) {
            return eprintln!("No remote control: its folder there is not this user's own");
        }
    }
    // A window whose --name is taken already shares it, on a socket of its own: the name and
    // its process ID.
    let mut id = socket_id().to_owned();
    let mut bound = bind_socket(&path);
    if let (Err(None), Some(name)) = (&bound, given_name()) {
        id = format!("{name}.{}", instance_id());
        bound = socket(&id).map_or(Err(None), |p| bind_socket(&p));
    }
    let listener = match bound {
        Ok(listener) => listener,
        Err(None) => {
            return eprintln!("A Glimt of ID {id} is running already: --remote goes to that one");
        }
        Err(Some(e)) => return eprintln!("No remote control as {id}: {e}"),
    };
    let _ = LISTENING_AS.set(id);
    LISTENING.store(true, std::sync::atomic::Ordering::Relaxed);
    // The accept thread below keeps the listener, so its number stays valid for
    // `has_waiting_client`.
    LISTENER.store(
        std::os::fd::AsRawFd::as_raw_fd(&listener),
        std::sync::atomic::Ordering::Relaxed,
    );
    mark_focused();
    std::thread::spawn(move || {
        serve_clients(listener, MOST_CLIENTS, move |stream| {
            serve(stream, &tx, &ctx)
        });
    });
}

/// Takes the clients of `listener`, each served by `serve` in a thread of its own, and at most
/// `most` at once. A thread for each client means that a client waiting for the window (a
/// folder of 500,000 files still being read, a dialog open) holds up no other. The window's
/// answer is waited for as long as it takes, and the client has the one time limit, so there is
/// one outcome when it runs out: "no answer" and exit 1, never an empty answer taken for "all
/// found". A client over `most` waits in the socket's queue, so clients that never end cannot
/// take a thread each.
fn serve_clients(
    listener: UnixListener,
    most: usize,
    serve: impl Fn(UnixStream) + Clone + Send + 'static,
) {
    // A free place is a message in this channel: taken before a client is accepted, and given
    // back when its thread is done.
    let (free, places) = mpsc::channel();
    for _ in 0..most {
        let _ = free.send(());
    }
    while places.recv().is_ok() {
        let Ok((stream, _)) = listener.accept() else {
            let _ = free.send(());
            // A lasting error, such as too many open files, would otherwise be a busy loop.
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        };
        // The client is counted from here, before its query is read, so a client that came
        // just before the socket went is waited for at closing too.
        let client = UnfinishedAnswer::new();
        let (serve, free) = (serve.clone(), free.clone());
        std::thread::spawn(move || {
            serve(stream);
            drop(client);
            let _ = free.send(());
        });
    }
}

/// One client's commands, each passed to the window, and its answer to a query sent back.
fn serve(
    stream: UnixStream,
    tx: &mpsc::Sender<(String, Option<Reply>)>,
    ctx: &eframe::egui::Context,
) {
    let _ = stream.set_read_timeout(Some(WAIT));
    let mut out = stream.try_clone().ok();
    // A client may send as much as `select` may, and no more, so a client that sends without
    // end does not fill the memory.
    let most = MAX_SELECT as u64 * 4096 + 64;
    let mut reader = std::io::BufReader::new(std::io::Read::take(stream, most));
    while let Some(line) = read_command(&mut reader) {
        let line = match line {
            Ok(line) => command_now(line.trim_start()),
            // Told, as an end without an answer would read as done. The rest of what the client
            // sends is read first, up to `most`, because closing with bytes unread resets the
            // connection, and the client then reads no answer.
            Err(why) => {
                let _ = std::io::copy(&mut reader, &mut std::io::sink());
                if let Some(out) = out.as_mut() {
                    let _ = out.write_all(&not_done_answer(why));
                }
                return;
            }
        };
        // Every command is answered, a query with what it asks and any other with whether it
        // was done, so a script knows when its `run` was refused.
        let (reply, answer) = mpsc::channel();
        // The client is counted until its answer is written, because `stop` waits for that at
        // closing.
        let answering = UnfinishedAnswer::new();
        if tx.send((line, Some(reply))).is_err() {
            return;
        }
        ctx.request_repaint();
        match answer.recv() {
            Ok(bytes) => {
                if let Some(out) = out.as_mut() {
                    let _ = out.write_all(&bytes);
                }
            }
            // When the window dropped the answer (too many waiting, or closing), closing the
            // connection now would make the client read nothing, "all found" or "done". Held
            // open, its time limit says "no answer".
            Err(_) => {
                drop(answering);
                return std::thread::sleep(WAIT + std::time::Duration::from_secs(1));
            }
        }
        drop(answering);
    }
}

/// Reads the next command, one line, from `reader`. A `select` may be as long as its files take,
/// and any other command at most `MOST_LINE` bytes, so a client that sends without a line break
/// holds no more memory than that. Returns the reason for a line too long, which the client is
/// told, and `None` at the end, for a line that is not UTF-8, and when the client sends nothing
/// within its time limit.
fn read_command(reader: &mut impl BufRead) -> Option<Result<String, &'static str>> {
    let mut line = Vec::new();
    std::io::Read::take(&mut *reader, MOST_LINE)
        .read_until(b'\n', &mut line)
        .ok()?;
    let ended = line.ends_with(b"\n");
    if !ended && line.trim_ascii_start().starts_with(b"select ") {
        reader.read_until(b'\n', &mut line).ok()?;
    } else if !ended && line.len() as u64 >= MOST_LINE {
        return Some(Err("too long"));
    }
    if line.is_empty() {
        return None;
    }
    let line = line.strip_suffix(b"\n").unwrap_or(&line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8(line.to_vec()).ok().map(Ok)
}

/// Whether `dir` is there and not the user's own folder.
fn is_another_users_dir(dir: &std::path::Path) -> bool {
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid());
    uid.is_ok_and(|uid| !is_own_or_missing(dir, uid))
}

/// Whether this Glimt listens for --remote commands.
pub fn listening() -> bool {
    LISTENING.load(std::sync::atomic::Ordering::Relaxed)
}

/// The socket at `path` for this Glimt. A socket left by a Glimt that did not close is replaced,
/// but not one that a running Glimt (of the same `--name`) answers on. Returns `Err(None)` when
/// a running Glimt answers on it, and `Err(Some(why))` when it cannot be made.
fn bind_socket(path: &std::path::Path) -> Result<UnixListener, Option<std::io::Error>> {
    // The socket is bound first, which fails if it is there, so two Glimts of one name started
    // at once cannot both find it free and then remove each other's.
    let first = match UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(e) => e,
    };
    if first.kind() != std::io::ErrorKind::AddrInUse {
        return Err(Some(first));
    }
    // From here only one at a time, because another Glimt of this name could take the left
    // socket over between our check and our remove, and lose its new one to us.
    #[allow(clippy::disallowed_methods)] // an empty lock file in the private runtime folder
    let lock = std::fs::File::create(path.with_extension("lock")).map_err(Some)?;
    lock.lock().map_err(Some)?;
    if UnixStream::connect(path).is_ok() {
        return Err(None);
    }
    #[allow(clippy::disallowed_methods)] // a socket left by a Glimt gone
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).map_err(Some)
}

/// Marks that this Glimt got the focus, so `--remote` goes to it from now on.
pub fn mark_focused() {
    if let Some(d) = runtime_dir() {
        // The file is written whole and never truncated first, because a `--remote` reading it
        // meanwhile got an empty name.
        let _ = crate::files::file_io::replace_file(&d.join(FOCUSED_FILE), own_socket_id());
    }
}

/// Waits, as Glimt closes, for the clients still connected, while `answer` answers the queries
/// they send meanwhile. `close` removed the socket before the last answers. It is not removed
/// again here, because a new Glimt of the same name may have it by now.
pub fn stop(answer: impl FnMut()) {
    if LISTENING.load(std::sync::atomic::Ordering::Relaxed) {
        wait_answered(answer, std::time::Duration::from_secs(1));
    }
}

/// Calls `answer` again and again until no client is left or `limit` has passed. So a query
/// that came after the window last looked is answered too, and an answer is written whole
/// before the process ends. A client left without one would read an empty answer, which for
/// select means "all found". That holds also for a client still queued for accept, because it
/// connected before `close` removed the socket. It must be quiet twice, 10 ms apart, so a client
/// the accept thread has just taken from the queue is counted.
fn wait_answered(mut answer: impl FnMut(), limit: std::time::Duration) {
    let until = std::time::Instant::now() + limit;
    let busy = || {
        UNFINISHED_ANSWERS.load(std::sync::atomic::Ordering::Relaxed) > 0
            || has_waiting_client(LISTENER.load(std::sync::atomic::Ordering::Relaxed))
    };
    let mut quiet = 0;
    while quiet < 2 && std::time::Instant::now() < until {
        quiet = if busy() { 0 } else { quiet + 1 };
        answer();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Whether a client waits to be accepted on the listening socket `fd` (-1 for none).
fn has_waiting_client(fd: i32) -> bool {
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
    // SAFETY: There is one pollfd and no wait, and a closed or negative fd is only reported,
    // not used.
    fd >= 0 && unsafe { poll(&mut p, 1, 0) } == 1 && p.revents & POLLIN != 0
}

/// Takes no new clients, by removing this Glimt's socket as it closes.
pub fn close() {
    if LISTENING.load(std::sync::atomic::Ordering::Relaxed)
        && let Some(path) = socket(own_socket_id())
    {
        #[allow(clippy::disallowed_methods)] // this Glimt's own socket
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;
    use eframe::egui;

    /// An empty or relative $XDG_RUNTIME_DIR is not used, so nothing goes into ./glimt.
    #[test]
    fn a_relative_runtime_dir_is_ignored() {
        let dir = |s: &str| super::runtime_dir_from(Some(s.into()));
        assert_eq!(dir(""), None);
        assert_eq!(dir("run"), None);
        assert_eq!(dir("/run/user/1000"), Some("/run/user/1000".into()));
        assert_eq!(super::runtime_dir_from(None), None);
    }

    /// The windows that share a --name are found by their sockets, the first one's and the
    /// others' with their process IDs, but not those of a longer name or of a process ID.
    #[test]
    fn a_shared_name_finds_each_window() {
        let named = |file: &str, name: &str| super::is_named(std::path::Path::new(file), name);
        assert!(named("/run/glimt/glimt-left.sock", "left"));
        assert!(named("/run/glimt/glimt-left.4242.sock", "left"));
        assert!(named("/run/glimt/glimt-left.app-1-4242.sock", "left"));
        assert!(!named("/run/glimt/glimt-left-2.sock", "left"));
        assert!(!named("/run/glimt/glimt-leftover.sock", "left"));
        assert!(!named("/run/glimt/glimt-4242.sock", "left"));
        assert!(named("/run/glimt/glimt-4242.sock", "4242"));
        assert!(!named("/run/glimt/focused", "focused"));
    }

    /// `identify` shows the window's ID for a few seconds and is told as done, and the client
    /// takes the word alone.
    #[test]
    fn identify_shows_the_id_for_a_while() {
        let dir = std::env::temp_dir().join(format!("glimt-test-identify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert_eq!(app.remote.identify_until, None);
        app.remote_command(&ctx, "identify").unwrap();
        let left = app.remote.identify_until.unwrap() - std::time::Instant::now();
        assert!(left > std::time::Duration::ZERO && left <= super::IDENTIFY_TIME);
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(super::is_known_command(&words("identify")));
        assert!(!super::is_known_command(&words("identify left")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `run ACTION` does an action a script may run, as its key would, and nothing for one that
    /// changes files, such as Delete, so no file goes, and no question is asked.
    #[test]
    fn run_does_only_the_actions_for_scripts() {
        let dir = std::env::temp_dir().join(format!("glimt-test-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.remote_command(&ctx, "run Next").unwrap();
        assert_eq!(app.list.shown(), 1);
        app.remote_command(&ctx, "run Last").unwrap();
        assert_eq!(app.list.shown(), 2);
        // The renamed command by its new word (its old word is mapped where it comes in, as
        // `an_old_command_word_reaches_the_window_mapped` checks), and the renamed action by
        // its old name.
        app.remote_command(&ctx, "previous").unwrap();
        assert_eq!(app.list.shown(), 1);
        app.remote_command(&ctx, "previous").unwrap();
        assert_eq!(app.list.shown(), 0);
        app.remote_command(&ctx, "run Next").unwrap();
        app.remote_command(&ctx, "run Prev").unwrap();
        assert_eq!(app.list.shown(), 0);
        app.remote_command(&ctx, "run Last").unwrap();
        app.config.confirm_trash = false;
        // Each is told as not done, so the script does not take it as done.
        app.remote_command(&ctx, "run Delete").unwrap_err();
        app.remote_command(&ctx, "run DeletePermanently")
            .unwrap_err();
        app.remote_command(&ctx, "run NoSuchAction").unwrap_err();
        assert!(!app.asking_about_selection());
        // Nor a script's action while a dialog asks the user: the Rename prompt stays.
        app.remote_command(&ctx, "run First").unwrap();
        assert_eq!(app.list.shown(), 0);
        let asked = crate::ui::dialogs::Asked::new(0, vec!["a.png".into()], None);
        let rename = crate::ui::dialogs::Prompt::Rename;
        app.dialogs.ask_about(asked, rename, "a.png".into());
        app.remote_command(&ctx, "run WindowTitle").unwrap_err();
        app.remote_command(&ctx, "run Last").unwrap_err();
        assert_eq!(app.list.shown(), 0);
        assert!(app.dialogs.asks_about_files());
        assert_eq!(app.list.files().len(), 3);
        assert!(dir.join("c.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A script's `run` does nothing while a text field has the focus, as the keys do nothing
    /// then. Before, `run FocusSearch` or `run Next` from a script took the focus from the
    /// field the user typed in, or changed what the field was about. The named commands still
    /// act, as a remote control's buttons.
    #[test]
    fn run_does_nothing_while_a_text_field_has_the_focus() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-run-typing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        let mut text = String::new();
        let mut field = None;
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                let response = egui::TextEdit::singleline(&mut text).show(ui).response;
                response.request_focus();
                field = Some(response.id);
            });
        }
        assert!(ctx.text_edit_focused());
        app.remote_command(&ctx, "run FocusSearch").unwrap_err();
        app.remote_command(&ctx, "run Next").unwrap_err();
        assert_eq!(app.list.shown(), 0);
        assert_eq!(ctx.memory(|m| m.focused()), field);
        app.remote_command(&ctx, "next").unwrap();
        assert_eq!(app.list.shown(), 1);
        ctx.memory_mut(|m| m.surrender_focus(field.unwrap()));
        app.remote_command(&ctx, "run Next").unwrap();
        assert_eq!(app.list.shown(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `run` that the window refuses is told as not done, with the window's reason. A kiosk
    /// refuses what is not for browsing, and an action on a file has none to act on in an
    /// empty list. Before, both were answered as done, so the script exited 0.
    #[test]
    fn a_run_refused_is_not_done() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-run-refused-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let why = app.remote_command(&ctx, "run CopyPath").unwrap_err();
        assert_eq!(why, "there is no image to act on");
        app.list.set_files(vec!["a.png".into(), "b.png".into()]);
        app.window = crate::ui::window::Window::new(true);
        let why = app.remote_command(&ctx, "run Toolbar").unwrap_err();
        assert!(why.contains("kiosk"), "{why}");
        // What a kiosk does is done as before.
        app.remote_command(&ctx, "run Next").unwrap();
        assert_eq!(app.list.shown(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The commands that can change the list wait behind a waiting select, run among them, and the
    /// others do not.
    #[test]
    fn what_may_change_the_list_waits_behind_a_select() {
        for command in [
            "open /a",
            "subfolders on",
            "slideshow",
            "run Up",
            "run Refresh",
        ] {
            assert!(super::may_relist(command), "{command}");
        }
        for command in ["next", "current", "selection", "title x", "quit"] {
            assert!(!super::may_relist(command), "{command}");
        }
    }

    /// Only the commands Glimt knows go out, with a path only where one is taken.
    #[test]
    fn knows_its_commands() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(super::is_known_command(&words("next")));
        // A renamed command is known by its old word too, as the client maps it before the
        // check, and takes no more words than before.
        let known_now = |s: &str| super::is_known_command(&super::words_now(&words(s)));
        assert!(known_now("previous") && known_now("prev"));
        assert!(!known_now("prev 2"));
        assert!(super::is_known_command(&["open".into(), "/a b.jpg".into()]));
        // Two words are two paths, not one with a space, because the shell joins no words.
        assert!(!super::is_known_command(&words("open /a.jpg /b.jpg")));
        assert!(super::is_known_command(&words("select /a.jpg /b.jpg")));
        assert!(!super::is_known_command(&words("nxet")));
        assert!(!super::is_known_command(&words("open")));
        assert!(super::is_known_command(&words("run ZoomIn")));
        assert!(!super::is_known_command(&words("run")));
        assert!(!super::is_known_command(&words("run ZoomIn ZoomOut")));
        assert!(!super::is_known_command(&words("next 2")));
        assert!(super::is_known_command(&words("subfolders on")));
        assert!(super::is_known_command(&words("title")));
        assert!(super::is_known_command(&words("title Left: %name%")));
        assert!(!super::is_known_command(&words("subfolders")));
        assert!(super::is_known_command(&words("subfolders off")));
        assert!(
            super::is_known_command(&words("slideshow"))
                && super::is_known_command(&words("slideshow off"))
        );
        assert!(
            !super::is_known_command(&words("slideshow yes"))
                && !super::is_known_command(&words("random on off"))
        );
        assert!(
            !super::is_known_command(&words("subfolders yes"))
                && !super::is_known_command(&words("subfolders on off"))
        );
        assert!(!super::is_known_command(&[]));
    }

    /// Each old word of `RENAMED_COMMANDS` reaches the window as its new word: the listener maps
    /// it before anything reads it, so the queries, the waits behind a select and
    /// `remote_command` never see the old word. `prev` goes on through the window and shows
    /// the image before.
    #[test]
    fn an_old_command_word_reaches_the_window_mapped() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("glimt-test-old-word-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png"].map(Into::into).to_vec());
        app.list.set_shown(1);
        let (tx, rx) = std::sync::mpsc::channel();
        let (mut client, window) = super::UnixStream::pair().unwrap();
        for (old, _) in super::RENAMED_COMMANDS {
            writeln!(client, "{old}").unwrap();
            writeln!(client, " {old}").unwrap();
        }
        drop(client);
        // The listener waits for each command's answer, so the window's part runs here.
        let window_ctx = ctx.clone();
        let server = std::thread::spawn(move || super::serve(window, &tx, &window_ctx));
        for (_, new) in super::RENAMED_COMMANDS {
            for _ in 0..2 {
                let (line, reply) = rx.recv().unwrap();
                assert_eq!(line, new.to_string());
                let _ = reply.unwrap().send(Vec::new());
            }
        }
        server.join().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (mut client, window) = super::UnixStream::pair().unwrap();
        writeln!(client, "prev").unwrap();
        drop(client);
        let window_ctx = ctx.clone();
        let server = std::thread::spawn(move || super::serve(window, &tx, &window_ctx));
        app.remote = super::Listener {
            rx,
            waiting_selects: Vec::new(),
            identify_until: None,
        };
        for _ in 0..500 {
            app.receive_remote(&ctx);
            if app.list.shown() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(app.list.shown(), 0);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A renamed command is read by its new word with the words after it kept, so a renamed
    /// command that takes words (`slideshow on`) works in the window as the client's check says.
    #[test]
    fn a_renamed_command_keeps_its_words() {
        assert_eq!(super::command_now("prev"), "previous");
        assert_eq!(super::command_now("prev on"), "previous on");
        assert_eq!(super::command_now("prevx on"), "prevx on");
        assert_eq!(super::command_now("next"), "next");
        assert_eq!(super::command_now(""), "");
    }

    /// The runtime folder is used when it is the user's own folder or not there yet. It is not
    /// used when it is another user's, a link, or a file.
    #[test]
    fn uses_only_its_own_runtime_folder() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        assert!(super::is_own_or_missing(&dir, uid));
        assert!(!super::is_own_or_missing(&dir, uid + 1));
        assert!(super::is_own_or_missing(&dir.join("none"), uid));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!super::is_own_or_missing(&link, uid));
        std::fs::write(dir.join("file"), "").unwrap();
        assert!(!super::is_own_or_missing(&dir.join("file"), uid));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// At closing, a client that sends its query after the window last answered is answered
    /// too. The window is asked again until no client is left, not only waited for.
    #[test]
    fn answers_a_query_that_comes_while_closing() {
        let mut client = Some(super::UnfinishedAnswer::new());
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

    /// A client that connected before the socket went, but that the accept thread has not taken
    /// yet, is seen as waiting, so closing waits for it too.
    #[test]
    fn sees_a_client_queued_for_accept() {
        let path = std::env::temp_dir().join(format!("glimt-queued-{}.sock", std::process::id()));
        let listener = super::bind_socket(&path).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
        assert!(!super::has_waiting_client(fd));
        let _client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(super::has_waiting_client(fd));
        let _served = listener.accept().unwrap();
        assert!(!super::has_waiting_client(fd));
        assert!(!super::has_waiting_client(-1));
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    /// Every command is answered: with nothing when the window did it, and with "not done:"
    /// and the reason when it did not, as a `run` refused while a dialog is open or a command
    /// the window does not know. Before, any command but a query was taken as done, so a script
    /// went on after a refused `run`, and a newer client could not tell that an older window
    /// did not know its command. The client reads the reason from the answer.
    #[test]
    fn a_command_not_done_is_told() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("glimt-test-not-done-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png"].map(Into::into).to_vec());
        let answer = |app: &mut crate::App, command: &str| {
            let (reply, answer) = std::sync::mpsc::channel();
            app.handle_remote(&ctx, command, Some(reply));
            answer.try_recv().unwrap()
        };
        assert_eq!(answer(&mut app, "next"), b"");
        assert_eq!(app.list.shown(), 1);
        for command in ["nxet", "slideshow maybe", "run Delete", "run NoSuchAction"] {
            let why = super::not_done_reason(&answer(&mut app, command));
            assert!(why.is_some_and(|w| !w.is_empty()), "{command}");
        }
        let asked = crate::ui::dialogs::Asked::new(0, vec!["a.png".into()], None);
        app.dialogs
            .ask_about(asked, crate::ui::dialogs::Prompt::Rename, "a.png".into());
        let refused = answer(&mut app, "run First");
        assert_eq!(
            super::not_done_reason(&refused).as_deref(),
            Some("a dialog is open or a text field has the focus")
        );
        assert_eq!(app.list.shown(), 1);
        // A query's answer of paths is never read as "not done".
        assert_eq!(super::not_done_reason(b"/a/not done: x\0"), None);
        // The listener writes the window's answer back to the client.
        let (mut client, window) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || super::serve(window, &tx, &eframe::egui::Context::default()));
        writeln!(client, "nxet").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let (line, reply) = rx.recv().unwrap();
        app.handle_remote(&ctx, &line, reply);
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        assert!(super::not_done_reason(&got).is_some(), "{got:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A command line other than a select is read up to `MOST_LINE` bytes, and a longer one ends
    /// the client, which is told that it is too long. A select may be as long as its files.
    /// Before, every line could be as long as the longest select, some 400 MB, and ten clients
    /// sending without a line break held 4 GB. And a longer line ended without an answer, which
    /// the client read as done.
    #[test]
    fn a_command_line_is_cut_short_unless_it_is_a_select() {
        let long = "x".repeat(super::MOST_LINE as usize + 10);
        // The lines read, up to the first that is refused, as `serve` reads them.
        let read = |text: String| {
            let mut reader = std::io::Cursor::new(text.into_bytes());
            let mut lines = Vec::new();
            while let Some(line) = super::read_command(&mut reader) {
                let refused = line.is_err();
                lines.push(line);
                if refused {
                    break;
                }
            }
            lines
        };
        // The client is told "too long", rather than an end that reads as done.
        assert_eq!(read(format!("title {long}\nnext\n")), [Err("too long")]);
        let select = format!("select /{long}\0");
        assert_eq!(
            read(format!("{select}\r\nnext\n")),
            [Ok(select.clone()), Ok("next".into())]
        );
        assert_eq!(read("next".into()), [Ok("next".into())]);
        assert!(read(String::new()).is_empty());
    }

    /// A line too long, longer too than the socket's buffer, reaches the client over a real
    /// socket as "not done: too long". Before, the window closed with the rest unread, and the
    /// client's write or read failed on the reset, so it exited with "no answer".
    #[test]
    fn a_line_too_long_is_answered_over_the_socket() {
        use std::io::{Read, Write};
        let (tx, _rx) = std::sync::mpsc::channel();
        let (mut client, window) = super::UnixStream::pair().unwrap();
        let ctx = eframe::egui::Context::default();
        let server = std::thread::spawn(move || super::serve(window, &tx, &ctx));
        let long = "x".repeat(4 * super::MOST_LINE as usize);
        client
            .write_all(format!("title {long}\n").as_bytes())
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        assert_eq!(super::not_done_reason(&got).as_deref(), Some("too long"));
        server.join().unwrap();
    }

    /// At most so many clients are served at once, each by a thread; the next waits until one
    /// is done. Before, every client that connected got a thread of its own, with no end.
    #[test]
    fn serves_at_most_so_many_clients_at_once() {
        let path = std::env::temp_dir().join(format!("glimt-clients-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (started, starts) = std::sync::mpsc::channel();
        // Each client is served until it closes.
        let serve = move |mut stream: std::os::unix::net::UnixStream| {
            let _ = started.send(());
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        };
        std::thread::spawn(move || super::serve_clients(listener, 2, serve));
        let connect = || std::os::unix::net::UnixStream::connect(&path).unwrap();
        let (first, _second, _third) = (connect(), connect(), connect());
        let wait = |ms| starts.recv_timeout(std::time::Duration::from_millis(ms));
        assert!(wait(5000).is_ok() && wait(5000).is_ok());
        assert!(wait(300).is_err(), "a third client was served beside two");
        drop(first);
        assert!(
            wait(5000).is_ok(),
            "the third client was not served when one was done"
        );
        std::fs::remove_file(path).unwrap();
    }

    /// A query whose answer the window drops (too many waiting) is not closed at once, because
    /// the client would read an empty answer, "all found". It is left to time out, "no answer".
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

    /// A socket that does not answer is passed over by `--remote-all` but left in place. A
    /// Glimt between binding it and listening refuses connections just as a crashed one's
    /// socket does, and `live_sockets` removed it, so that Glimt could not be reached.
    #[test]
    fn a_socket_not_answering_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (quiet, answering) = (dir.join("glimt-1.sock"), dir.join("glimt-2.sock"));
        drop(std::os::unix::net::UnixListener::bind(&quiet).unwrap());
        let _listening = std::os::unix::net::UnixListener::bind(&answering).unwrap();
        // A process another test starts holds a copy of the closed socket until it runs its
        // program, so it can answer a moment longer, as in `claims_only_a_left_socket`.
        let passed_over = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            super::live_sockets(Some(dir.clone())) == [answering.clone()]
        });
        assert!(passed_over);
        assert!(quiet.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A socket that a Glimt answers on is kept, and one it left behind is taken over.
    #[test]
    fn claims_only_a_left_socket() {
        let path = std::env::temp_dir().join(format!("glimt-claim-{}.sock", std::process::id()));
        let first = super::bind_socket(&path).unwrap();
        assert!(super::bind_socket(&path).is_err_and(|e| e.is_none()));
        drop(first);
        // A process another test starts holds a copy of the socket until it runs its program,
        // so it can answer a moment longer.
        let taken = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            super::bind_socket(&path).is_ok()
        });
        assert!(taken);
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    /// `--remote select` of a file the search hides selects nothing and answers every path, so
    /// the script does not take the files as selected. The selection stays as it was.
    #[test]
    fn a_select_of_a_file_the_search_hides_selects_nothing() {
        let dir = std::env::temp_dir().join(format!("glimt-test-sel-hid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.list.select([2].into());
        app.list.set_matches(Some(vec![1, 2]));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let command = format!("select {}\0{}\0", a.display(), b.display());
        let answer = app.remote_select(&ctx, &command);
        assert_eq!(
            answer,
            format!("{}\0{}\0", a.display(), b.display()).into_bytes()
        );
        assert_eq!(app.list.selected(), &[2].into());
        assert!(app.notice.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two `--remote select` waiting while the folder is read each get their answer. The
    /// second took the first one's place, and the first script got no answer, which it read as
    /// every file found.
    #[test]
    fn waiting_selects_each_get_their_answer() {
        let dir = std::env::temp_dir().join(format!("glimt-test-selects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        // A folder still being read.
        let (_reading, listing) = std::sync::mpsc::channel();
        app.folder.hold_listing(listing);
        let mut answers = Vec::new();
        for name in ["a.png", "none.png"] {
            let (reply, answer) = std::sync::mpsc::channel();
            let command = format!("select {}\0", dir.join(name).display());
            tx.send((command, Some(reply))).unwrap();
            answers.push(answer);
        }
        app.receive_remote(&ctx);
        app.folder.drop_listing();
        app.receive_remote(&ctx);
        let got: Vec<Vec<u8>> = answers.iter().map(|a| a.try_recv().unwrap()).collect();
        assert!(got[0].is_empty(), "a.png is there");
        assert!(String::from_utf8_lossy(&got[1]).contains("none.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A command after a waiting select waits its turn, so another script's `open` does not
    /// run first and leave the select answered for the other folder. When more than 64 are
    /// waiting, the client is told to try again.
    #[test]
    fn commands_behind_a_waiting_select_wait_their_turn() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        image::RgbImage::new(4, 4)
            .save(other.join("b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        let (_reading, listing) = std::sync::mpsc::channel();
        app.folder.hold_listing(listing);
        let (reply, answer) = std::sync::mpsc::channel();
        let select = format!("select {}\0", dir.join("a.png").display());
        tx.send((select.clone(), Some(reply))).unwrap();
        tx.send((format!("open {}", other.display()), None))
            .unwrap();
        // At most 64 wait, so one more is dropped.
        let mut more = Vec::new();
        for _ in 0..63 {
            let (reply, answer) = std::sync::mpsc::channel();
            tx.send((select.clone(), Some(reply))).unwrap();
            more.push(answer);
        }
        // A query is answered at once, also behind a waiting select.
        let (reply, current) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        app.receive_remote(&ctx);
        assert!(current.try_recv().is_ok(), "current answered at once");
        assert_eq!(app.folder.dir(), dir, "open waits behind the select");
        assert!(matches!(
            more[62].try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        app.folder.drop_listing();
        app.receive_remote(&ctx);
        assert!(
            answer.try_recv().unwrap().is_empty(),
            "a.png found in its own folder"
        );
        assert_eq!(app.folder.dir(), other);
        // Closing with selects still waiting, and one sent just before, tells each that none of
        // its paths was selected.
        let (reply, closing) = std::sync::mpsc::channel();
        app.remote
            .waiting_selects
            .push((select.clone(), Some(reply)));
        let (reply, late) = std::sync::mpsc::channel();
        tx.send((select.clone(), Some(reply))).unwrap();
        app.answer_waiting_selects();
        assert!(String::from_utf8_lossy(&closing.try_recv().unwrap()).contains("a.png"));
        assert!(String::from_utf8_lossy(&late.try_recv().unwrap()).contains("a.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The man page describes each start option and remote command in an entry of its own (the
    /// line after .TP) before its KEYS, whose EXAMPLES have entries too. So a new one cannot
    /// ship without its page. `where` is left out, as only the release tests' build has it.
    #[test]
    fn man_page_names_every_option_and_command() {
        let page = include_str!("../../docs/glimt.1").replace("\\-", "-");
        let lines: Vec<&str> = page.split(".SH KEYS").next().unwrap().lines().collect();
        let entries: std::collections::HashSet<&str> = (lines.windows(2))
            .filter(|w| w[0] == ".TP")
            .flat_map(|w| w[1].split([' ', '"', ',']))
            .collect();
        let options = (crate::cli::START_OPTIONS.iter())
            .flat_map(|(o, _)| o.split([' ', ',']))
            .filter(|w| w.starts_with('-'));
        let commands = (super::COMMANDS.iter())
            .filter_map(|(c, _)| c.split(' ').next())
            .filter(|&c| c != "where");
        let missing: Vec<&str> = options
            .chain(commands)
            .filter(|w| !entries.contains(w))
            .collect();
        assert!(missing.is_empty(), "docs/glimt.1 lacks {missing:?}");
    }
}
