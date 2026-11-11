//! Remote control, as with `geeqie --remote`. `glimt --remote next` sends a command to the
//! Glimt window used last, and `glimt --remote-all quit` sends one to every window. Each Glimt
//! listens on its own Unix socket in `$XDG_RUNTIME_DIR/glimt`, which only the user can reach,
//! and the one that gets the focus writes its process ID to `focused` there. A Glimt started
//! with `--name left` listens as `left` instead, and `glimt --name left --remote next` reaches
//! only it. In a Flatpak the folder is in the app's own runtime folder, the part of
//! `$XDG_RUNTIME_DIR` its instances share. Both ends are here: `client` sends, and `listen`
//! receives.

use crate::features::remote;
use crate::support::lang::tr;
#[cfg(feature = "test-hooks")]
use crate::support::test_hooks;
use crate::support::threads::lock_even_if_poisoned;
use crate::support::threads::{AnswerSender, Receiving, Took, Working, answer_channel};
use crate::tf;
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
        "Show a file, folder or collection, or mount a network location such as \
         smb://nas/photos (starts Glimt if it is not running)",
    ),
    (
        "title [TEXT…]",
        "This window's title, a template as in Preferences (%name%, %path%, %folder%, %app%, %id%); without the text, the one in Preferences",
    ),
    (
        "name [NAME]",
        "Give this window a new --name, which --remote reaches it by from now on; without NAME, none, so its process ID does, or nothing when remote control is off in Preferences",
    ),
    (
        "identify",
        "Show this window's ID (%id%: its --name or process ID, or both, as left (4242), when it shares its name) large over the image for a few seconds",
    ),
    ("quit", "Close Glimt"),
    (
        "run ACTION",
        "Do an action by its name, one of those glimt --action-list lists, which change no file, sidecar, mark or collection",
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
    (
        "info [FILE…]",
        "Print what Glimt knows about the image shown, or about these files: a key, a tab and a value to a line, with a backslash, tab, line break and a keyword's ; written as \\\\, \\t, \\n or \\r and \\;, and a blank line after each file",
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
/// arguments, a `run` of an action that is not for scripts, more than `MAX_SELECT` files, or a
/// path that cannot go in the text sent (not UTF-8, or with a line break in it).
pub const NOT_SENT: i32 = 2;

/// The exit code of `glimt --remote` and `--remote-all` when no Glimt takes the command, or
/// none of those that took it answers in time. `exit_code` holds the rule that chooses between
/// this and `NOT_DONE`.
pub const NOT_TAKEN: i32 = 1;

/// The exit code of `glimt --remote` when a window took the command but did not do it: a `run`
/// refused while a dialog is open or a text field has the focus, or a command that window does
/// not know (an older Glimt than the client). With `--remote-all` it is enough that one window
/// did not do it, or did not answer in time while another one answered.
pub const NOT_DONE: i32 = 3;

/// The start of the window's answer to a command it did not do, before the reason. Every other
/// answer of a command is empty, and that of a query is paths, which start with "/".
const NOT_DONE_ANSWER: &str = "not done: ";

/// The word before a command that `--wait` sends. The window then answers only when the work
/// that the command started has ended (`answer_awaited_outcomes`). A window older than `--wait`
/// does not know the line, and does nothing: one of 0.76 or newer answers that it does not
/// know the command, and one of 0.75 or older answers nothing.
const WAIT_WORD: &str = "wait ";

/// The answer to a command sent with `--wait` that was done, with no write failing while its
/// work ran. It is not empty, because an empty answer is that of a window of 0.75 or older,
/// which did nothing.
const OUTCOME_DONE: &str = "done\n";

/// How long a command sent with `--wait` may take, with the work it started, before the client
/// is told "no answer". A folder of 500,000 files on a slow share is read within it.
const OUTCOME_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Why a name cannot be a window's, as `--remote name` prints it.
const NOT_A_NAME: &str = "a name holds up to 64 letters, digits, - and _, not only digits";

/// The keys of `--remote info`'s answer before the EXIF rows, in their order. Scripts read
/// them, so they stay as they are, and a new one goes at the end, before the EXIF rows.
const INFO_KEYS: [&str; 13] = [
    "path",
    "size",
    "modified",
    "width",
    "height",
    "format",
    "orientation",
    "stars",
    "marks",
    "keywords",
    "comment",
    "alt_text",
    "extended_description",
];

/// How long `--remote identify` shows the window's ID.
pub(crate) const IDENTIFY_TIME: std::time::Duration = std::time::Duration::from_secs(3);

/// The file in the runtime folder that holds the name of the Glimt that had the focus last.
pub const FOCUSED_FILE: &str = "focused";

/// The most files `select` and `info` take, because the window finds a select's files in its
/// own thread. Glimt's client checks it, and the window too (`read_command`), for any other
/// program that sends on the socket.
pub const MAX_SELECT: usize = 100_000;

/// What a client is told when its `select` or `info` names more files than `MAX_SELECT`.
const TOO_MANY_FILES: &str = "more than 100000 files";

/// The longest line of a command other than `select` and `info`, in bytes: room for a path or
/// a title. Only those two may be as long as their files take.
const MOST_LINE: u64 = 64 * 1024;

/// The most clients served at once, each by a thread of its own. Another waits in the socket's
/// queue until one is done, or until its time limit tells it "no answer".
const MOST_CLIENTS: usize = 16;

/// How long a client waits for each part of an answer, and the window for a client's command.
/// A client that sends nothing, or a window that does not answer, holds up the others no longer
/// than this. `info` sends each file's record as soon as it is read, so its whole answer may take
/// as long as its files take, while no one file takes longer than this.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a look for a running Glimt on a socket waits for the connection (`answers`). A Glimt
/// that runs takes it at once, unless it hangs.
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// The name from `--name`, set before the window opens, or later by `--remote name` and Window
/// → Set Window Name….
static GIVEN_NAME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The ID of the socket this Glimt listens on. It is `socket_id()`, or `NAME.ID` for a window
/// that shares its --name with one that listens as `NAME` already.
static LISTENING_AS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// A window took a command but gave no answer in time.
static TIMED_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A window took a command and answered it in time, whether it did it or not.
static ANSWERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A window answered that it did not do the command.
static WAS_NOT_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This Glimt listens on its socket, so it removes it when it closes.
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The listening socket's file number, -1 when this Glimt does not listen.
static LISTENER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// This Glimt's name, found once (`find_instance_id`), because every new thumbnail asks.
static INSTANCE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Held while the window's name is changed or its socket made, so that one change waits for
/// the one before it, each in a thread of its own.
static NAMING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Clients connected, and queries sent to the window whose answers are not written yet.
static UNFINISHED_ANSWERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Where the window sends a query's answer. It is paths, each ending in a NUL byte, as
/// `find -print0` writes them, because a file name can hold a line break.
pub type Reply = mpsc::Sender<Vec<u8>>;

/// Who asked for a new window name, whom its answer goes back to.
pub(crate) enum NameAskedBy {
    /// Window → Set Window Name…, which tells a refusal in a notice.
    Dialog,
    /// `--remote name`, answered where there is a client to answer.
    Remote(Option<Reply>),
    /// The window's start, which listens as its --name, or as its name and process ID when
    /// another window has that name already. The title shows which once it is answered.
    Start,
}

/// A window name taken or refused in the background (`App::set_window_name`).
pub(crate) struct NameAnswer {
    asked_by: NameAskedBy,
    /// `Err(None)` for a name that a window cannot take (`can_be_window_name`), and
    /// `Err(Some(why))` for one refused for another reason.
    taken: Result<(), Option<String>>,
}

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

/// A command sent with `--wait`, whose answer waits until the work it started has ended: the
/// folder it opens is read, the answers of its threads are taken, and its writes are done.
struct AwaitedOutcome {
    /// The answer of the command itself, as without `--wait`.
    command_answer: mpsc::Receiver<Vec<u8>>,
    /// Whether the command answered that it was done, so that only its work is waited for.
    was_done: bool,
    /// Whether the command closes the window. Its outcome is told as the window closes, after
    /// the last writes, or when a write fails and keeps the window open.
    closes_the_window: bool,
    /// Where the outcome goes, to the client.
    reply: Reply,
    /// How many failed writes the window had told when the command came. A write that fails
    /// after that, while the work runs, makes the command not done.
    failed_writes_before: u64,
    /// When the client has stopped waiting, and the outcome is dropped.
    until: std::time::Instant,
}

/// Commands from --remote, the selects that wait until the list is read, and how long the
/// window's ID shows.
pub(crate) struct Listener {
    /// Commands from `glimt --remote`.
    rx: mpsc::Receiver<(String, Option<remote::Reply>)>,
    /// The sender of `rx`, for a window that starts to listen when it is given a name.
    tx: mpsc::Sender<(String, Option<remote::Reply>)>,
    /// The commands that came while the place to open was looked up, a `--remote select` that
    /// came while the folder was read or while Delete or Rename asked, and the commands after
    /// it that may open another folder or come after one that may. They are done in the order
    /// they came (`must_wait`).
    waiting_commands: Vec<(String, Option<remote::Reply>)>,
    /// The time until which `--remote identify` shows the window's ID over the image.
    identify_until: Option<std::time::Instant>,
    /// The window listens for `--remote` also without a name (`listens`).
    listens_without_name: bool,
    /// The names given to the window, taken or refused in the background, because the socket
    /// is renamed for them.
    name_sender: AnswerSender<NameAnswer>,
    name_answers: Receiving<(NameAnswer, Working)>,
    /// The commands sent with `--wait` that were taken, until their outcome is answered.
    awaiting_outcome: Vec<AwaitedOutcome>,
}

impl Listener {
    /// Listens for `--remote` when the window does without a name (`listens_without_name`), or
    /// when it was started with --name. The socket is made in the background, as a name is.
    pub(crate) fn new(
        ctx: &egui::Context,
        busy: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        listens_without_name: bool,
    ) -> Self {
        let start_listening =
            listens(listens_without_name, given_name().is_some()).then_some(listen);
        Self::start(ctx, busy, listens_without_name, start_listening)
    }

    /// Makes the listener, and runs `start_listening` in the background when it is given. Its
    /// answer comes back as a name's does (`receive_window_name`), because the title shows the
    /// ID that the window listens as, which is known only once the socket is made.
    fn start(
        ctx: &egui::Context,
        busy: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        listens_without_name: bool,
        start_listening: Option<
            impl FnOnce(mpsc::Sender<(String, Option<Reply>)>, egui::Context) -> Result<(), String>
            + Send
            + 'static,
        >,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (name_sender, name_answers) = answer_channel(busy);
        if let Some(start_listening) = start_listening {
            let (tx, ctx, answers) = (tx.clone(), ctx.clone(), name_sender.clone());
            std::thread::spawn(move || {
                let taken = {
                    let _one_at_a_time = lock_even_if_poisoned(&NAMING);
                    start_listening(tx, ctx.clone())
                };
                if let Err(why) = &taken {
                    eprintln!("{why}");
                }
                let taken = taken.map_err(Some);
                let _ = answers.send(NameAnswer {
                    asked_by: NameAskedBy::Start,
                    taken,
                });
                ctx.request_repaint();
            });
        }
        Listener {
            rx,
            tx,
            waiting_commands: Vec::new(),
            identify_until: None,
            listens_without_name,
            name_sender,
            name_answers: Receiving::of(name_answers),
            awaiting_outcome: Vec::new(),
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

    /// Receives `glimt --remote` commands and queries. While the place that an `open` named is
    /// looked up or mounted, every command but `quit` waits, and a `select` waits too while the
    /// folder is read or a dialog asks about the selection.
    pub(crate) fn receive_remote(&mut self, ctx: &egui::Context) {
        // `open` only starts a look-up, and the window shows the old list until its answer is
        // taken. It is taken here first, because a hidden window runs no `ui`, which takes it
        // too.
        self.receive_looked_up(ctx, std::time::Duration::ZERO);
        // An `open` of a network location mounts it first, and its answer is taken here for
        // the same reason.
        self.receive_mount(ctx);
        // A hidden window runs no `ui`, which takes the listing, so a waiting select takes it
        // here. Otherwise the select would wait until the window is shown again.
        if !self.remote.waiting_commands.is_empty() {
            self.receive_listing(ctx, std::time::Duration::ZERO);
        }
        // The waiting ones are done in the order they came, until one (an `open`) makes them
        // wait again. Each gets its own answer, from the list as it was when it was sent.
        while let Some((command, reply)) = self.remote.waiting_commands.first()
            && !self.must_wait(command, reply.is_some())
        {
            let (command, reply) = self.remote.waiting_commands.remove(0);
            self.handle_remote(ctx, &command, reply);
        }
        while let Ok((command, reply)) = self.remote.rx.try_recv() {
            let (command, reply) = self.await_outcome(ctx, command, reply);
            // An `open` replaces a look-up that has not answered, as a click in the folder tree
            // does, so that a script can leave a share that stopped answering. The commands that
            // waited for the old place were sent for a place that will not open, so they are
            // not done.
            if is_open(&command) && self.folder.is_opening() {
                self.cancel_waiting_commands("the place is no longer being opened");
            }
            // A command behind a waiting one that may open another folder waits its turn,
            // because it would otherwise act on the list from before that folder.
            let behind_an_open =
                (self.remote.waiting_commands.iter()).any(|(waiting, _)| may_relist(waiting));
            // A quit is done at once, ahead of the queue, because it does not read the list, and
            // a look-up on a share that stopped answering would otherwise hold it for ever.
            let wait = command.trim() != "quit"
                && (self.must_wait(&command, reply.is_some())
                    || (!self.remote.waiting_commands.is_empty()
                        && (may_relist(&command) || behind_an_open)));
            if !wait {
                self.handle_remote(ctx, &command, reply);
            } else if self.remote.waiting_commands.len() < 64 {
                self.remote.waiting_commands.push((command, reply));
            }
            // When more than that are waiting, the answer is dropped, and the client is told
            // "no answer" (serve), so the script tries again.
        }
        self.answer_awaited_outcomes(ctx, false);
    }

    /// Takes `command` as it is done: a command sent with `--wait` loses its word, and its
    /// answer goes to `awaiting_outcome`, which answers the client once the command's work has
    /// ended. A query, and a command sent without `--wait`, are returned as they came.
    fn await_outcome(
        &mut self,
        ctx: &egui::Context,
        command: String,
        reply: Option<Reply>,
    ) -> (String, Option<Reply>) {
        let (Some(waited), Some(reply)) = (command.strip_prefix(WAIT_WORD), reply.clone()) else {
            return (command, reply);
        };
        if is_query(waited) {
            return (waited.to_owned(), Some(reply));
        }
        // A failed write told before the command came is taken first, so that it does not
        // count as the command's.
        self.take_saves(ctx);
        let (command_reply, command_answer) = mpsc::channel();
        self.remote.awaiting_outcome.push(AwaitedOutcome {
            command_answer,
            was_done: false,
            closes_the_window: closes_the_window(waited),
            reply,
            failed_writes_before: self.window.failed_writes_told(),
            until: std::time::Instant::now() + OUTCOME_WAIT,
        });
        (waited.to_owned(), Some(command_reply))
    }

    /// Answers each command sent with `--wait` whose work has ended: with the reason when the
    /// command was not done or a write failed while its work ran, and with `OUTCOME_DONE`
    /// otherwise. The work has ended when no place is opened, no folder read, no thread works
    /// for the window and no file is written, and the window has taken the failed writes. A
    /// command that closes the window waits for `closing`, which `on_exit` gives after its last
    /// writes.
    pub(crate) fn answer_awaited_outcomes(&mut self, ctx: &egui::Context, closing: bool) {
        if self.remote.awaiting_outcome.is_empty() {
            return;
        }
        // Whether files are written is read before the failures are taken, as `guard_close`
        // does: a write tells its failure before it stops counting as writing.
        let is_writing = self.is_writing();
        let at_rest = !is_writing && !self.folder.is_opening() && !self.working_besides_image();
        self.take_saves(ctx);
        let failed_writes = self.window.failed_writes_told();
        let last_failure = self.window.last_failed_write().to_owned();
        let now = std::time::Instant::now();
        self.remote.awaiting_outcome.retain_mut(|awaited| {
            if !awaited.was_done {
                match awaited.command_answer.try_recv() {
                    Ok(answer) if answer.is_empty() => awaited.was_done = true,
                    // Not done, as without `--wait`.
                    Ok(answer) => {
                        let _ = awaited.reply.send(answer);
                        return false;
                    }
                    Err(mpsc::TryRecvError::Empty) if closing => {
                        let _ = awaited.reply.send(not_done_answer("this Glimt is closing"));
                        return false;
                    }
                    Err(mpsc::TryRecvError::Empty) => return now < awaited.until,
                    // Dropped, so the client is told "no answer" (serve).
                    Err(mpsc::TryRecvError::Disconnected) => return false,
                }
            }
            let failed = failed_writes != awaited.failed_writes_before;
            let ended = match closing {
                true => true,
                false => at_rest && (!awaited.closes_the_window || failed),
            };
            if !ended {
                return now < awaited.until;
            }
            let outcome = match (failed, closing && is_writing) {
                (true, _) => not_done_answer(&last_failure),
                (false, true) => not_done_answer("this Glimt closed before its writes were done"),
                (false, false) => OUTCOME_DONE.as_bytes().to_vec(),
            };
            let _ = awaited.reply.send(outcome);
            false
        });
        // A write ends in its own thread, which asks for no frame, so the window looks again.
        if !self.remote.awaiting_outcome.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Whether `command` must wait before it is done. Every command waits while the place to
    /// open is looked up, because it would act on the list from before that place. A select
    /// (with a reply, `is_query`) also waits while the folder is read, because its files are
    /// not all listed yet. It waits as well while a dialog asks about the selection (Delete,
    /// Rename), which is answered for the files it asked about, not for a selection that a
    /// script put in their place. An `open` does not wait for the look-up or the mount: it
    /// replaces them (`receive_remote`).
    fn must_wait(&self, command: &str, is_query: bool) -> bool {
        let select = is_query && command.starts_with("select ");
        (self.folder.is_opening() && !is_open(command))
            || (select && (self.folder.is_listing() || self.asking_about_selection()))
    }

    /// Answers each select still waiting at closing, with all its paths and none of them
    /// selected. Left without an answer, the closed socket would read as "all found". It also
    /// answers those sent just before and not yet taken from the channel, and a query is
    /// answered as usual (current, selection).
    pub(crate) fn answer_waiting_selects(&mut self) {
        let sent: Vec<_> = self.remote.rx.try_iter().collect();
        let waiting = std::mem::take(&mut self.remote.waiting_commands);
        let ctx = self.ctx.clone();
        for (command, reply) in waiting.into_iter().chain(sent) {
            let Some(reply) = reply else { continue };
            // A query other than a select is answered as at any other time (`where` too, in the
            // release tests' build). Any other command is not done as the window closes.
            match is_query(&command) && !command.starts_with("select ") {
                true => self.handle_remote(&ctx, &command, Some(reply)),
                false => {
                    let _ = reply.send(not_done_reply(&command, "this Glimt is closing"));
                }
            }
        }
    }

    /// Answers each waiting command that it was not done, for the reason `why`, and forgets
    /// them all.
    fn cancel_waiting_commands(&mut self, why: &str) {
        for (command, reply) in std::mem::take(&mut self.remote.waiting_commands) {
            if let Some(reply) = reply {
                let _ = reply.send(not_done_reply(&command, why));
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
        if let Some(name) = name_asked(command) {
            return self.set_window_name(ctx, name, NameAskedBy::Remote(reply));
        }
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
            Some(reply) if is_info(command) => self.remote_info(command, reply),
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
                // in a space. A network location is mounted, as on the command line.
                if let Some(path) = command.trim_start().strip_prefix("open ") {
                    match crate::files::external::network_location(path) {
                        Some(uri) => self.mount(ctx, uri.to_owned()),
                        None => self.visit(ctx, PathBuf::from(path)),
                    }
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

    /// Gives this window the --name `name`, or none when it is empty, in the background,
    /// because its socket is renamed for it. The title shows it once it is taken
    /// (`receive_window_name`), and `asked_by` gets the answer then.
    pub(crate) fn set_window_name(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        asked_by: NameAskedBy,
    ) {
        let name = Some(name.to_owned()).filter(|n| !n.is_empty());
        let (commands, ctx) = (self.remote.tx.clone(), ctx.clone());
        let listens_without_name = self.remote.listens_without_name;
        let (tx, pointer) = (self.remote.name_sender.clone(), Working::new(&self.busy));
        std::thread::spawn(move || {
            let taken = {
                let _one_at_a_time = lock_even_if_poisoned(&NAMING);
                name_this_window(name.as_deref(), listens_without_name, commands, &ctx)
            };
            let _ = tx.send(NameAnswer { asked_by, taken });
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Takes the names taken or refused in the background: the title shows a name taken, and
    /// whoever asked is answered.
    pub(crate) fn receive_window_name(&mut self, ctx: &egui::Context) {
        while let Took::Answer((answer, _pointer)) = self.remote.name_answers.next() {
            if answer.taken.is_ok() {
                self.update_window_title(ctx);
            }
            match (answer.asked_by, answer.taken) {
                (NameAskedBy::Remote(Some(reply)), Ok(())) => drop(reply.send(Vec::new())),
                (NameAskedBy::Remote(Some(reply)), Err(why)) => {
                    let why = why.unwrap_or_else(|| NOT_A_NAME.to_owned());
                    drop(reply.send(not_done_answer(&why)));
                }
                // A start that could not listen printed why in its thread.
                (NameAskedBy::Remote(None) | NameAskedBy::Start, _)
                | (NameAskedBy::Dialog, Ok(())) => {}
                // The name typed is told in the user's language. The other reasons come from
                // the system, rarely, and are the English ones that `--remote name` prints too.
                (NameAskedBy::Dialog, Err(None)) => self.notice.give(
                    tr(
                        "Could not name this window: a name holds up to 64 letters, digits, \
                        - and _, and not only digits",
                    )
                    .to_owned(),
                ),
                (NameAskedBy::Dialog, Err(Some(why))) => self
                    .notice
                    .give(tf!("Could not name this window: {why}", why)),
            }
        }
    }

    /// The answer to `glimt --remote current` or `selection`, the paths, each ending in a NUL
    /// byte. Nothing else is told, and nothing changes.
    // shortcut: the paths of a selection made in the window's thread, some 10 ms for 500,000
    // selected; in a thread of its own if scripts ask for large selections often.
    pub(crate) fn remote_query(&self, command: &str) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        let files = match command.trim() {
            "current" => vec![self.list.shown()],
            // When the search hides every selected file, the answer names none of them, and
            // not the shown image in their place.
            _ => self.targets_without_notice().unwrap_or_default(),
        };
        // One path is made again for each file, as `path` makes it, without a new one each time.
        let (mut answer, mut path) = (Vec::new(), PathBuf::new());
        for i in files.into_iter().filter(|&i| i < self.list.files().len()) {
            path.as_mut_os_string().clear();
            path.push(self.folder.dir());
            path.push(&self.list.files()[i]);
            answer.extend_from_slice(path.as_os_str().as_bytes());
            answer.push(0);
        }
        answer
    }

    /// Answers `glimt --remote info [FILE…]` (the files after a NUL each) with an `info_record`
    /// of each file, or of the image shown when no file is named. The files are read in a
    /// thread of their own, which sends the answer. Any readable file may be named, listed or
    /// not, as nothing is written. The marks and stars are this window's, as its list shows
    /// them. Without a file named and no image shown, the answer is "not done".
    pub(crate) fn remote_info(&self, command: &str, reply: Reply) {
        let paths: Vec<PathBuf> = match command.strip_prefix("info ") {
            Some(paths) => (paths.split('\0').filter(|p| !p.is_empty()))
                .map(PathBuf::from)
                .collect(),
            None if self.list.shown() < self.list.files().len() => {
                vec![self.path(self.list.shown())]
            }
            None => return drop(reply.send(not_done_answer("no image is shown"))),
        };
        if paths.is_empty() {
            return drop(reply.send(not_done_answer("no file is named")));
        }
        #[cfg(feature = "marks")]
        let marks: Vec<_> = paths.iter().map(|p| Some(self.marks.get(p))).collect();
        #[cfg(not(feature = "marks"))]
        let marks = vec![None; paths.len()];
        // Each record is sent as soon as it is read, so the client's time limit (`WAIT`) holds
        // for each file and not for all of them. The thread stops when the client is gone.
        std::thread::spawn(move || {
            for (path, marks) in paths.iter().zip(marks) {
                // A blank line ends each record, so the answers of several windows
                // (--remote-all) stay apart too.
                let mut record = info_record(path, marks);
                record.push(b'\n');
                if reply.send(record).is_err() {
                    return;
                }
            }
        });
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

/// Returns the answer to `command` when it was not done, for the reason `why`. A select is answered
/// with all its paths, none of them selected, because an empty answer would read as "all
/// found".
fn not_done_reply(command: &str, why: &str) -> Vec<u8> {
    match command.strip_prefix("select ") {
        Some(paths) => {
            let paths = paths.split('\0').filter(|p| !p.is_empty());
            paths.flat_map(|p| p.bytes().chain([0])).collect()
        }
        None => not_done_answer(why),
    }
}

/// Returns the reason in a window's answer `answer`, or `None` when the answer says that the
/// command was done, or is a query's.
fn not_done_reason(answer: &[u8]) -> Option<String> {
    let why = answer.strip_prefix(NOT_DONE_ANSWER.as_bytes())?;
    Some(String::from_utf8_lossy(why).trim_end().to_owned())
}

/// Returns why a command sent with `--wait` was not done, from a window's `answer` that is no
/// `NOT_DONE_ANSWER`. Returns `None` for `OUTCOME_DONE`. Any other answer, empty above all, is
/// that of a window older than `--wait`, which did nothing.
fn waited_outcome_reason(answer: &[u8]) -> Option<String> {
    (answer != OUTCOME_DONE.as_bytes()).then(|| "this Glimt does not know --wait".to_owned())
}

/// Whether `command` is a query, which answers with what it asks for. None of them changes a
/// file. `select` only selects, and what is done with the selection is the user's to do in the
/// window.
fn is_query(command: &str) -> bool {
    matches!(command.trim(), "current" | "selection")
        || is_info(command)
        || command.starts_with("select ")
        || command.starts_with("where ")
}

/// Whether `command` closes the window: `quit`, or `run` of Quit or CloseWindow.
fn closes_the_window(command: &str) -> bool {
    let run = command.trim().strip_prefix("run ").map(str::trim);
    let action = run.and_then(Action::for_script);
    command.trim() == "quit" || matches!(action, Some(Action::Quit | Action::CloseWindow))
}

/// Whether `command` is `info`, alone or with files.
fn is_info(command: &str) -> bool {
    command.trim_end() == "info" || command.starts_with("info ")
}

/// The answer of `--remote info` for the file at `path`, with its (mark bits, stars) in
/// `marks`, or `None` when marks are not built in. Its form is frozen, as scripts read it:
/// one `key<TAB>value` line for each of `INFO_KEYS` and then each EXIF row of
/// `exif::LABELS`, always all of them in this order, and an empty value where the file has
/// none. Each value is `escaped`, so it stays on its line and a script can undo it. The path
/// is the file's bytes, also when they are not UTF-8, as `current` gives them. A path that is
/// not a regular file has every value but its path empty.
fn info_record(path: &Path, marks: Option<(u8, u8)>) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let file = std::fs::metadata(path).ok().filter(|m| m.is_file());
    let (values, exif_rows) = match file {
        Some(file) => info_values(path, &file, marks),
        None => Default::default(),
    };
    let exif_value = |label: &str| exif_rows.iter().find(|r| r.0 == label).map(|r| r.1.clone());
    let exif_lines = (crate::metadata::exif::LABELS.iter()).map(|label| {
        let value = exif_value(label).unwrap_or_default();
        (info_exif_key(label), escaped(value.as_bytes(), b""))
    });
    let path = escaped(path.as_os_str().as_bytes(), b"");
    let lines = (INFO_KEYS.iter().map(|k| k.to_string()))
        .zip([path].into_iter().chain(values))
        .chain(exif_lines);
    let mut record = Vec::new();
    for (key, value) in lines {
        record.extend_from_slice(key.as_bytes());
        record.push(b'\t');
        record.extend_from_slice(&value);
        record.push(b'\n');
    }
    record
}

/// The values of `INFO_KEYS` after the path, for the regular file at `path` with the metadata
/// `file` and the (mark bits, stars) `marks`, and the EXIF rows the info panel shows. The
/// width and height are as stored, before the orientation turns the image. The format is the
/// one the file's first bytes tell, or else its extension in lower case. Each value is
/// `escaped`. Keywords are joined by "; ", and a ";" inside a keyword is written as "\;", so a
/// script splits them at a "; " that has no backslash before it.
fn info_values(
    path: &Path,
    file: &std::fs::Metadata,
    marks: Option<(u8, u8)>,
) -> ([Vec<u8>; 12], Vec<(&'static str, String)>) {
    use crate::metadata::{exif, xmp};
    use crate::support::threads::catch_panic;
    let since_1970 = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).ok();
    let modified = file.modified().ok().and_then(since_1970);
    let modified = modified.map(|d| crate::support::format::utc_time(d.as_secs() as i64));
    let size = catch_panic(|| crate::images::decode::dimensions(path)).flatten();
    let format = (crate::images::decode::image_reader(path).ok())
        .and_then(|reader| reader.format())
        .and_then(|f| f.extensions_str().first().map(|e| e.to_string()))
        .or_else(|| path.extension().map(|e| e.to_string_lossy().to_lowercase()));
    let (exif_rows, orientation) =
        catch_panic(|| exif::read_with_orientation(path)).unwrap_or_default();
    let (meta, _, alt_text) = catch_panic(|| xmp::read_all(path)).unwrap_or_default();
    let mark_numbers = marks.map(|(bits, _)| (1..=6).filter(move |n| bits & (1 << (n - 1)) != 0));
    let mark_numbers: Vec<String> = (mark_numbers.into_iter().flatten())
        .map(|n| n.to_string())
        .collect();
    let keywords: Vec<Vec<u8>> = (meta.keywords.iter())
        .map(|k| escaped(k.as_bytes(), b";"))
        .collect();
    let value = |text: String| escaped(text.as_bytes(), b"");
    let text = |value: Option<String>| value.unwrap_or_default();
    let values = [
        value(file.len().to_string()),
        value(text(modified)),
        value(text(size.map(|s| s.0.to_string()))),
        value(text(size.map(|s| s.1.to_string()))),
        value(text(format)),
        value(text(orientation.map(|o| o.to_string()))),
        value(text(marks.map(|m| m.1.to_string()))),
        value(mark_numbers.join(",")),
        keywords.join(&b"; "[..]),
        value(meta.comment),
        value(alt_text.text),
        value(alt_text.extended_description),
    ];
    (values, exif_rows)
}

/// The key of `--remote info`'s line for the EXIF row `label`: "exif." and the label in lower
/// case, with "_" for a space, such as "exif.focal_length".
fn info_exif_key(label: &str) -> String {
    format!("exif.{}", label.to_lowercase().replace(' ', "_"))
}

/// Returns `value` as `--remote info` writes it, so it stays one value on one line and a
/// script can undo it: a backslash as `\\`, a tab as `\t`, a line feed as `\n`, a carriage
/// return as `\r`, and each byte of `also` with a backslash before it. Every other byte stays
/// as it is.
fn escaped(value: &[u8], also: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    for &byte in value {
        match byte {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            _ if also.contains(&byte) => out.extend_from_slice(&[b'\\', byte]),
            _ => out.push(byte),
        }
    }
    out
}

/// Returns how many records of `--remote info` the window had still to send when its answer
/// `answer` to `command` ended, as when the window closed meanwhile. A blank line ends each
/// record, and no value holds a line break. It is 0 for any other command.
fn info_records_missing(command: &str, answer: &[u8]) -> usize {
    if !is_info(command) {
        return 0;
    }
    let asked = match command.strip_prefix("info ") {
        Some(paths) => paths.split('\0').filter(|p| !p.is_empty()).count(),
        None => 1,
    };
    let records = answer.windows(2).filter(|pair| pair == b"\n\n").count();
    asked.saturating_sub(records)
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

/// Finds this Glimt's name (`instance_id`) once. In a Flatpak that reads /.flatpak-info, so
/// `main` calls it first, before any thread starts and before the window opens, and the window
/// never reads it.
pub fn find_instance_id() {
    INSTANCE_ID.get_or_init(|| {
        let info = (crate::files::external::in_flatpak())
            .then(|| crate::images::decode::read_text("/.flatpak-info").ok())
            .flatten();
        let instance = info
            .as_deref()
            .and_then(|i| i.lines().find_map(|l| l.strip_prefix("instance-id=")));
        let pid = std::process::id();
        instance.map_or_else(|| pid.to_string(), |i| format!("{i}-{pid}"))
    });
}

/// This Glimt's name. It is its process ID, and in a Flatpak, where every instance can have
/// the same process ID, the Flatpak instance ID too (a new window from Ctrl+N shares the
/// instance), as `find_instance_id` found it at start. Without that, as in the tests, it is the
/// process ID.
pub fn instance_id() -> &'static str {
    INSTANCE_ID.get_or_init(|| std::process::id().to_string())
}

/// Listen as `name` instead of the process ID.
pub fn set_name(name: String) {
    *lock_even_if_poisoned(&GIVEN_NAME) = Some(name);
}

/// The `--name` given, if any.
pub fn given_name() -> Option<String> {
    lock_even_if_poisoned(&GIVEN_NAME).clone()
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

/// Whether a window may take `name` as its own (`rename`): a `valid_name` whose socket fits.
pub fn can_be_window_name(name: &str) -> bool {
    !name.is_empty() && valid_name(name) && name_fits(name)
}

/// Whether the socket of `--name name` fits in the bytes a socket's path may have here. In a
/// Flatpak its folder alone takes some 60.
pub fn name_fits(name: &str) -> bool {
    socket(name).is_none_or(|p| socket_path_fits(&p))
}

/// Whether a client can connect to a socket at `path`. A longer path can be made, by a link,
/// but no client reaches the socket by it, because a socket's address holds at most 107 bytes.
fn socket_path_fits(path: &Path) -> bool {
    path.as_os_str().len() <= 107
}

/// Returns whether `command` may read the folder again or open another, so that behind a waiting
/// select it waits its turn, and the select is answered for the list it was sent to: another
/// script's open, subfolders, a slideshow with its subfolders, and run, whose ParentFolder,
/// HomeFolder, Back or Refresh read a folder. Other queries and commands are done at once, as
/// without it.
fn may_relist(command: &str) -> bool {
    ["open ", "subfolders", "slideshow", "run "]
        .iter()
        .any(|c| command.trim_start().starts_with(c))
}

/// Whether `command` is `open`, which replaces a look-up that has not answered.
fn is_open(command: &str) -> bool {
    command.trim_start().starts_with("open ")
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
            // One word in brackets may be left out.
            Some(one) if one.starts_with('[') => words.len() <= 2,
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

/// Chooses the exit code of `--remote` and `--remote-all` from what the windows that took the
/// command did: whether one `answered` in time, whether one `timed_out`, and whether one
/// answered that it did `not_done` it. A window that did not answer while another one did has
/// not done the command, as far as the client knows, so that is `NOT_DONE`, and a script that
/// retries on `NOT_TAKEN` does not do it twice in the windows that did. Returns `None` when no
/// window said no and none was silent, so the command was done or no Glimt took it.
fn exit_code(answered: bool, timed_out: bool, not_done: bool) -> Option<i32> {
    match (not_done || (answered && timed_out), timed_out) {
        (true, _) => Some(NOT_DONE),
        (false, true) => Some(NOT_TAKEN),
        (false, false) => None,
    }
}

/// The ID that this Glimt listens as, which is its --name or else its process ID.
fn socket_id() -> String {
    given_name().unwrap_or_else(|| instance_id().to_owned())
}

/// The ID of the socket that this Glimt listens on, which `--remote` reaches this window alone
/// by. It is `socket_id()` until the socket is bound.
fn own_socket_id() -> String {
    lock_even_if_poisoned(&LISTENING_AS)
        .clone()
        .unwrap_or_else(socket_id)
}

/// The IDs that `--name` reaches this window by, as the %id% of the window title and
/// `--remote identify` show them: its name or process ID, and for a window that shares its name
/// with another, the name and its own ID, as `left (4242)`.
pub fn window_id() -> String {
    shown_id(&own_socket_id())
}

/// Returns the socket ID `id` as the user reads it: `NAME.ID` as `NAME (ID)`, and any other as
/// it is.
fn shown_id(id: &str) -> String {
    match id.split_once('.') {
        Some((name, own)) => format!("{name} ({own})"),
        None => id.to_owned(),
    }
}

/// Whether the socket at `path` is one of the windows that `--name name` reaches: the first
/// one's `glimt-NAME.sock`, or `glimt-NAME.ID.sock` of a window that shares the name, which its
/// own ID after the dot reaches too. A name holds no dot, so the part before the first dot is
/// the name.
fn is_named(path: &std::path::Path, name: &str) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str());
    let Some(id) = stem.and_then(|s| s.strip_prefix("glimt-")) else {
        return false;
    };
    match id.split_once('.') {
        Some((first, own)) => first == name || own == name,
        None => id == name,
    }
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
    let waits = command.starts_with(WAIT_WORD);
    let limit = match waits {
        // The window's listener says "no answer" a second after `OUTCOME_WAIT`.
        true => OUTCOME_WAIT + std::time::Duration::from_secs(2),
        false => WAIT,
    };
    let _ = s.set_read_timeout(Some(limit));
    let mut answer = Vec::new();
    #[allow(clippy::disallowed_methods)] // a reply on Glimt's own private socket, with a time limit
    let read = s.read_to_end(&mut answer);
    let timed_out = read.is_err();
    match read {
        Ok(_) => ANSWERED.store(true, std::sync::atomic::Ordering::Relaxed),
        Err(e) => {
            eprintln!("glimt: no answer from {}: {e}", path.display());
            TIMED_OUT.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // An `info` that ends before its last record, because the window closed, is not done.
    let missing = info_records_missing(command, &answer);
    let why = match not_done_reason(&answer) {
        Some(why) => Some(why),
        None if waits && !timed_out => waited_outcome_reason(&answer),
        None => {
            let _ = std::io::stdout().write_all(&answer);
            let closed = format!("the window closed before it told about {missing} of the files");
            (missing > 0 && !timed_out).then_some(closed)
        }
    };
    if let Some(why) = why {
        eprintln!("glimt: not done: {why}");
        WAS_NOT_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    true
}

/// The sockets in `dir`, whether or not a Glimt answers on them. A command is sent to these,
/// without a test of each socket first, which would be a connection of its own, without a
/// command, to every window.
fn socket_files(dir: Option<PathBuf>) -> Vec<PathBuf> {
    let entries = dir.and_then(|d| std::fs::read_dir(d).ok());
    entries
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .collect()
}

/// Whether a Glimt in the runtime folder `dir` answers, one that `--name name` reaches
/// (`is_named`) when `name` is given. Only those sockets are connected to, so the windows of
/// other names see no connection. A socket that does not answer is left alone, because a
/// Glimt that is starting refuses connections between binding its socket and listening on it,
/// and removing it then would leave that Glimt without remote control.
/// shortcut: the socket of a Glimt that crashed stays until the runtime folder is cleared at
/// logout, and costs one refused connection each time; remove such sockets at start, under
/// `bind_socket`'s lock, if they ever pile up.
fn is_running(dir: Option<PathBuf>, name: Option<&str>) -> bool {
    let mut sockets = socket_files(dir).into_iter();
    sockets.any(|p| name.is_none_or(|name| is_named(&p, name)) && UnixStream::connect(p).is_ok())
}

/// Sends `command` to the Glimt used last, or else to another one. Returns false if none runs.
pub fn send(command: &str) -> bool {
    let last = runtime_dir()
        .and_then(|d| crate::images::decode::read_text(d.join(FOCUSED_FILE)).ok())
        .and_then(|id| socket(id.trim()));
    if last.is_some_and(|p| send_and_print_answer(&p, command)) {
        return true;
    }
    socket_files(runtime_dir())
        .iter()
        .any(|p| send_and_print_answer(p, command))
}

/// Sends `command` to every Glimt started with `--name name`, as several windows may share a
/// name, in the runtime folder `dir`. Returns false if none runs. Only their sockets are
/// connected to, so the windows of other names see no connection.
pub fn send_to(dir: Option<PathBuf>, name: &str, command: &str) -> bool {
    let named = socket_files(dir).into_iter().filter(|p| is_named(p, name));
    named.filter(|p| send_and_print_answer(p, command)).count() > 0
}

/// Send `command` to every running Glimt. Returns how many got it.
pub fn send_all(command: &str) -> usize {
    socket_files(runtime_dir())
        .iter()
        .filter(|p| send_and_print_answer(p, command))
        .count()
}

/// `glimt --remote` and `--remote-all`. It sends `words` (the command and its arguments) to the
/// Glimt used last, to the one of `start`'s --name, or to every one, and `paths` are `select`'s
/// files, whole. With `start`'s --wait, a command that is no query is answered only when the
/// work it started has ended. Returns `None` once the command is done and its answer printed,
/// or the path to open when `open` finds no Glimt running. It exits with `NOT_SENT` for a
/// command that cannot be sent, and with what `exit_code` chooses from the windows' answers.
pub fn client(
    words: &[String],
    paths: &[std::ffi::OsString],
    start: &crate::cli::Start,
    to_all: bool,
) -> Option<PathBuf> {
    let (name, wait) = (start.name.as_deref(), start.wait);
    // The command is checked by its word now, and sent with the word as given, so an older
    // Glimt still takes an old word.
    let now = words_now(words);
    if !is_known_command(&now) {
        eprintln!("glimt: no such command, or not with these arguments; glimt --help lists them");
        std::process::exit(NOT_SENT);
    }
    if now[0] == "run" && Action::for_script(&now[1]).is_none() {
        eprintln!(
            "glimt: {} is not an action a script may run; glimt --action-list lists them",
            words[1]
        );
        std::process::exit(NOT_SENT);
    }
    let mut command = words.join(" ");
    // select and info send whole paths after a NUL each, because names may hold spaces. A line
    // ends the command.
    if now[0] == "select" || (now[0] == "info" && !paths.is_empty()) {
        if paths.len() > MAX_SELECT {
            eprintln!("glimt: {} takes at most {} files", now[0], MAX_SELECT);
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
        if !is_running(runtime_dir(), name) {
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
        command = format!("{} {}", words[0], open_path_sent(&path));
    }
    // A line ends a command at the window, so a line break in a file name would be read as a
    // command of its own, such as quit.
    if command.contains('\n') {
        eprintln!("glimt: cannot send a line break in a command or file name");
        std::process::exit(NOT_SENT);
    }
    let line = match wait && !is_query(&command) {
        true => format!("{WAIT_WORD}{command}"),
        false => command.clone(),
    };
    let sent = match (name, to_all) {
        (_, true) => send_all(&line) > 0,
        (Some(name), false) => send_to(runtime_dir(), name, &line),
        (None, false) => send(&line),
    };
    let answered = ANSWERED.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(code) = exit_code(answered, timed_out(), was_not_done()) {
        std::process::exit(code);
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

/// Returns `path` of `--remote open` as it is sent to a window, or given to a new Glimt: a
/// network location as it is, to be mounted there, and a path in full, because the window has
/// another working folder.
fn open_path_sent(path: &str) -> String {
    if let Some(uri) = crate::files::external::network_location(path) {
        return uri.to_owned();
    }
    let full = std::path::absolute(path).unwrap_or_else(|_| path.into());
    full.display().to_string()
}

/// Listens for commands in the background and passes each line to `tx`, with where to answer
/// for a query. Returns the reason when the window cannot listen.
pub fn listen(
    tx: mpsc::Sender<(String, Option<Reply>)>,
    ctx: eframe::egui::Context,
) -> Result<(), String> {
    let Some(path) = socket(&socket_id()) else {
        return Err(
            match runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR")) {
                None => "No remote control: $XDG_RUNTIME_DIR is not set to a full path".into(),
                Some(_) => "No remote control: its folder there is not this user's own".into(),
            },
        );
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
            return Err("No remote control: its folder there is not this user's own".into());
        }
    }
    // A window whose --name is taken already shares it, on a socket of its own: the name and
    // its process ID.
    let mut id = socket_id();
    let mut bound = bind_socket(&path);
    if let (Err(None), Some(name)) = (&bound, given_name()) {
        id = format!("{name}.{}", instance_id());
        bound = socket(&id).map_or(Err(None), |p| bind_socket(&p));
    }
    let listener = match bound {
        Ok(listener) => listener,
        Err(None) => {
            return Err(format!(
                "A Glimt of ID {id} is running already: --remote goes to that one"
            ));
        }
        Err(Some(e)) => return Err(format!("No remote control as {id}: {e}")),
    };
    *lock_even_if_poisoned(&LISTENING_AS) = Some(id);
    LISTENING.store(true, std::sync::atomic::Ordering::Relaxed);
    // The accept thread below keeps the listener, so its number stays valid for
    // `has_waiting_client`.
    LISTENER.store(
        std::os::fd::AsRawFd::as_raw_fd(&listener),
        std::sync::atomic::Ordering::Relaxed,
    );
    mark_this_window_focused();
    std::thread::spawn(move || {
        serve_clients(listener, MOST_CLIENTS, move |stream| {
            serve(stream, &tx, &ctx, WAIT)
        });
    });
    Ok(())
}

/// Returns the name that `command` gives the window, empty to take its name away, or `None`
/// for another command.
fn name_asked(command: &str) -> Option<&str> {
    match command.trim() {
        "name" => Some(""),
        other => other.strip_prefix("name ").map(str::trim),
    }
}

/// Gives this window the --name `name`, or none, and listens for `--remote` by the rule it
/// started with (`listens`): one that did not, with remote control off, starts to with a name,
/// and stops again when the name is taken away. Returns `Err(None)` for a name that a window
/// cannot take, and the reason when the window cannot listen by it. The window then keeps the
/// name it had. It renames the socket, so it is called in a thread of its own.
fn name_this_window(
    name: Option<&str>,
    listens_without_name: bool,
    commands: mpsc::Sender<(String, Option<Reply>)>,
    ctx: &egui::Context,
) -> Result<(), Option<String>> {
    if name.is_some_and(|n| !can_be_window_name(n)) {
        return Err(None);
    }
    let should_listen = listens(listens_without_name, name.is_some());
    if listening() && !should_listen {
        stop_listening();
        return Ok(());
    }
    let name_before = given_name();
    remote::rename(name).map_err(Some)?;
    if !listening()
        && should_listen
        && let Err(why) = listen(commands, ctx.clone())
    {
        // The window does not listen, so this only puts the name back.
        remote::rename(name_before.as_deref()).map_err(Some)?;
        return Err(Some(why));
    }
    Ok(())
}

/// Whether a window listens for `--remote`: when it does without a name (`listens_without_name`,
/// with remote control on in Preferences at start, or in a kiosk), or when it has a name, as
/// a name is only of use with remote control. The same rule holds at start and for a name
/// given or taken away later.
fn listens(listens_without_name: bool, has_name: bool) -> bool {
    listens_without_name || has_name
}

/// Stops listening for `--remote` and forgets the name, for a window that listened only because
/// it had one. A client that is being answered still gets its answer.
// shortcut: the accept thread keeps waiting on the listener, which no client reaches once its
// socket is removed, so one thread and socket stay for each name taken away; end the thread if
// names are taken away often.
fn stop_listening() {
    close();
    LISTENING.store(false, std::sync::atomic::Ordering::Relaxed);
    *lock_even_if_poisoned(&LISTENING_AS) = None;
    *lock_even_if_poisoned(&GIVEN_NAME) = None;
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

/// One client's commands, each passed to the window, and its answer to a query sent back. The
/// client has `wait` (`WAIT`) to send each command, and the window a second more to answer it.
fn serve(
    stream: UnixStream,
    tx: &mpsc::Sender<(String, Option<Reply>)>,
    ctx: &eframe::egui::Context,
    wait: std::time::Duration,
) {
    let _ = stream.set_read_timeout(Some(wait));
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
        let comes_in_parts = is_info(&line);
        // A command sent with `--wait` is answered when its work has ended, so it has longer.
        let limit = match line.starts_with(WAIT_WORD) {
            true => OUTCOME_WAIT,
            false => wait,
        } + std::time::Duration::from_secs(1);
        if tx.send((line, Some(reply))).is_err() {
            return;
        }
        ctx.request_repaint();
        // The client gives up after `wait`, so a command that waits longer for a look-up that
        // does not answer lets its thread and its place among `MOST_CLIENTS` go. The command
        // may still be done later, and its answer then goes nowhere.
        match answer.recv_timeout(limit) {
            Ok(bytes) => {
                // `info` sends each file's record as it is read, until it has sent them all.
                // Each part is written at once, so the client's time limit holds for each part.
                let rest = answer.iter().take_while(|_| comes_in_parts);
                for bytes in std::iter::once(bytes).chain(rest) {
                    if out
                        .as_mut()
                        .is_none_or(|out| out.write_all(&bytes).is_err())
                    {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(out) = out.as_mut() {
                    let _ = out.write_all(&not_done_answer("no answer"));
                }
                return;
            }
            // When the window dropped the answer (too many waiting, or closing), closing the
            // connection now would make the client read nothing, "all found" or "done". Held
            // open, its time limit says "no answer".
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                drop(answering);
                return std::thread::sleep(limit);
            }
        }
        drop(answering);
    }
}

/// Reads the next command, one line, from `reader`. A `select` or `info` may be as long as its
/// files take, and any other command at most `MOST_LINE` bytes, so a client that sends without
/// a line break holds no more memory than that. Returns the reason for a line too long, which
/// the client is told, and `None` at the end, for a line that is not UTF-8, and when the client
/// sends nothing within its time limit.
fn read_command(reader: &mut impl BufRead) -> Option<Result<String, &'static str>> {
    let mut line = Vec::new();
    std::io::Read::take(&mut *reader, MOST_LINE)
        .read_until(b'\n', &mut line)
        .ok()?;
    let ended = line.ends_with(b"\n");
    let takes_files = |line: &[u8]| line.starts_with(b"select ") || line.starts_with(b"info ");
    if !ended && takes_files(line.trim_ascii_start()) {
        // The rest is read in parts, and the files counted, so that a select of more files than
        // `MAX_SELECT` is refused here, and not split into a list on the window thread.
        let mut files = line.iter().filter(|&&b| b == 0).count();
        loop {
            let start = line.len();
            std::io::Read::take(&mut *reader, MOST_LINE)
                .read_until(b'\n', &mut line)
                .ok()?;
            files += line[start..].iter().filter(|&&b| b == 0).count();
            if files > MAX_SELECT {
                return Some(Err(TOO_MANY_FILES));
            }
            if line.len() == start || line.ends_with(b"\n") {
                break;
            }
        }
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
    let _lock = crate::files::file_io::open_and_lock(&path.with_extension("lock")).map_err(Some)?;
    if answers(path) {
        return Err(None);
    }
    #[allow(clippy::disallowed_methods)] // a socket left by a Glimt gone
    let _ = std::fs::remove_file(path);
    // Another window can take the name between the remove and this bind, as a rename links its
    // socket there without this lock. Its socket then answers, so it counts as taken.
    UnixListener::bind(path).map_err(|e| (e.kind() != std::io::ErrorKind::AddrInUse).then_some(e))
}

/// Gives this Glimt the --name `name`, or no name for `None`, so that `--remote` reaches it by
/// the new name, or by its process ID, from now on and no longer by the old one. A Glimt that
/// listens keeps its socket and links it to the new ID before the old ID is removed, so it is
/// never without remote control. The new name is shared, as at start, when another window has
/// it already. Returns the reason when the name cannot be taken.
pub fn rename(name: Option<&str>) -> Result<(), String> {
    if name.is_some_and(|n| !can_be_window_name(n)) {
        return Err(NOT_A_NAME.into());
    }
    if !listening() {
        *lock_even_if_poisoned(&GIVEN_NAME) = name.map(str::to_owned);
        return Ok(());
    }
    let dir = runtime_dir().ok_or("there is no runtime folder")?;
    let ids = match name {
        Some(n) => vec![n.to_owned(), format!("{n}.{}", instance_id())],
        None => vec![instance_id().to_owned()],
    };
    let id = move_socket(&dir, &own_socket_id(), &ids)?;
    *lock_even_if_poisoned(&GIVEN_NAME) = name.map(str::to_owned);
    *lock_even_if_poisoned(&LISTENING_AS) = Some(id);
    Ok(())
}

/// Moves this Glimt's socket in `dir` from the ID `old` to the first of `ids` that is free and
/// that a client can connect to. The socket is linked to the new ID before the old one is
/// removed, so the window is never without remote control. The window used last keeps that
/// place under its new ID. Returns the ID taken, or the reason when none of them can be taken.
fn move_socket(dir: &Path, old: &str, ids: &[String]) -> Result<String, String> {
    let from = dir.join(socket_name(old));
    let mut why = "a Glimt of that name and process ID runs already";
    for id in ids {
        let to = dir.join(socket_name(id));
        if !socket_path_fits(&to) {
            why = "the name is too long for the socket's path here";
            continue;
        }
        if id != old {
            match link_socket(&from, &to) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
            #[allow(clippy::disallowed_methods)] // this Glimt's own socket, linked anew
            let _ = std::fs::remove_file(&from);
        }
        let focused = crate::images::decode::read_text(dir.join(FOCUSED_FILE)).ok();
        if focused.is_some_and(|f| f.trim() == old) {
            mark_focused(dir, id);
        }
        return Ok(id.clone());
    }
    Err(why.into())
}

/// Links this Glimt's socket at `from` to `to` as well, never over a socket that a running
/// Glimt answers on. One left by a Glimt gone is replaced, under the lock that `bind_socket`
/// takes too.
fn link_socket(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // a link to Glimt's own socket, never over a file
    let first = std::fs::hard_link(from, to);
    match first {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _lock = crate::files::file_io::open_and_lock(&to.with_extension("lock"))?;
            if answers(to) {
                return Err(e);
            }
            #[allow(clippy::disallowed_methods)] // a socket left by a Glimt gone
            let _ = std::fs::remove_file(to);
            #[allow(clippy::disallowed_methods)] // a link to Glimt's own socket, never over a file
            std::fs::hard_link(from, to)
        }
        other => other,
    }
}

/// Returns whether a running Glimt answers on the socket at `path`. The connection is tried in
/// a thread of its own, because a connection to a Glimt that hangs with a full queue of clients
/// waits until that Glimt takes it, and the window that asks must not wait with it. A connection
/// that has not come after `PROBE_WAIT` counts as answered, as only a running Glimt holds one
/// back that long.
// shortcut: the thread waits as long as the hung Glimt does, one for each probe; give the
// connection a deadline of its own if hung windows are probed often.
fn answers(path: &Path) -> bool {
    let path = path.to_path_buf();
    connected_in_time(move || UnixStream::connect(path).is_ok(), PROBE_WAIT)
}

/// Runs `connect` in a thread of its own and returns what it returns. Returns true when it has
/// not returned after `limit`, as `answers` says.
fn connected_in_time(
    connect: impl FnOnce() -> bool + Send + 'static,
    limit: std::time::Duration,
) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(connect());
    });
    rx.recv_timeout(limit).unwrap_or(true)
}

/// Marks that this Glimt got the focus, so `--remote` goes to it from now on, when it listens.
/// A window that does not listen is never marked, as `--remote` could not reach it.
pub fn mark_this_window_focused() {
    if !listening() {
        return;
    }
    if let Some(dir) = runtime_dir() {
        mark_focused(&dir, &own_socket_id());
    }
}

/// Writes `id` as the ID of the Glimt that had the focus last, in the runtime folder `dir`.
fn mark_focused(dir: &Path, id: &str) {
    // The file is written whole and never truncated first, because a `--remote` reading it
    // meanwhile got an empty name.
    let path = dir.join(FOCUSED_FILE);
    if let Err(e) = crate::files::file_io::replace_file(&path, id) {
        eprintln!("glimt: {}: {e}", path.display());
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
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    if fd < 0 {
        return false;
    }
    #[allow(unsafe_code)]
    // SAFETY: There is one pollfd and no wait, and a closed fd is only reported, not used.
    let ready = unsafe { libc::poll(&mut p, 1, 0) } == 1;
    ready && p.revents & libc::POLLIN != 0
}

/// Takes no new clients, by removing this Glimt's socket as it closes.
pub fn close() {
    if LISTENING.load(std::sync::atomic::Ordering::Relaxed)
        && let Some(path) = socket(&own_socket_id())
    {
        #[allow(clippy::disallowed_methods)] // this Glimt's own socket
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;
    use eframe::egui;

    /// The exit code keeps each frozen meaning when windows disagree. Before, one window that
    /// did not answer made the exit 1 ("none answered") though another had done the command,
    /// so a script that retried did it twice there, and it hid a 3 of another window.
    #[test]
    fn the_exit_code_tells_what_the_windows_did() {
        use super::{NOT_DONE, NOT_TAKEN, exit_code};
        // (answered, timed out, not done)
        assert_eq!(exit_code(true, false, false), None, "done");
        assert_eq!(exit_code(false, false, false), None, "none took it");
        assert_eq!(
            exit_code(false, true, false),
            Some(NOT_TAKEN),
            "none answered"
        );
        assert_eq!(exit_code(true, false, true), Some(NOT_DONE), "one said no");
        assert_eq!(
            exit_code(true, true, false),
            Some(NOT_DONE),
            "one done, one silent"
        );
        assert_eq!(
            exit_code(true, true, true),
            Some(NOT_DONE),
            "one said no, one silent"
        );
    }

    /// A URI given to `--remote open` is sent as it is, to be mounted, as `glimt URI` mounts it.
    /// Before, it was made a path in the working folder, `$PWD/smb:/nas`, which does not exist.
    #[test]
    fn an_open_uri_is_sent_as_it_is() {
        assert_eq!(
            super::open_path_sent("smb://nas/photos"),
            "smb://nas/photos"
        );
        assert_eq!(super::open_path_sent("/a/b.jpg"), "/a/b.jpg");
        let here = std::env::current_dir().unwrap().join("b.jpg");
        assert_eq!(super::open_path_sent("b.jpg"), here.display().to_string());
    }

    /// A window given `open` with a URI mounts it, as at start, and does not look for a file
    /// of that name.
    #[test]
    fn an_open_uri_is_mounted() {
        let dir = std::env::temp_dir().join(format!("glimt-test-open-uri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        // An address that starts with "-" is refused before gio runs, so no program starts.
        app.remote_command(&ctx, "open -glimt-test://nas/photos")
            .unwrap();
        assert!(app.folder.is_mounting());
        // The commands sent after it wait for the mount, as for a look-up. Before, they acted
        // at once on the list of the folder from before.
        assert!(app.must_wait("select /a.jpg", true));
        assert!(app.must_wait("next", false));
        assert!(!app.must_wait("open /a.jpg", false));
        std::fs::remove_dir_all(dir).unwrap();
    }

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
        // A window that shares a name is reached alone by its own ID, and shows both.
        assert!(named("/run/glimt/glimt-left.4242.sock", "4242"));
        assert!(named("/run/glimt/glimt-left.app-1-4242.sock", "app-1-4242"));
        assert!(!named("/run/glimt/glimt-left.4242.sock", "424"));
        assert!(!named("/run/glimt/glimt-left.sock", "4242"));
        assert_eq!(super::shown_id("left.4242"), "left (4242)");
        assert_eq!(super::shown_id("left"), "left");
    }

    /// The start's listening is answered to the window as a name is, so that the title shows
    /// the ID the window listens as: a window that shares its name shows its process ID too.
    /// Before, the socket was made in the background and the title never learned of it.
    #[test]
    fn the_listening_at_start_is_answered_to_the_window() {
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ctx = eframe::egui::Context::default();
        let mut listener = super::Listener::start(&ctx, &busy, false, Some(|_, _| Ok(())));
        let answered = (0..250).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            matches!(
                listener.name_answers.next(),
                crate::support::threads::Took::Answer((
                    super::NameAnswer {
                        asked_by: super::NameAskedBy::Start,
                        taken: Ok(())
                    },
                    _
                ))
            )
        });
        assert!(answered);
    }

    /// A window's socket is linked to its new name, so it answers there, but never over the
    /// socket of a Glimt that answers. One left by a Glimt gone is replaced.
    #[test]
    fn a_new_name_links_the_socket() {
        let dir = std::env::temp_dir().join(format!("glimt-test-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.sock"), dir.join("b.sock"), dir.join("c.sock"));
        let _listener = std::os::unix::net::UnixListener::bind(&a).unwrap();
        super::link_socket(&a, &b).unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&b).is_ok());
        let taken = super::link_socket(&a, &b).unwrap_err();
        assert_eq!(taken.kind(), std::io::ErrorKind::AlreadyExists);
        drop(std::os::unix::net::UnixListener::bind(&c).unwrap());
        // A process another test starts holds a copy of the closed socket until it runs its
        // program, so it can answer a moment longer, as in `claims_only_a_left_socket`.
        let replaced = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            super::link_socket(&a, &c).is_ok()
        });
        assert!(replaced);
        assert!(std::os::unix::net::UnixStream::connect(&c).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A look for a running Glimt on a socket does not wait for a connection that a Glimt that
    /// hangs with a full queue of clients holds back, and counts it as answered. Before, a
    /// rename to the name of such a Glimt hung the renamed window too. A connection that takes
    /// ten seconds stands in for one held back.
    #[test]
    fn a_connection_held_back_counts_as_answered_without_waiting() {
        let (_never, held_back) = std::sync::mpsc::channel::<()>();
        let slow = move || {
            held_back
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok()
        };
        let started = std::time::Instant::now();
        assert!(super::connected_in_time(
            slow,
            std::time::Duration::from_millis(100)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(!super::connected_in_time(
            || false,
            std::time::Duration::from_secs(5)
        ));
    }

    /// A new name whose socket's path would be too long for a client to connect to is not
    /// taken, so the window keeps its old socket, and with it remote control.
    #[test]
    fn a_name_too_long_for_the_socket_keeps_the_old_one() {
        let base = std::env::temp_dir().join(format!("glimt-test-long-{}-", std::process::id()));
        // The path of `glimt-left.sock` takes the 107 bytes exactly, and `left.123456` more.
        let padding = 107 - "/glimt-left.sock".len() - base.as_os_str().len();
        let dir = std::path::PathBuf::from(format!("{}{}", base.display(), "x".repeat(padding)));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(super::socket_name("old"));
        let _own = std::os::unix::net::UnixListener::bind(&old).unwrap();
        let _taken = std::os::unix::net::UnixListener::bind(dir.join("glimt-left.sock")).unwrap();
        let ids = ["left".to_owned(), "left.123456".to_owned()];
        assert!(super::move_socket(&dir, "old", &ids).is_err());
        assert!(std::os::unix::net::UnixStream::connect(&old).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A rename of a window that listens: its socket answers under the new name and no longer
    /// under the old one, and the window used last keeps that place. When the name is taken,
    /// the window shares it under the name and its own ID.
    #[test]
    fn a_rename_moves_the_listening_socket() {
        use std::os::unix::net::{UnixListener, UnixStream};
        let dir = std::env::temp_dir().join(format!("glimt-test-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = |id: &str| dir.join(super::socket_name(id));
        let _own = UnixListener::bind(socket("old")).unwrap();
        let focused = dir.join(super::FOCUSED_FILE);
        std::fs::write(&focused, "old").unwrap();
        let ids = ["right".to_owned(), "right.42".to_owned()];
        assert_eq!(
            super::move_socket(&dir, "old", &ids),
            Ok("right".to_owned())
        );
        assert!(UnixStream::connect(socket("right")).is_ok());
        assert!(!socket("old").exists());
        assert_eq!(std::fs::read_to_string(&focused).unwrap(), "right");
        // Another window has `left`, so this one is `left.42`; the focused file is another's.
        let _other = UnixListener::bind(socket("left")).unwrap();
        std::fs::write(&focused, "7").unwrap();
        let ids = ["left".to_owned(), "left.42".to_owned()];
        assert_eq!(
            super::move_socket(&dir, "right", &ids),
            Ok("left.42".to_owned())
        );
        assert!(UnixStream::connect(socket("left.42")).is_ok());
        assert!(!socket("right").exists());
        assert_eq!(std::fs::read_to_string(&focused).unwrap(), "7");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A command sent to a name connects only to the windows of that name, so a window of
    /// another name sees no connection that sends nothing.
    #[test]
    fn a_command_to_a_name_connects_only_to_its_windows() {
        let dir = std::env::temp_dir().join(format!("glimt-test-send-to-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = std::os::unix::net::UnixListener::bind(dir.join("glimt-right.sock")).unwrap();
        other.set_nonblocking(true).unwrap();
        assert!(!super::send_to(Some(dir.clone()), "left", "next"));
        // Every connection waiting on the other window is counted, also one closed at once.
        let connections = std::iter::from_fn(|| other.accept().ok()).count();
        assert_eq!(connections, 0, "the window named right was connected to");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `name` takes one name or none, and a name that --name would refuse is not taken.
    #[test]
    fn name_takes_only_a_valid_name() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(super::is_known_command(&words("name")));
        assert!(super::is_known_command(&words("name left")));
        assert!(!super::is_known_command(&words("name left right")));
        for name in ["123", "a/b", "a.b"] {
            assert!(super::rename(Some(name)).is_err(), "{name}");
        }
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
        let none = crate::files::file_io::PickedFiles::new(Vec::new());
        let asked = crate::ui::dialogs::Asked::new(0, vec!["a.png".into()], None, none);
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
            "run ParentFolder",
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
        let server =
            std::thread::spawn(move || super::serve(window, &tx, &window_ctx, super::WAIT));
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
        let server =
            std::thread::spawn(move || super::serve(window, &tx, &window_ctx, super::WAIT));
        app.remote = super::Listener {
            rx,
            tx: std::sync::mpsc::channel().0,
            waiting_commands: Vec::new(),
            identify_until: None,
            listens_without_name: false,
            name_sender: super::answer_channel(&app.busy).0,
            name_answers: Default::default(),
            awaiting_outcome: Vec::new(),
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
        let none = crate::files::file_io::PickedFiles::new(Vec::new());
        let asked = crate::ui::dialogs::Asked::new(0, vec!["a.png".into()], None, none);
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
        std::thread::spawn(move || {
            super::serve(window, &tx, &eframe::egui::Context::default(), super::WAIT)
        });
        writeln!(client, "nxet").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let (line, reply) = rx.recv().unwrap();
        app.handle_remote(&ctx, &line, reply);
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        assert!(super::not_done_reason(&got).is_some(), "{got:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A command sent with `--wait` is answered when the work it started has ended, and not
    /// done when a write failed meanwhile. A command the window did not do is answered at once,
    /// as without `--wait`, and a query as without it. A command that closes the window is
    /// answered as it closes, after its last writes, or at once when a write failed. Before,
    /// "done" came when the command was taken, and a script never learnt of a failed write.
    #[test]
    fn a_command_sent_with_wait_tells_the_outcome_of_its_work() {
        let dir = std::env::temp_dir().join(format!("glimt-test-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        let send = |app: &mut crate::App, command: &str| {
            let (reply, answer) = std::sync::mpsc::channel();
            app.remote
                .tx
                .send((command.to_owned(), Some(reply)))
                .unwrap();
            answer
        };
        // Other tests write Glimt's own files beside this one, so the window may wait for them.
        let outcome = |app: &mut crate::App, answer: &std::sync::mpsc::Receiver<Vec<u8>>| {
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(20) {
                app.receive_remote(&ctx);
                app.receive_answers_without_a_frame(&ctx);
                if let Ok(got) = answer.try_recv() {
                    return Some(got);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            None
        };
        // Done, with nothing left to wait for.
        let answer = send(&mut app, "wait next");
        assert_eq!(
            outcome(&mut app, &answer).unwrap(),
            super::OUTCOME_DONE.as_bytes()
        );
        assert_eq!(app.list.shown(), 1);
        // Work in the background holds the answer, and a write that fails meanwhile is told.
        let work = crate::support::threads::Working::new(&app.busy);
        let answer = send(&mut app, "wait next");
        app.receive_remote(&ctx);
        assert_eq!(app.list.shown(), 2);
        assert!(answer.try_recv().is_err());
        app.tell_failed_write("Could not save the settings".into());
        drop(work);
        let got = outcome(&mut app, &answer).unwrap();
        let why = super::not_done_reason(&got);
        assert_eq!(why.as_deref(), Some("Could not save the settings"));
        // Not done by the window, at once and with its reason.
        let answer = send(&mut app, "wait nxet");
        let why = super::not_done_reason(&outcome(&mut app, &answer).unwrap());
        assert!(why.is_some_and(|w| w.contains("nxet")));
        // A query is answered as without --wait.
        let answer = send(&mut app, "wait current");
        let got = outcome(&mut app, &answer).unwrap();
        assert!(got.ends_with(b"c.png\0"), "{got:?}");
        // A quit waits for the close, and a write that failed keeps the window open.
        let answer = send(&mut app, "wait quit");
        for _ in 0..10 {
            app.receive_remote(&ctx);
        }
        assert!(answer.try_recv().is_err());
        // As `on_exit` does it, after the writes, which here are other tests'.
        let start = std::time::Instant::now();
        while app.is_writing() && start.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.answer_awaited_outcomes(&ctx, true);
        assert_eq!(answer.try_recv().unwrap(), super::OUTCOME_DONE.as_bytes());
        let answer = send(&mut app, "wait run Quit");
        app.receive_remote(&ctx);
        app.tell_failed_write("Could not save the state".into());
        let why = super::not_done_reason(&outcome(&mut app, &answer).unwrap());
        assert_eq!(why.as_deref(), Some("Could not save the state"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A write in a thread of its own (`threads::told_work`) that breaks while a `--wait`
    /// command waits makes that command not done: the failure is in the window before the work
    /// stops counting as writing.
    #[test]
    fn wait_sees_a_failure_told_by_a_writing_thread() {
        let dir = std::env::temp_dir().join(format!("glimt-test-wait-told-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png"].map(Into::into).to_vec());
        // The work runs when the command comes, so the command waits for it.
        assert!(app.run_in_background(&ctx, Vec::new(), || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            panic!("a broken file")
        }));
        let (reply, answer) = std::sync::mpsc::channel();
        app.remote
            .tx
            .send(("wait next".to_owned(), Some(reply)))
            .unwrap();
        let start = std::time::Instant::now();
        let got = loop {
            app.receive_remote(&ctx);
            if let Ok(got) = answer.try_recv() {
                break got;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(20));
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let why = super::not_done_reason(&got);
        assert!(
            why.is_some_and(|w| w.contains("could not be saved")),
            "{got:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The client reads `OUTCOME_DONE` as done, and any other answer to a command sent with
    /// `--wait` as that of a window older than `--wait`. A window of 0.75 or older answers
    /// nothing, which without `--wait` counts as done.
    #[test]
    fn an_answer_without_the_outcome_is_not_done() {
        assert_eq!(
            super::waited_outcome_reason(super::OUTCOME_DONE.as_bytes()),
            None
        );
        assert!(super::waited_outcome_reason(b"").is_some());
        assert!(super::closes_the_window("quit") && super::closes_the_window("run CloseWindow"));
        assert!(!super::closes_the_window("run Next"));
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
        // A select or info of more files than `MAX_SELECT` is refused when it is read, from
        // any program on the socket. Before, only Glimt's own client counted them.
        let files = |n: usize| "/x\0".repeat(n);
        let most = format!("select {}", files(super::MAX_SELECT));
        assert_eq!(read(format!("{most}\n")), [Ok(most)]);
        for command in ["select", "info"] {
            let more = format!("{command} {}\nnext\n", files(super::MAX_SELECT + 1));
            assert_eq!(read(more), [Err(super::TOO_MANY_FILES)]);
        }
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
        let server = std::thread::spawn(move || super::serve(window, &tx, &ctx, super::WAIT));
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

    /// A command whose answer does not come in time (it waits for a look-up that never answers)
    /// is told "no answer", and its client's place is given to the next client. Before, the
    /// thread waited for ever, and 16 such clients kept every other one out.
    #[test]
    fn a_command_not_answered_in_time_frees_its_place() {
        use std::io::{Read, Write};
        let path = std::env::temp_dir().join(format!("glimt-slow-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let wait = std::time::Duration::from_millis(200);
        let serve = move |stream| {
            super::serve(stream, &tx, &eframe::egui::Context::default(), wait);
        };
        std::thread::spawn(move || super::serve_clients(listener, 1, serve));
        let connect = || std::os::unix::net::UnixStream::connect(&path).unwrap();
        let (mut first, mut second) = (connect(), connect());
        writeln!(first, "current").unwrap();
        writeln!(second, "current").unwrap();
        let five_seconds = std::time::Duration::from_secs(5);
        // Both replies are kept, as the window keeps a waiting command's, and never answered.
        let first_reply = rx.recv_timeout(five_seconds).unwrap();
        let second_reply = rx.recv_timeout(five_seconds);
        assert!(second_reply.is_ok(), "the second client was not served");
        let mut got = Vec::new();
        first.read_to_end(&mut got).unwrap();
        assert_eq!(super::not_done_reason(&got).as_deref(), Some("no answer"));
        drop((first_reply, second_reply));
        std::fs::remove_file(path).unwrap();
    }

    /// A query whose answer the window drops (too many waiting) is not closed at once, because
    /// the client would read an empty answer, "all found". It is left to time out, "no answer".
    #[test]
    fn a_dropped_answer_is_no_answer() {
        use std::io::{Read, Write};
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            super::serve(server, &tx, &eframe::egui::Context::default(), super::WAIT)
        });
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

    /// A socket that does not answer is passed over but left in place. A Glimt between binding
    /// it and listening refuses connections just as a crashed one's socket does, and removing
    /// it would leave that Glimt unreachable.
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
            !super::is_running(Some(dir.clone()), Some("1"))
        });
        assert!(passed_over);
        assert!(super::is_running(Some(dir.clone()), Some("2")));
        assert!(quiet.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `open` with a path that cannot be sent finds a running window of a name as every other
    /// command does (`is_named`): also one that shares the name, and by its own ID. Otherwise
    /// a new Glimt started and took the name.
    #[test]
    fn a_window_of_a_name_is_found_running_by_each_of_its_sockets() {
        let dir = std::env::temp_dir().join(format!("glimt-test-running-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _listening =
            std::os::unix::net::UnixListener::bind(dir.join("glimt-left.4242.sock")).unwrap();
        assert!(super::is_running(Some(dir.clone()), Some("left")));
        assert!(super::is_running(Some(dir.clone()), Some("4242")));
        assert!(super::is_running(Some(dir.clone()), None));
        assert!(!super::is_running(Some(dir.clone()), Some("right")));
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

    /// A link planted at the lock of a left socket is not followed, so the file it points to is
    /// neither emptied nor locked, and the left socket is not replaced. Before, the lock was
    /// opened with `File::create`, which emptied it.
    #[test]
    fn the_socket_lock_leaves_a_planted_link_alone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-sock-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, target) = (dir.join("glimt.sock"), dir.join("notes.txt"));
        // A socket left by a Glimt gone, which answers no one.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, path.with_extension("lock")).unwrap();
        assert!(super::bind_socket(&path).is_err_and(|e| e.is_some()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A socket that another window takes after a left one was removed and before this window
    /// binds its own, as by a rename to this name, counts as taken. The window then shares the
    /// name under its own ID. Before, it had no remote control. A folder stands in for the
    /// other window's socket, as it cannot be removed either.
    #[test]
    fn a_socket_taken_while_a_left_one_is_replaced_counts_as_taken() {
        let path = std::env::temp_dir().join(format!("glimt-taken-{}.sock", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        assert!(super::bind_socket(&path).is_err_and(|e| e.is_none()));
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }

    /// `--remote selection` answers each selected file's path as `App::path` makes it, also for
    /// a name in a subfolder (a slideshow with subfolders) and a whole path (a collection), with
    /// the path made in one buffer rather than anew for each file.
    #[test]
    fn the_selection_is_answered_with_each_path() {
        let dir = std::env::temp_dir().join(format!("glimt-test-sel-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _ctx) = app_on(&dir);
        let names = ["a.png", "sub/b.png", "/elsewhere/c.png"];
        app.list.set_files(names.map(Into::into).to_vec());
        app.list.select((0..3).collect());
        let (a, b) = (dir.join("a.png"), dir.join("sub/b.png"));
        let answer = format!("{}\0{}\0/elsewhere/c.png\0", a.display(), b.display());
        assert_eq!(app.remote_query("selection"), answer.into_bytes());
        std::fs::remove_dir_all(dir).unwrap();
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
        assert!(app.notice.text().is_some());
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
        // A query is answered at once, also behind a waiting select.
        let (reply, current) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        tx.send((format!("open {}", other.display()), None))
            .unwrap();
        // Behind a waiting open, it waits its turn, so it answers about the place opened.
        let (reply, current_after_open) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        // At most 64 wait, so the last of these are dropped.
        let mut more = Vec::new();
        for _ in 0..63 {
            let (reply, answer) = std::sync::mpsc::channel();
            tx.send((select.clone(), Some(reply))).unwrap();
            more.push(answer);
        }
        app.receive_remote(&ctx);
        assert!(current.try_recv().is_ok(), "current answered at once");
        assert!(current_after_open.try_recv().is_err());
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
        // The open runs once the select is answered, and its folder is looked up in the
        // background.
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert_eq!(app.folder.dir(), other);
        app.receive_remote(&ctx);
        assert!(current_after_open.try_recv().is_ok());
        // Closing with selects still waiting, and one sent just before, tells each that none of
        // its paths was selected.
        let (reply, closing) = std::sync::mpsc::channel();
        app.remote
            .waiting_commands
            .push((select.clone(), Some(reply)));
        let (reply, late) = std::sync::mpsc::channel();
        tx.send((select.clone(), Some(reply))).unwrap();
        app.answer_waiting_selects();
        assert!(String::from_utf8_lossy(&closing.try_recv().unwrap()).contains("a.png"));
        assert!(String::from_utf8_lossy(&late.try_recv().unwrap()).contains("a.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Runs `receive_remote` alone, as a hidden window's `logic` does without `ui`, until
    /// `done` holds, for at most five seconds. Returns whether it held.
    fn receive_remote_until(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        mut done: impl FnMut(&mut crate::App) -> bool,
    ) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            app.receive_remote(ctx);
            if done(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// The commands after `open` wait until the place it named is looked up, also in a window
    /// that runs no `ui`, so they act on that place and not on the list from before it:
    /// `current` names the file opened, and `run ParentFolder` goes up from its folder.
    #[test]
    fn commands_after_an_open_act_on_the_place_it_opens() {
        let dir = std::env::temp_dir().join(format!("glimt-test-open-then-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        image::RgbImage::new(4, 4)
            .save(other.join("b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        let b = other.join("b.png");
        let (reply, current) = std::sync::mpsc::channel();
        tx.send((format!("open {}", b.display()), None)).unwrap();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        tx.send(("run ParentFolder".to_owned(), None)).unwrap();
        let mut answer = None;
        assert!(receive_remote_until(&mut app, &ctx, |_| {
            answer = current.try_recv().ok();
            answer.is_some()
        }));
        assert_eq!(answer.unwrap(), format!("{}\0", b.display()).into_bytes());
        assert!(
            receive_remote_until(&mut app, &ctx, |app| app.folder.dir() == dir),
            "Up from {}, not from {}",
            other.display(),
            dir.display()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `slideshow on` after `open`, with subfolders in random order, runs on the folder opened
    /// and keeps its shuffle, which opening the folder would otherwise take away.
    #[test]
    fn a_slideshow_after_an_open_keeps_its_shuffle() {
        let dir = std::env::temp_dir().join(format!("glimt-test-open-show-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        image::RgbImage::new(4, 4)
            .save(other.join("b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        tx.send((format!("open {}", other.display()), None))
            .unwrap();
        tx.send(("slideshow on".to_owned(), None)).unwrap();
        let shown = receive_remote_until(&mut app, &ctx, |app| app.slideshow.is_running());
        assert!(shown && app.folder.dir() == other);
        assert!(app.slideshow.is_shuffled());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A `quit` closes the window at once, while a look-up never answers (a share that stopped
    /// answering) and a command waits for it.
    #[test]
    fn quit_does_not_wait_for_a_look_up() {
        use eframe::egui::{RawInput, ViewportCommand, ViewportId};
        let dir = std::env::temp_dir().join(format!("glimt-test-quit-now-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        let _held = app.folder.hold_look_up();
        tx.send(("next".to_owned(), None)).unwrap();
        tx.send(("quit".to_owned(), None)).unwrap();
        let out = ctx.run_logic(&RawInput::default(), |ctx| app.receive_remote(ctx));
        let commands = out.viewport_commands.get(&ViewportId::ROOT);
        assert!(commands.is_some_and(|c| c.contains(&ViewportCommand::Close)));
        assert_eq!(app.remote.waiting_commands.len(), 1, "next still waits");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An `open` replaces a look-up that never answers, also while a command waits for it, so a
    /// script can leave a share that stopped answering. The waiting command is told it was not
    /// done, and a later one acts on the place opened. Before, the `open` waited behind it.
    #[test]
    fn an_open_replaces_a_look_up_that_never_answers() {
        let dir = std::env::temp_dir().join(format!("glimt-test-open-over-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        image::RgbImage::new(4, 4)
            .save(other.join("b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote.rx = rx;
        let _held = app.folder.hold_look_up();
        let (reply, parked) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        app.receive_remote(&ctx);
        assert_eq!(app.remote.waiting_commands.len(), 1, "current waits");
        let b = other.join("b.png");
        let (reply, opened) = std::sync::mpsc::channel();
        tx.send((format!("open {}", b.display()), Some(reply)))
            .unwrap();
        let (reply, current) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        let mut answer = None;
        assert!(receive_remote_until(&mut app, &ctx, |_| {
            answer = current.try_recv().ok();
            answer.is_some()
        }));
        assert_eq!(answer.unwrap(), format!("{}\0", b.display()).into_bytes());
        let not_done = super::not_done_reason(&parked.try_recv().unwrap());
        assert_eq!(
            not_done.as_deref(),
            Some("the place is no longer being opened")
        );
        assert_eq!(opened.try_recv().unwrap(), b"");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The keys and values of an `info` answer, one pair to a line.
    fn info_lines(answer: &str) -> Vec<(String, String)> {
        (answer.lines().filter(|line| !line.is_empty()))
            .map(|line| line.split_once('\t').unwrap())
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    /// `info`'s record of a file with EXIF, keywords, a comment, alt text, marks and stars, in
    /// its frozen form: every key in its order, a ";" in a keyword as "\;", and a backslash,
    /// line break or tab in a value as "\\", "\n" or "\t". The values that change with each
    /// run (the path, the size and the time) are read from the file.
    #[test]
    fn info_prints_a_known_file_in_its_frozen_form() {
        use crate::metadata::xmp;
        let dir = std::env::temp_dir().join(format!("glimt-test-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let exif = crate::support::test_support::CANON_EXIF;
        crate::support::test_support::png_with_raw_profile(
            &file,
            "zTXt",
            "Raw profile type exif",
            exif,
        );
        let added = vec!["beach".to_owned(), "a;b".to_owned()];
        let keywords = xmp::KeywordChange {
            added,
            removed: Vec::new(),
        };
        let comment = "line one\nline\ttwo".to_owned();
        xmp::write_changed(&file, Some(keywords), Some(comment)).unwrap();
        let alt_text = xmp::AltText {
            text: "A red \\ square".into(),
            extended_description: String::new(),
        };
        xmp::write_alt_text(&file, &alt_text).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH);
        let modified = crate::support::format::utc_time(modified.unwrap().as_secs() as i64);
        let expected = format!(
            "path\t{}\nsize\t{}\nmodified\t{modified}\nwidth\t8\nheight\t4\nformat\tpng\n\
             orientation\t6\nstars\t3\nmarks\t1,4\nkeywords\tbeach; a\\;b\n\
             comment\tline one\\nline\\ttwo\nalt_text\tA red \\\\ square\nextended_description\t\n\
             exif.camera\tCanon\nexif.lens\t\nexif.taken\t2020:01:02 03:04:05\n\
             exif.exposure\t\nexif.aperture\t\nexif.iso\t\nexif.focal_length\t\n\
             exif.exposure_bias\t\nexif.flash\t\nexif.gps\t\n",
            file.display(),
            meta.len()
        );
        let record = super::info_record(&file, Some((0b1001, 3)));
        assert_eq!(String::from_utf8(record).unwrap(), expected);
        // Without marks built in, the stars and marks are empty, and the keys stay.
        let without_marks = String::from_utf8(super::info_record(&file, None)).unwrap();
        assert!(
            without_marks.contains("\nstars\t\nmarks\t\n"),
            "{without_marks}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file without metadata, and a path that is no file, have every key, in the same order
    /// as a file with all of them, and an empty value where there is nothing to tell.
    #[test]
    fn info_of_a_file_without_metadata_has_every_key_empty() {
        let dir = std::env::temp_dir().join(format!("glimt-test-info-bare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bare.png");
        let mut png = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png);
        image::RgbImage::new(4, 2)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&file, &png).unwrap();
        let record = super::info_record(&file, Some((0, 0)));
        let lines = info_lines(&String::from_utf8(record).unwrap());
        let keys: Vec<String> = (super::INFO_KEYS.iter().map(|k| k.to_string()))
            .chain(crate::metadata::exif::LABELS.map(super::info_exif_key))
            .collect();
        assert_eq!(lines.iter().map(|l| l.0.clone()).collect::<Vec<_>>(), keys);
        let filled: Vec<(&str, &str)> = (lines.iter())
            .filter(|l| !l.1.is_empty() && !["path", "size", "modified"].contains(&&*l.0))
            .map(|l| (l.0.as_str(), l.1.as_str()))
            .collect();
        let expected = [
            ("width", "4"),
            ("height", "2"),
            ("format", "png"),
            ("stars", "0"),
        ];
        assert_eq!(filled, expected);
        let record = super::info_record(&dir.join("no.png"), Some((1, 2)));
        let missing = info_lines(&String::from_utf8(record).unwrap());
        assert_eq!(
            missing.iter().map(|l| l.0.clone()).collect::<Vec<_>>(),
            keys
        );
        assert!(
            missing.iter().skip(1).all(|l| l.1.is_empty()),
            "{missing:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A value's backslash, tab and line breaks are written so that a script can undo them, a
    /// ";" too in a keyword, and a path that is not UTF-8 keeps its bytes, as `current` gives
    /// them.
    #[test]
    fn info_escapes_what_a_script_can_undo() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            super::escaped(b"a\\b\tc\nd\re;f", b""),
            b"a\\\\b\\tc\\nd\\re;f"
        );
        assert_eq!(super::escaped(b"a;b\\", b";"), b"a\\;b\\\\");
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/no/\xff\tx\\.png"));
        let record = super::info_record(path, None);
        assert!(record.starts_with(b"path\t/no/\xff\\tx\\\\.png\nsize\t\n"));
    }

    /// `info`'s records go to the client as the window sends them, each at once, so the
    /// client's time limit holds for each file. The client counts the records it got, and an
    /// answer cut short, as by a window that closed, is not done.
    #[test]
    fn info_is_sent_a_record_at_a_time() {
        use std::io::{Read, Write};
        let (mut client, window) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = eframe::egui::Context::default();
        std::thread::spawn(move || super::serve(window, &tx, &ctx, super::WAIT));
        writeln!(client, "info /a.png\0/b.png").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let (command, reply) = rx.recv().unwrap();
        let reply = reply.unwrap();
        reply.send(b"path\t/a.png\n\n".to_vec()).unwrap();
        let mut first = [0; 13];
        client.read_exact(&mut first).unwrap();
        assert_eq!(super::info_records_missing(&command, &first), 1);
        reply.send(b"path\t/b.png\n\n".to_vec()).unwrap();
        drop(reply);
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"path\t/b.png\n\n");
        assert_eq!(
            super::info_records_missing(&command, &[&first[..], &rest].concat()),
            0
        );
        assert_eq!(super::info_records_missing("info", b""), 1);
        assert_eq!(super::info_records_missing("next", b""), 0);
    }

    /// `info` through the listener: about the image shown, about files named (listed or not)
    /// with a blank line between them, and "not done" when no image is shown. The client takes
    /// the word alone or with files.
    #[test]
    fn info_is_answered_through_the_socket() {
        use std::io::{Read, Write};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-info-serve-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let mut png = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png);
        image::RgbImage::new(4, 2)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        std::fs::write(other.join("b.png"), &png).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list.set_files(vec!["a.png".into()]);
        let shown = app.path(0);
        #[cfg(feature = "marks")]
        app.marks.rate(std::slice::from_ref(&shown), 2);
        let (mut client, window) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let window_ctx = ctx.clone();
        std::thread::spawn(move || super::serve(window, &tx, &window_ctx, super::WAIT));
        let named = format!("{}\0{}", shown.display(), other.join("b.png").display());
        writeln!(client, "info").unwrap();
        writeln!(client, "info {named}").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        for _ in 0..2 {
            let (line, reply) = rx.recv().unwrap();
            app.handle_remote(&ctx, &line, reply);
        }
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        let got = String::from_utf8(got).unwrap();
        let records: Vec<&str> = got.split_terminator("\n\n").collect();
        assert_eq!(records.len(), 3, "{got}");
        let value = |record: &str, key: &str| {
            let lines = info_lines(record);
            lines.into_iter().find(|l| l.0 == key).map(|l| l.1)
        };
        let stars = if cfg!(feature = "marks") { "2" } else { "" };
        for record in &records[..2] {
            assert_eq!(value(record, "path"), Some(shown.display().to_string()));
            assert_eq!(value(record, "stars").as_deref(), Some(stars));
            assert_eq!(value(record, "width").as_deref(), Some("4"));
        }
        let unlisted = other.join("b.png").display().to_string();
        assert_eq!(value(records[2], "path"), Some(unlisted));
        assert_eq!(super::not_done_reason(got.as_bytes()), None);
        // With no image shown, `info` alone is not done, and is told so.
        app.list.set_files(Vec::new());
        let (reply, answer) = std::sync::mpsc::channel();
        app.handle_remote(&ctx, "info", Some(reply));
        let answer = answer
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(super::not_done_reason(&answer).is_some(), "{answer:?}");
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(super::is_known_command(&words("info")));
        assert!(super::is_known_command(&words("info /a.jpg /b.jpg")));
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

    /// The window takes every command of `COMMANDS`, with each of its choices and with and
    /// without an optional argument, so no command is listed in `glimt --help` and taken only
    /// by the client. `name` is sent only without one, as a name would start to listen on a
    /// socket. Where a command's effect is cheap to see, the test checks it too: a query
    /// answers, `next` moves, `run Next` is done and `title` sets the title.
    #[test]
    fn the_window_takes_every_listed_command() {
        let dir = std::env::temp_dir().join(format!("glimt-test-commands-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"").unwrap();
        std::fs::write(dir.join("b.png"), b"").unwrap();
        let (mut app, ctx) = app_on(&dir);
        let file = dir.join("a.png").display().to_string();
        for (usage, _) in super::COMMANDS {
            let mut parts = usage.split(' ');
            let word = parts.next().unwrap();
            let sent: Vec<String> = match parts.next() {
                None => vec![word.to_owned()],
                Some(argument) if argument.starts_with('[') && !argument.contains('|') => {
                    let given = match argument {
                        "[NAME]" => None,
                        "[TEXT…]" => Some("Holiday".to_owned()),
                        _ => Some(file.clone()),
                    };
                    let with = given.map(|given| format!("{word} {given}"));
                    std::iter::once(word.to_owned()).chain(with).collect()
                }
                Some(choices) if choices.contains('|') => {
                    let each = choices.trim_matches(['[', ']']).split('|');
                    let alone = choices.starts_with('[').then(|| word.to_owned());
                    alone
                        .into_iter()
                        .chain(each.map(|c| format!("{word} {c}")))
                        .collect()
                }
                Some("ACTION") => vec![format!("{word} Next")],
                Some(_) => vec![format!("{word} {file}")],
            };
            for command in sent {
                let shown_before = app.list.shown();
                let (reply, answer) = std::sync::mpsc::channel();
                app.handle_remote(&ctx, &command, Some(reply));
                // A name is taken in the background, and answered when the window takes it.
                let answer = (0..1000)
                    .find_map(|_| {
                        app.poll_background(&ctx);
                        let wait = std::time::Duration::from_millis(10);
                        answer.recv_timeout(wait).ok()
                    })
                    .unwrap();
                let answer = String::from_utf8_lossy(&answer);
                assert!(!answer.contains("does not know"), "{command}: {answer}");
                match command.as_str() {
                    "current" | "selection" | "info" => assert!(!answer.is_empty(), "{command}"),
                    "next" => assert_ne!(app.list.shown(), shown_before, "{command}"),
                    "run Next" => assert!(answer.is_empty(), "{command}: {answer}"),
                    "title Holiday" => assert_eq!(app.title_text(), "Holiday"),
                    _ => {}
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
