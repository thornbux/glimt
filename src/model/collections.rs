//! Collections, as in Geeqie: named lists of images from any folders. Each is a Geeqie `.gqv`
//! file, one quoted path per line, kept in `$XDG_DATA_HOME/glimt/collections`; Geeqie's own
//! collections can be opened too.

use crate::files::config::fits_on_a_line;
use crate::model::listing::real_dir;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, lock_even_if_poisoned};
use crate::ui::actions::Action;
use crate::ui::menus::{icon_button, menu_item, scroll_menu, submenu, with_icon};
use crate::{tf, ui::theme};
use eframe::egui;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Plain lists of images, as a script or a music player writes them: one path on each line,
/// relative to the list or absolute, `#` for comments. Opened as a collection, never written.
const LIST_EXTS: &[&str] = &["txt", "m3u", "m3u8"];

/// This window's own changes to each plain list it has shown, by the list's file. Glimt never
/// writes a list, so what the window took out, moved or renamed in it is kept here, and laid
/// over what the file says each time it is read (`load_with_window_changes`). The changes are
/// made here by `edit_open_collection`, the one way every operation edits the open collection,
/// as they are made to a `.gqv` file.
static WINDOW_LISTS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, WindowList>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The saved collections as the File menu lists them, read in the background while it is open,
/// because the window's thread does not wait for the disk.
#[derive(Default)]
pub(crate) struct SavedCollections {
    /// The collections as last read, or `None` before the first read has answered.
    listed: Option<Vec<(String, PathBuf)>>,
    answer: Receiving<Vec<(String, PathBuf)>>,
    read_at: Option<std::time::Instant>,
}

impl SavedCollections {
    /// Returns the saved collections as last read, or `None` before the first read has
    /// answered. They are read again in the background when they were read more than a second
    /// ago, so a menu kept open shows a collection made meanwhile.
    pub(crate) fn listed(&mut self, ctx: &egui::Context) -> Option<Vec<(String, PathBuf)>> {
        if let Took::Answer(listed) = self.answer.take() {
            self.listed = Some(listed);
        }
        let is_old = (self.read_at).is_none_or(|at| at.elapsed().as_secs() >= 1);
        if self.answer.is_none() && is_old {
            self.read_at = Some(std::time::Instant::now());
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            // No busy pointer: the menu's collections, a look-up nobody waits for, shown when it
            // comes.
            std::thread::spawn(move || {
                let _ = tx.send(saved_collections());
                ctx.request_repaint();
            });
            self.answer = Receiving::of(rx);
        }
        self.listed.clone()
    }
}

/// The collection that images are added to: one saved already, or a new one by its name.
pub(crate) enum CollectionAddedTo {
    Saved(PathBuf),
    New(String),
}

/// A plain list as this window has it, apart from its file, which Glimt never writes. Every
/// entry is named by its real path, as the list and the edits of a collection name it.
struct WindowList {
    /// The entries as the list's file gave them when it was last read.
    read: Vec<PathBuf>,
    /// The entries as this window has them: those read, with the window's own changes made.
    ours: Vec<PathBuf>,
}

impl WindowList {
    /// Returns the entries as this window has them, after the file was read again and gave
    /// `now`. An entry that the file took out since it was last read goes, and one it added
    /// comes after the window's entries. An entry that the window took out stays out, and the
    /// window's order and renames stay. When the window changed nothing, the file's own order
    /// is taken as it is.
    fn read_again(&mut self, now: Vec<PathBuf>) -> Vec<PathBuf> {
        if self.ours == self.read {
            self.ours = now.clone();
        } else {
            let before: HashSet<&PathBuf> = self.read.iter().collect();
            let there: HashSet<&PathBuf> = now.iter().collect();
            let ours: HashSet<PathBuf> = self.ours.iter().cloned().collect();
            self.ours
                .retain(|p| !before.contains(p) || there.contains(p));
            let added = now
                .iter()
                .filter(|p| !before.contains(p) && !ours.contains(*p));
            self.ours.extend(added.cloned());
        }
        self.read = now;
        self.ours.clone()
    }
}

impl crate::App {
    /// The collection shown is a plain list (.txt, .m3u), which Glimt reads but never writes.
    pub(crate) fn collection_is_list(&self) -> bool {
        self.folder.collection().is_some_and(is_list)
    }

    // ---- Collections ----

    /// Makes `change` to the open collection's file as it is then, with what other windows
    /// changed. It is done in the writer thread for the user's folders, because the file and
    /// the folders of its entries are read, and a share that stopped answering would hold the
    /// window. A failure is told as every failed save is.
    pub(crate) fn edit_collection(
        &mut self,
        change: impl FnOnce(&mut Vec<PathBuf>) + Send + 'static,
    ) {
        let Some(file) = self.folder.collection() else {
            return;
        };
        let file = file.to_path_buf();
        crate::files::file_io::write_later_in_user_folders(move || {
            if let Err(e) = edit_open_collection(&file, change) {
                crate::files::file_io::save_failed(e);
            }
        });
    }

    /// The collection's file put in the order of the list, unless shuffled for a slideshow.
    pub(crate) fn save_collection_order(&mut self) {
        if self.slideshow.is_shuffled() {
            return;
        }
        let ours: Vec<PathBuf> = (0..self.list.files().len()).map(|i| self.path(i)).collect();
        self.edit_collection(move |all| reorder(all, &ours));
    }

    /// Move the selected images (or the shown one) of the open collection one place earlier or
    /// later, or to its start or end, and save its new order: a slideshow plays it so. Several
    /// selected keep their order among themselves; the shown image and the selection follow.
    // shortcut: the whole list put together again for each move, some milliseconds for 100,000
    // images; move the indices alone if collections that large are moved image by image.
    pub(crate) fn move_in_collection(
        &mut self,
        ctx: &egui::Context,
        picked: HashSet<usize>,
        order: Vec<usize>,
    ) {
        if self.folder.collection().is_none() {
            self.notice
                .give(tr("Only the images of a collection can be moved").into());
            return;
        }
        // An empty collection: nothing to move, and no shown image to keep.
        if self.list.files().is_empty() {
            return;
        }
        if self.slideshow.is_shuffled() {
            self.notice
                .give(tr("Random order is on: R turns it off to move images").into());
            return;
        }
        // A fuzzy search shows the best matches first, and terms joined by "|" show each term's
        // matches in turn. The rows are then in another order than the collection's, so a move
        // or a line drawn between two rows would land somewhere else in the collection than it
        // shows. This is the one place that keys and drags both reach.
        if !self.search_rows_are_in_list_order() {
            self.notice.give(
                tr("The search shows the images in another order: clear it to move them").into(),
            );
            return;
        }
        let shown = self.list.files().get(self.list.shown()).cloned();
        // The images moved stay selected when they were, and so do the selected images that the
        // search hides. A row dragged from outside the selection takes the selection's place, as a
        // click on it would, so the hidden selected images are let go with the rest. That is so
        // also when only hidden selected images were selected. With nothing selected at all, the
        // moved images stay unselected.
        let selected = self.list.selected();
        let any = !selected.is_empty() || !self.list.hidden_selected().is_empty();
        let replaced = any && !picked.iter().all(|i| selected.contains(i));
        let moved = picked.iter().filter(|_| !selected.is_empty() || replaced);
        let hidden = self.list.hidden_selected().iter().filter(|_| !replaced);
        let names = self.list.names_with_identities(moved.chain(hidden));
        let files = order
            .iter()
            .map(|&i| (self.list.files()[i].clone(), self.list.identity(i)))
            .collect();
        self.replace_files(ctx, files, shown);
        self.reselect(ctx, &names);
        self.save_collection_order();
    }

    /// Returns whether the rows shown are in the order of the list: with no search, or with a
    /// search whose matches keep the list's order (`re:`, `exif:`, `stars:`).
    fn search_rows_are_in_list_order(&self) -> bool {
        (self.list.matches()).is_none_or(|m| m.windows(2).all(|w| w[0] < w[1]))
    }

    /// Moves the collection's file `picked` to the trash, in the background, while it is still
    /// the file the question asked about. The images stay. When the window still shows that
    /// collection, it shows the shown image's folder after.
    pub(crate) fn trash_collection(
        &mut self,
        ctx: &egui::Context,
        picked: crate::files::file_io::PickedFiles,
    ) {
        let Some(file) = picked.paths().first().cloned() else {
            return;
        };
        let shown = self.folder.collection() == Some(file.as_path());
        if !self.run_in_background(ctx, vec![file.clone()], move || {
            let name = file
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let (_, picked) = picked.wait_for_fingerprints();
            let trashed = picked
                .still_the_one_picked(&file)
                .map_err(|e| e.to_string())
                .and_then(|()| crate::files::trash::trash(std::slice::from_ref(&file)));
            Some(match trashed {
                Ok(()) => crate::support::threads::Told::Notice(tf!(
                    "Collection {name} moved to the trash",
                    name
                )),
                Err(err) => crate::support::threads::Told::FailedWrite(tf!(
                    "Could not trash {name}: {err}",
                    name,
                    err
                )),
            })
        }) || !shown
        {
            return;
        }
        let folder = (!self.list.files().is_empty())
            .then(|| self.path(self.list.shown()))
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(folder) = folder.or_else(std::env::home_dir) {
            self.visit(ctx, folder);
        }
    }

    /// Add the selected files, or the shown image, to the collection `added_to`. It is written
    /// in the background, because Select All in a large folder adds hundreds of thousands of
    /// files, and a home folder on a share may be slow to write. A new collection's name is
    /// looked up there too, and why there can be none of that name is told. A file whose name
    /// has a line break is left out with a notice, because the collection keeps a name on a
    /// line.
    pub(crate) fn add_to_collection(&mut self, ctx: &egui::Context, added_to: CollectionAddedTo) {
        let Some(targets) = self.targets() else {
            return;
        };
        let mut paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let left_out = crate::files::config::keep_those_that_fit_on_a_line(&mut paths);
        self.run_in_background(ctx, Vec::new(), move || {
            let file = match added_to {
                CollectionAddedTo::Saved(file) => file,
                CollectionAddedTo::New(name) => match new_file(&name) {
                    Ok(file) => file,
                    Err(why) => return Some(crate::support::threads::Told::FailedWrite(why)),
                },
            };
            let name = file.file_stem().unwrap_or_default().to_string_lossy();
            let (told, failed) = match append(&file, &paths) {
                Ok(n) => (
                    tf!("{n} added to the collection \"{name}\"", n, name),
                    false,
                ),
                Err(e) => (tf!("Could not save the collection: {e}", e), true),
            };
            let told = match left_out {
                Some(left_out) => format!("{told}\n{left_out}"),
                None => told,
            };
            Some(match failed {
                true => crate::support::threads::Told::FailedWrite(told),
                false => crate::support::threads::Told::Notice(told),
            })
        });
    }

    /// File menu entries for collections; the ones listing collections read the folder only
    /// while open.
    pub(crate) fn collections_menu(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::NewCollection);
        menu_item(ui, chosen, Action::OpenCollection);
        menu_item(ui, chosen, Action::ImportLightroom);
        let mut open = None;
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::STACK, tr("Collections")),
            |ui| {
                let all = self.folder.saved_collections().listed(ui.ctx());
                if all.as_ref().is_some_and(Vec::is_empty) {
                    ui.label(tr("No collections yet"));
                }
                for (name, file) in all.into_iter().flatten() {
                    if icon_button(ui, theme::icon::STACK, &name).clicked() {
                        open = Some(file);
                    }
                }
            },
        );
        if !self.list.files().is_empty() {
            scroll_menu(
                ui,
                with_icon(ui, theme::icon::STACK_PLUS, tr("Add to Collection")),
                |ui| {
                    let all = self.folder.saved_collections().listed(ui.ctx());
                    if all.as_ref().is_some_and(Vec::is_empty) {
                        ui.label(tr("No collections yet"));
                    }
                    for (name, file) in all.into_iter().flatten() {
                        if icon_button(ui, theme::icon::STACK, &name).clicked() {
                            self.add_to_collection(ui.ctx(), CollectionAddedTo::Saved(file));
                        }
                    }
                },
            );
        }
        if let Some(file) = self.folder.collection() {
            menu_item(ui, chosen, Action::RemoveFromCollection);
            // A plain list is the user's own file: Glimt never writes it.
            if !is_list(file) {
                menu_item(ui, chosen, Action::RenameCollection);
                menu_item(ui, chosen, Action::TrashCollection);
            }
            submenu(
                ui,
                with_icon(ui, theme::icon::MOVE_IN, tr("Move in Collection")),
                |ui| {
                    for action in [
                        Action::MoveEarlier,
                        Action::MoveLater,
                        Action::MoveFirst,
                        Action::MoveLast,
                    ] {
                        menu_item(ui, chosen, action);
                    }
                },
            );
        }
        if let Some(file) = open {
            self.visit(ui.ctx(), file);
        }
    }

    /// Collection → Remove: the targets out of the collection's file at once; the files stay where
    /// they are.
    pub(crate) fn remove_from_collection(&mut self, ctx: &egui::Context) {
        let Some(targets) = self.targets() else {
            return;
        };
        // Out of the collection's file here, at once: the files stay where they are.
        let out: HashSet<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        self.edit_collection(move |all| all.retain(|p| !out.contains(p)));
        self.remove_indices(ctx, &targets);
    }
}

/// Where new collections go.
pub fn collections_dir() -> Option<PathBuf> {
    crate::files::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt")
        .map(|d| d.join("collections"))
}

/// A `.gqv` collection, or a plain list: what opens as a list of images from any folders.
pub fn is_collection(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("gqv") || LIST_EXTS.iter().any(|l| e.eq_ignore_ascii_case(l))
    }) && crate::files::file_io::is_surely_file(path)
}

/// Whether `file` is a plain list, which Glimt shows but never writes: a change in the window
/// (an image taken out, another order) stays there.
pub fn is_list(file: &Path) -> bool {
    (file.extension()).is_some_and(|e| LIST_EXTS.iter().any(|l| e.eq_ignore_ascii_case(l)))
}

/// The saved collections: (name, file), by name.
pub fn saved_collections() -> Vec<(String, PathBuf)> {
    let entries = collections_dir().and_then(|d| std::fs::read_dir(d).ok());
    let mut all: Vec<(String, PathBuf)> = (entries.into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gqv"))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    all.sort();
    all
}

/// Makes the entries of every saved collection follow the files or folders moved `(from, to)`,
/// as Geeqie's collection manager does, so an entry never stays on a name that another file
/// takes later. The `open` collection is left out, because the window has changed it already,
/// and a second pass would move a swapped entry back. It runs in the writer thread for the
/// user's folders, because it reads every saved collection and the folders of their entries.
/// A collection that holds none of the files is not written.
// shortcut: each move reads every saved collection whole; keep an index of their entries if
// someone keeps thousands of large collections.
pub(crate) fn follow_in_saved_collections(pairs: &[(PathBuf, PathBuf)], open: Option<&Path>) {
    let (pairs, open) = (pairs.to_vec(), open.map(Path::to_path_buf));
    crate::files::file_io::write_later_in_user_folders(move || {
        use crate::files::file_ops::{moves_by_source, new_place};
        let moved = moves_by_source(&pairs);
        let is_open = |file: &Path| {
            (open.as_deref()).is_some_and(|o| crate::images::save::same_file(o, file))
        };
        for (_, file) in saved_collections().into_iter().filter(|(_, f)| !is_open(f)) {
            let follow = |all: &mut Vec<PathBuf>| {
                for p in all {
                    if let Some(to) = new_place(p, &moved) {
                        *p = to;
                    }
                }
            };
            // A collection removed since the folder was read has no entries left to follow.
            match edit(&file, false, follow) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    crate::files::file_io::save_failed(tf!(
                        "Could not save {name}: {e}",
                        name = file.display(),
                        e
                    ));
                }
                _ => {}
            }
        }
    });
}

/// `name` trimmed, if it can be the name of a collection's file.
fn valid_collection_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    match crate::model::listing::refused_name(name, false).is_some() {
        true => Err(tf!("Not a valid collection name: \"{name}\"", name)),
        false => Ok(name),
    }
}

/// The file of a new collection called `name`, or why there can be none.
pub fn new_file(name: &str) -> Result<PathBuf, String> {
    let name = valid_collection_name(name)?;
    let file = collections_dir()
        .ok_or(tr("no data folder ($HOME is not set)"))?
        .join(format!("{name}.gqv"));
    match !crate::files::file_io::is_surely_gone(&file) {
        true => Err(tf!("There is a collection called \"{name}\" already", name)),
        false => Ok(file),
    }
}

/// Give the collection in `file` the name `name`, in the same folder, when it is still the one
/// `picked`. Returns the new file.
pub fn rename(
    file: &Path,
    name: &str,
    picked: &crate::files::file_io::PickedFingerprints,
) -> Result<PathBuf, String> {
    let name = valid_collection_name(name)?;
    // Under the collection's lock, as every edit of it: another window's edit lands before or
    // after the rename, never in between, where it would make the old name again.
    #[allow(clippy::disallowed_methods)] // a collection's own lock, as edit
    let _held = crate::files::file_io::file_lock(file);
    // Another program may have put another file at the name while the question was open.
    (picked.still_the_one_picked(file))
        .map_err(|e| tf!("Could not rename the collection: {e}", e))?;
    let new = file.with_file_name(format!("{name}.gqv"));
    // Never over one of that name, also one another window makes between the look and the
    // rename.
    match crate::files::file_io::rename_never_over(file, &new) {
        Ok(()) => Ok(new),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(tf!("There is a collection called \"{name}\" already", name))
        }
        Err(e) => Err(tf!("Could not rename the collection: {e}", e)),
    }
}

/// Makes `change` to the plain list `file` as this window has it. A list the window has not
/// read yet is read first. Returns the error of that read.
fn edit_window_list(file: &Path, change: impl FnOnce(&mut Vec<PathBuf>)) -> std::io::Result<()> {
    let is_known = lock_even_if_poisoned(&WINDOW_LISTS).contains_key(file);
    // The file is read without the lock, which is never held while the disk is waited for.
    let read = match is_known {
        true => None,
        false => Some(entries_by_real_path(file)?),
    };
    let mut lists = lock_even_if_poisoned(&WINDOW_LISTS);
    let list = lists.entry(file.to_path_buf()).or_insert_with(|| {
        let read = read.unwrap_or_default();
        WindowList {
            ours: read.clone(),
            read,
        }
    });
    change(&mut list.ours);
    Ok(())
}

/// Returns the entries of a collection for a listing. A `.gqv` is read as its file says, as
/// every change the window makes is written to it. A plain list is read as its file says now,
/// with this window's own changes laid over it (`WindowList::read_again`), by real path. So
/// F5 and Auto Refresh show what a script or a music player wrote to the list since, and an
/// entry the window took out stays out. Before, a list was never read again after it opened.
pub fn load_with_window_changes(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    if !is_list(file) {
        return load(file);
    }
    let now = entries_by_real_path(file)?;
    let mut lists = lock_even_if_poisoned(&WINDOW_LISTS);
    let list = lists
        .entry(file.to_path_buf())
        .or_insert_with(|| WindowList {
            read: now.clone(),
            ours: now.clone(),
        });
    Ok(list.read_again(now))
}

/// Starts this window's own changes to the plain list `file` anew from its `entries` as just
/// read, because it is opened again: the changes made when it was shown before are dropped, as
/// File → Open shows the list as its file has it. Nothing happens for a `.gqv`.
pub fn start_window_list(file: &Path, entries: &[PathBuf]) {
    if is_list(file) {
        let list = WindowList {
            read: entries.to_vec(),
            ours: entries.to_vec(),
        };
        lock_even_if_poisoned(&WINDOW_LISTS).insert(file.to_path_buf(), list);
    }
}

/// Returns the entries of the collection in `file`, each by its real path (`by_real_path`).
fn entries_by_real_path(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = load(file)?;
    Ok((by_real_path(&entries).into_iter())
        .map(|(real, _)| real)
        .collect())
}

pub fn load(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    let text = crate::images::decode::read_limited(file)?;
    // A list's entries relative to it start from its folder as an absolute path: `glimt
    // show.m3u` in its own folder gave "" as that folder, and every entry was left out.
    let file = std::path::absolute(file)?;
    Ok(match is_list(&file) {
        true => parse_list(&text, file.parent().unwrap_or(Path::new("/"))),
        false => parse(&text),
    })
}

/// The paths of a plain list, those relative to it joined to `base`, its folder; each once.
/// Lines may be quoted, end in CR, or be file:// URIs, as players write them; a URI of another
/// computer is left out.
fn parse_list(text: &[u8], base: &Path) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    (text.split(|&b| b == b'\n'))
        .map(|line| line.trim_ascii())
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|line| match line {
            [b'"', inner @ .., b'"'] => inner,
            line => line,
        })
        .filter_map(|line| match line.strip_prefix(b"file://") {
            Some(uri) => {
                let uri = uri.strip_prefix(b"localhost").unwrap_or(uri);
                let path = PathBuf::from(OsStr::from_bytes(&percent_decoded(uri)));
                path.is_absolute().then_some(path)
            }
            None => Some(lexically_normal(&base.join(OsStr::from_bytes(line)))),
        })
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// `path` with its `.` and `..` worked out as written, without looking at the disk: the same
/// image named "../Pictures/a.jpg" in a list is "/home/u/Pictures/a.jpg", the key its stars are
/// kept by when it is shown from its folder.
fn lexically_normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => drop(out.pop()),
            part => out.push(part),
        }
    }
    out
}

/// A URI's path with its %XX escapes undone.
pub fn percent_decoded(uri: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(uri.len());
    let mut i = 0;
    while i < uri.len() {
        // Two hex digits: from_str_radix alone takes "+4" too.
        let hex = (uri.get(i + 1..i + 3))
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok());
        match (uri[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Makes `change` to the open collection in `file`, and words what went wrong for the user. A
/// plain list (.m3u, .txt) is the user's own and is never written, so the change is made to
/// the list as this window has it (`WINDOW_LISTS`), which each listing lays over the file. A
/// file renamed or moved to the trash in another window since it was opened is not made again
/// under its old name. This is the one way an operation edits the open collection.
pub(crate) fn edit_open_collection(
    file: &Path,
    change: impl FnOnce(&mut Vec<PathBuf>),
) -> Result<(), String> {
    if is_list(file) {
        return edit_window_list(file, change)
            .map_err(|e| tf!("Could not save the collection: {e}", e));
    }
    edit(file, false, change).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => tf!(
            "The collection {file} is gone (renamed or removed in another window): not saved",
            file = file.display()
        ),
        _ => tf!("Could not save the collection: {e}", e),
    })
}

/// Make `change` to the collection in `file` as it is on disk now, under its lock: each window
/// writes only its own change (images taken out, renamed, put in order), never its whole list,
/// so what other windows changed meanwhile stays. With `new` a file not there yet starts empty;
/// else that is an error, so a collection renamed or trashed elsewhere is not made again. One
/// that cannot be read is never written over: what it holds would be lost.
pub fn edit(file: &Path, new: bool, change: impl FnOnce(&mut Vec<PathBuf>)) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // edit_shared cannot refuse to make one that went
    let _held = crate::files::file_io::file_lock(file);
    let (text, was_new) = match crate::images::decode::read_limited(file) {
        Err(e) if new && e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), true),
        read => (read?, false),
    };
    // The change sees each entry by its real path, the name the list and every operation give
    // the file. An entry named through a link was else never found: Remove left it in the file,
    // and a reorder put it last. An entry the change leaves is written as the user spelled it.
    let written = parse(&text);
    let (mut all, spelled): (Vec<PathBuf>, std::collections::HashMap<PathBuf, &PathBuf>) =
        (by_real_path(&written).into_iter())
            .map(|(real, w)| (real.clone(), (real, w)))
            .unzip();
    let before = all.clone();
    change(&mut all);
    // Nothing changed (an unrelated folder renamed): the file stays as it is.
    if all == before {
        return Ok(());
    }
    let mut all: Vec<PathBuf> = (all.into_iter())
        .map(|p| spelled.get(&p).map_or(p, |&w| w.clone()))
        .collect();
    // A name that a line cannot hold is refused here, whatever the change: an add, an entry
    // that follows a move, or a new order. The entry is checked as it is written, so an entry
    // through a link to a folder whose name holds a line break is kept.
    if let Some(left_out) = crate::files::config::keep_those_that_fit_on_a_line(&mut all) {
        crate::files::file_io::save_failed(left_out);
    }
    // Written whole or not at all; a new one never lands over a collection another window made
    // meanwhile. What else the file held (Geeqie's #geometry line) goes back in.
    let out = to_text(&all, &non_entry_lines(&text));
    // A collection opened from a downloaded folder may be a link to another file of the user's.
    // The link itself is replaced, so the file it points to is never written over.
    match was_new {
        true => crate::files::file_io::create_file(file, out),
        false => crate::files::file_io::replace_name(file, out),
    }
}

/// Gives each of a collection's `entries` its real path (`listing::real_path`), the name the
/// list and the marks, stars and `--remote select` know the file by, with the entry as written.
/// An image named twice, also through a link, is given once. Each folder is looked up once, so a
/// collection of 500,000 images in a few folders costs a few lookups. Each lookup waits for the
/// disk, so this runs only in a background thread: the look-up when a collection opens, the
/// listing, the writer thread for an edit, and the read of the saved order.
pub fn by_real_path(entries: &[PathBuf]) -> Vec<(PathBuf, &PathBuf)> {
    let mut folders: std::collections::HashMap<&Path, PathBuf> = Default::default();
    let mut seen = HashSet::new();
    (entries.iter())
        .map(|p| match (p.parent(), p.file_name()) {
            (Some(dir), Some(name)) => {
                let real = folders.entry(dir).or_insert_with(|| real_dir(dir));
                (real.join(name), p)
            }
            _ => (p.clone(), p),
        })
        .filter(|(real, _)| seen.insert(real.clone()))
        .collect()
}

/// The lines of a collection that are not Glimt's to write: neither the header and the end
/// marker it writes itself, nor an image's path, which it writes from its list. Geeqie's own
/// lines (#geometry, its comments) stay as they were when Glimt edits one of its collections.
/// Only lines starting with `#` are kept: a collection that is a link to another file of the
/// user's would else copy that file's text into the collection written in the link's place.
fn non_entry_lines(text: &[u8]) -> Vec<&[u8]> {
    let ours = |line: &[u8]| {
        line.is_empty()
            || line.starts_with(b"#GQview collection")
            || line.starts_with(b"#created with ")
            || line == b"#end"
            || line.starts_with(b"\"/")
    };
    text.split(|&b| b == b'\n')
        .filter(|line| line.starts_with(b"#") && !ours(line))
        .collect()
}

/// Add `paths` to `all`, leaving out those it has; how many were added.
pub fn add(all: &mut Vec<PathBuf>, paths: &[PathBuf]) -> usize {
    // A set, as Select All in a large folder adds hundreds of thousands at once.
    let mut have: HashSet<PathBuf> = all.iter().cloned().collect();
    let before = all.len();
    all.extend(paths.iter().filter(|p| have.insert((*p).clone())).cloned());
    all.len() - before
}

/// Puts the entries of `all` that `ours`, a window's list, holds in the order of `ours`, in the
/// places those entries had. An entry the window does not list keeps its place: one on a disk
/// that is not mounted, a file gone for a moment, or one added elsewhere since. Before, each
/// such entry was put after all the others, so a move of one image sent an entry the user
/// placed first, and never moved, to the end of the file. An entry only in `ours` (taken out
/// elsewhere) is left out.
pub fn reorder(all: &mut [PathBuf], ours: &[PathBuf]) {
    let placed: HashSet<&PathBuf> = ours.iter().collect();
    let there: HashSet<&PathBuf> = all.iter().filter(|p| placed.contains(p)).collect();
    let in_order: Vec<PathBuf> = ours.iter().filter(|p| there.contains(p)).cloned().collect();
    let mut in_order = in_order.into_iter();
    for slot in all.iter_mut().filter(|p| placed.contains(&**p)) {
        if let Some(path) = in_order.next() {
            *slot = path;
        }
    }
}

/// `files` in the order of `saved`, a collection's file: shuffled ones put back, those it lacks
/// (added since) after them, in the order they had.
pub fn in_saved_order(saved: &[PathBuf], files: Vec<PathBuf>) -> Vec<PathBuf> {
    let have: HashSet<&PathBuf> = files.iter().collect();
    let mut ordered: Vec<PathBuf> = (saved.iter())
        .filter(|p| have.contains(p))
        .cloned()
        .collect();
    let placed: HashSet<PathBuf> = ordered.iter().cloned().collect();
    ordered.extend(files.into_iter().filter(|p| !placed.contains(p)));
    ordered
}

/// Add `paths` to the collection in `file` (made if new), leaving out those it has.
pub fn append(file: &Path, paths: &[PathBuf]) -> std::io::Result<usize> {
    // The first collection of a new install: its folder is not there yet.
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut added = 0;
    edit(file, true, |all| added = add(all, paths))?;
    Ok(added)
}

/// Geeqie's format: comment lines start with #, a path is in double quotes.
fn parse(text: &[u8]) -> Vec<PathBuf> {
    // A file listed twice (in a .gqv edited by hand) is one image: shown, deleted and found
    // among duplicates once.
    let mut seen = std::collections::HashSet::new();
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let rest = line.strip_prefix(b"\"")?;
            let end = rest.iter().rposition(|&b| b == b'"')?;
            Some(PathBuf::from(OsStr::from_bytes(&rest[..end])))
        })
        .filter(|p| p.is_absolute() && seen.insert(p.clone()))
        .collect()
}

fn to_text(paths: &[PathBuf], other: &[&[u8]]) -> Vec<u8> {
    // Geeqie still writes the marker of GQview, which it came from.
    let head = format!(
        "#GQview collection\n#created with Glimt version {}\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut text = head.into_bytes();
    for line in other {
        text.extend(*line);
        text.push(b'\n');
    }
    for path in paths {
        if fits_on_a_line(path) {
            text.push(b'"');
            text.extend(path.as_os_str().as_bytes());
            text.extend(b"\"\n");
        }
    }
    text.extend(b"#end\n");
    text
}

/// Returns the new order of a list of `n` after `action` moves the `picked` places among the
/// `shown` ones, the rows as the user sees them, in list order. Each picked image moves one row
/// earlier or later, passing one shown image that is not picked and never another picked one,
/// or all go to the start or end of the rows, among themselves in the order they had. Before,
/// the moves were worked out over the whole list, so under a search an image passed one the
/// search hid, and a key press showed no change.
pub(crate) fn moved(
    n: usize,
    shown: &[usize],
    picked: &HashSet<usize>,
    action: Action,
) -> Vec<usize> {
    let mut rows = shown.to_vec();
    let (first, rest): (Vec<usize>, Vec<usize>) = rows.iter().partition(|i| picked.contains(i));
    let count = rows.len();
    match action {
        Action::MoveFirst => rows = [first, rest].concat(),
        Action::MoveLast => rows = [rest, first].concat(),
        Action::MoveEarlier => {
            for at in 1..count {
                if picked.contains(&rows[at]) && !picked.contains(&rows[at - 1]) {
                    rows.swap(at - 1, at);
                }
            }
        }
        Action::MoveLater => {
            for at in (0..count.saturating_sub(1)).rev() {
                if picked.contains(&rows[at]) && !picked.contains(&rows[at + 1]) {
                    rows.swap(at, at + 1);
                }
            }
        }
        _ => {}
    }
    // The shown images take the places that shown images had, in their new order. An image the
    // search hides stays where it is.
    let mut order: Vec<usize> = (0..n).collect();
    for (&slot, file) in shown.iter().zip(rows) {
        order[slot] = file;
    }
    order
}

/// The new order of a list of `n` after the `picked` places are dragged before place `to` (`n`:
/// to the end), among themselves in the order they had.
pub(crate) fn dropped(n: usize, picked: &HashSet<usize>, to: usize) -> Vec<usize> {
    let (first, mut rest): (Vec<usize>, Vec<usize>) = (0..n).partition(|i| picked.contains(i));
    let at = rest.iter().take_while(|&&i| i < to).count();
    rest.splice(at..at, first);
    rest
}

#[cfg(test)]
mod tests {
    /// Move Collection to Trash, answered after another file took the collection file's name,
    /// leaves that file where it is.
    #[test]
    fn a_collection_trashed_after_its_question_leaves_another_file_alone() {
        use crate::support::test_support::{app_on, run_background_until};
        let dir = std::env::temp_dir().join(format!("glimt-test-trash-gqv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gqv = dir.join("trip.gqv");
        std::fs::write(&gqv, "#GQview collection\n").unwrap();
        let picked = (
            gqv.clone(),
            crate::files::file_io::fingerprint(&gqv).unwrap(),
        );
        let picked = crate::files::file_io::PickedFiles::recorded(vec![picked]);
        std::fs::remove_file(&gqv).unwrap();
        std::fs::write(&gqv, "#GQview collection\n\"/another\"\n").unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.trash_collection(&ctx, picked);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert!(std::fs::read_to_string(&gqv).unwrap().contains("another"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection's row dragged from outside the selection is selected alone afterwards, as a
    /// click on it would leave it. The selected images that the search hid stayed selected with it,
    /// though the rest of the selection was let go. When only hidden selected images were selected,
    /// they stayed, and the dragged row was not selected.
    #[test]
    fn a_row_dragged_from_outside_the_selection_lets_the_hidden_selected_go() {
        use std::collections::{BTreeSet, HashSet};
        let d = crate::support::test_support::contract_folder("drag-held");
        image::RgbImage::new(2, 2).save(d.join("c.png")).unwrap();
        let gqv = (["a", "b", "c"].iter())
            .map(|n| format!("\"{}\"\n", d.join(format!("{n}.png")).display()))
            .collect::<String>();
        for first in [BTreeSet::from([0, 1]), BTreeSet::from([0])] {
            // The move of the round before is saved in the writer thread, before the file is
            // written anew here.
            crate::files::file_io::wait_for_queued_writes();
            std::fs::write(d.join("col.gqv"), format!("#GQview collection\n{gqv}")).unwrap();
            let (mut app, ctx) = crate::support::test_support::app_on(&d.join("col.gqv"));
            assert_eq!(app.list.files().len(), 3);
            app.list.select(first);
            app.list.set_matches(Some(vec![1, 2]));
            assert_eq!(app.list.hidden_selected(), &BTreeSet::from([0]));
            let picked = HashSet::from([2]);
            let order = super::dropped(3, &picked, 0);
            app.move_in_collection(&ctx, picked, order);
            let names: Vec<_> = (app.list.selected_with_hidden().into_iter())
                .map(|i| app.list.files()[i].clone())
                .collect();
            assert_eq!(names.len(), 1, "{names:?}");
            assert!(
                std::path::Path::new(&names[0]).ends_with("c.png"),
                "{names:?}"
            );
        }
        crate::files::file_io::wait_for_queued_writes();
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Rename Collection renames the collection it asked about, also when a script opened
    /// another one while the dialog was open, and the window then keeps showing the other one.
    #[test]
    fn rename_collection_acts_on_the_one_it_asked_about() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-asked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (trip, home) = (dir.join("trip.gqv"), dir.join("home.gqv"));
        for file in [&trip, &home] {
            std::fs::write(file, "#GQview collection\n#end\n").unwrap();
        }
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, trip.clone());
        app.run(&ctx, crate::ui::actions::Action::RenameCollection);
        crate::support::test_support::open_now(&mut app, &ctx, home.clone());
        app.act_on_prompt_answer(&ctx, crate::ui::dialogs::Prompt::RenameCollection, "summer");
        let summer = dir.join("summer.gqv");
        assert!(crate::support::test_support::run_background_until(
            &mut app,
            &ctx,
            |_| summer.is_file()
        ));
        assert!(!trip.exists() && home.is_file());
        assert_eq!(app.folder.collection(), Some(home.as_path()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename Collection leaves alone a collection that another file has taken the name of
    /// while the question was open. Before, the other file was renamed.
    #[test]
    fn rename_collection_leaves_another_file_at_the_name_alone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let trip = dir.join("trip.gqv");
        std::fs::write(&trip, "#GQview collection\n#end\n").unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, trip.clone());
        app.run(&ctx, crate::ui::actions::Action::RenameCollection);
        app.dialogs.wait_until_folder_taken();
        std::fs::remove_file(&trip).unwrap();
        std::fs::write(&trip, "#GQview collection\n\"/other.png\"\n#end\n").unwrap();
        app.act_on_prompt_answer(&ctx, crate::ui::dialogs::Prompt::RenameCollection, "summer");
        crate::files::file_io::wait_for_queued_writes();
        assert!(trip.is_file() && !dir.join("summer.gqv").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename Collection renames in the background, so a collection's lock that waits never
    /// holds the window, and the window then shows the collection under its new name.
    #[test]
    fn renames_a_collection_in_the_background() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-gqv-renamed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let trip = dir.join("trip.gqv");
        std::fs::write(&trip, "#GQview collection\n#end\n").unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, trip.clone());
        // Another window's edit holds the lock until the window has acted, or for two seconds
        // when the window waits for it. It is let go at once after, because the writer thread
        // that waits for it is shared with the other tests.
        let held = crate::files::file_io::file_lock(&trip).unwrap();
        let (release, released) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = released.recv_timeout(std::time::Duration::from_secs(2));
            drop(held);
        });
        app.run(&ctx, crate::ui::actions::Action::RenameCollection);
        let started = std::time::Instant::now();
        app.act_on_prompt_answer(&ctx, crate::ui::dialogs::Prompt::RenameCollection, "summer");
        let waited = started.elapsed();
        drop(release);
        assert!(
            waited < std::time::Duration::from_secs(1),
            "the window waited"
        );
        let summer = dir.join("summer.gqv");
        assert!(crate::support::test_support::run_background_until(
            &mut app,
            &ctx,
            |app| app.folder.collection() == Some(summer.as_path())
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Add to Collection writes the collection in the background and tells how many it added.
    #[test]
    fn adds_to_a_collection_in_the_background() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        assert!(crate::support::test_support::run_background_until(
            &mut app,
            &ctx,
            |a| !a.folder.is_listing()
        ));
        let file = dir.join("trip.gqv");
        app.add_to_collection(&ctx, super::CollectionAddedTo::Saved(file.clone()));
        let told = |app: &crate::App| app.notice.text().is_some_and(|n| n.contains("trip"));
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, told
        ));
        assert_eq!(super::load(&file).unwrap(), [a]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file whose name has a line break is refused at the add with a notice, and the others
    /// are added. A collection keeps a name on a line, so it was counted as added and then left
    /// out of the file without a word.
    #[test]
    fn a_name_with_a_line_break_is_refused_at_the_add() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-nl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        app.list
            .set_files(["a\nb.png", "c.png"].map(Into::into).to_vec());
        app.list.select([0, 1].into());
        let file = dir.join("trip.gqv");
        app.add_to_collection(&ctx, super::CollectionAddedTo::Saved(file.clone()));
        let told = |app: &crate::App| app.notice.text().is_some_and(|n| n.contains("trip"));
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, told
        ));
        let notice = app.notice.text().unwrap();
        assert!(
            notice.contains("1 added") && notice.contains("line break"),
            "{notice}"
        );
        assert_eq!(super::load(&file).unwrap(), [dir.join("c.png")]);
        // The file's own writer leaves such a name out too.
        let text = super::to_text(&[dir.join("a\n/b.png"), dir.join("c.png")], &[]);
        assert_eq!(super::parse(&text), [dir.join("c.png")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Move Collection to Trash asks first, as a folder's move to the trash does, and the
    /// question names the collection it asked about.
    #[test]
    fn trashing_a_collection_asks_first() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-ask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trip.gqv");
        std::fs::write(&file, "#GQview collection\n#end\n").unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, file.clone());
        app.run(&ctx, crate::ui::actions::Action::TrashCollection);
        assert!(app.dialog_open());
        assert!(file.is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Editing a collection Geeqie wrote keeps Geeqie's own lines: its #geometry line was
    /// dropped the first time Glimt took an image out.
    #[test]
    fn an_edit_keeps_geeqies_lines() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trip.gqv");
        let geeqie = "#GQview collection\n#created with Geeqie version 2.0\n#geometry: 0 0 400 300\n\"/a.jpg\"\n\"/b.jpg\"\n#end\n";
        std::fs::write(&file, geeqie).unwrap();
        super::edit(&file, false, |all| {
            all.retain(|p| p.as_os_str() != "/a.jpg")
        })
        .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("#geometry: 0 0 400 300\n") && text.contains("\"/b.jpg\""));
        assert!(!text.contains("/a.jpg") && text.ends_with("#end\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection that is a link is replaced by the edited list, and the file the link points
    /// to stays as it was.
    #[test]
    fn an_edit_never_writes_through_a_link() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("notes.txt");
        // The file linked to holds a line of its own, which must not be copied in.
        let text = "#GQview collection\n\"/a.jpg\"\nsecret\n\"/b.jpg\"\n#end\n";
        std::fs::write(&target, text).unwrap();
        let file = dir.join("trip.gqv");
        std::os::unix::fs::symlink(&target, &file).unwrap();
        super::edit(&file, false, |all| {
            all.retain(|p| p.as_os_str() != "/a.jpg")
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(!std::fs::symlink_metadata(&file).unwrap().is_symlink());
        assert_eq!(super::load(&file).unwrap(), [PathBuf::from("/b.jpg")]);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("secret"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An edit finds an entry named through a link by its real path, the name the list gives
    /// it, and writes the entries it leaves as the user spelled them. Compared as written, the
    /// entry was never found: Remove left it in the file, and a reorder put it last.
    #[test]
    fn an_edit_finds_an_entry_named_through_a_link() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-gqv-spelled-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let (real, file) = (dir.canonicalize().unwrap().join("real"), dir.join("c.gqv"));
        let spelled = [
            dir.join("real/b.jpg"),
            dir.join("link/a.jpg"),
            dir.join("real/c.jpg"),
        ];
        let text: String = spelled
            .iter()
            .map(|p| format!("\"{}\"\n", p.display()))
            .collect();
        std::fs::write(&file, format!("#GQview collection\n{text}#end\n")).unwrap();
        let ours = ["a.jpg", "b.jpg", "c.jpg"].map(|n| real.join(n));
        super::edit(&file, false, |all| super::reorder(all, &ours)).unwrap();
        let order = [&spelled[1], &spelled[0], &spelled[2]].map(|p| p.to_path_buf());
        assert_eq!(super::load(&file).unwrap(), order);
        super::edit(&file, false, |all| all.retain(|p| *p != ours[0])).unwrap();
        assert_eq!(super::load(&file).unwrap(), &order[1..]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A list opened by a path relative to where Glimt was started keeps its relative entries:
    /// `glimt show.m3u` in its own folder showed none of them.
    #[test]
    fn a_list_opened_by_a_relative_path_keeps_its_entries() {
        let dir = std::path::PathBuf::from(format!("glimt-test-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("show.m3u"), "a.jpg\nsub/b.jpg\n").unwrap();
        let got = super::load(&dir.join("show.m3u")).unwrap();
        let here = std::env::current_dir().unwrap().join(&dir);
        assert_eq!(got, [here.join("a.jpg"), here.join("sub/b.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An entry written through a link above a folder (/home on /var/home) follows the folder
    /// when it is renamed, by the real path the rename names it with. The entry's folder was
    /// gone, so its path was only made absolute, kept the link's name and never matched.
    #[test]
    fn an_entry_through_a_link_follows_its_folder_renamed() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-gqv-follow-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("base/pics")).unwrap();
        std::os::unix::fs::symlink(dir.join("base"), dir.join("top")).unwrap();
        let file = dir.join("c.gqv");
        let entry = dir.join("top/pics/a.jpg");
        let text = format!("#GQview collection\n\"{}\"\n#end\n", entry.display());
        std::fs::write(&file, text).unwrap();
        let base = dir.canonicalize().unwrap().join("base");
        let pairs = [(base.join("pics"), base.join("new"))];
        std::fs::rename(&pairs[0].0, &pairs[0].1).unwrap();
        let moved = crate::files::file_ops::moves_by_source(&pairs);
        super::edit(&file, false, |all| {
            for p in all {
                if let Some(to) = crate::files::file_ops::new_place(p, &moved) {
                    *p = to;
                }
            }
        })
        .unwrap();
        assert_eq!(super::load(&file).unwrap(), [base.join("new/a.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A window's order: images only in the file (added elsewhere) go last, those taken out
    /// elsewhere stay out.
    #[test]
    fn reorders_what_is_there() {
        let p = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let mut all = p(&["/a", "/b", "/c", "/new"]);
        super::reorder(&mut all, &p(&["/c", "/gone", "/a", "/b"]));
        assert_eq!(all, p(&["/c", "/a", "/b", "/new"]));
    }

    /// An entry the window does not list (on a disk not mounted) keeps its place in the file
    /// when another is moved first, or the list is sorted. Before, it went to the end, and each
    /// later move kept it there, also after the disk came back.
    #[test]
    fn entries_the_window_does_not_list_keep_their_place() {
        let p = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let mut all = p(&["/usb/a", "/b", "/c", "/d"]);
        super::reorder(&mut all, &p(&["/d", "/b", "/c"]));
        assert_eq!(all, p(&["/usb/a", "/d", "/b", "/c"]));
        let mut all = p(&["/c", "/usb/a", "/b"]);
        super::reorder(&mut all, &p(&["/b", "/c"]));
        assert_eq!(all, p(&["/b", "/usb/a", "/c"]));
    }

    /// Shuffled files back in the saved order; one added since goes last.
    #[test]
    fn puts_back_the_saved_order() {
        let p = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let saved = p(&["/a", "/b", "/c", "/gone"]);
        assert_eq!(
            super::in_saved_order(&saved, p(&["/c", "/new", "/a", "/b"])),
            p(&["/a", "/b", "/c", "/new"])
        );
    }

    use std::path::PathBuf;

    /// A plain list: relative to its folder (also with ..) or absolute, quoted or not, file://
    /// URIs (of this computer), comments, blank lines and CR, each image once and in the order
    /// written.
    #[test]
    fn reads_plain_lists() {
        let text = b"#EXTM3U\r\nb.jpg\r\n\n  \"sub/a b.jpg\"\n/abs/c.png\n\
                     file:///abs/with%20space.jpg\n# a comment\nb.jpg\n../Pictures/./d.jpg\n\
                     file://localhost/abs/e.jpg\nfile://nas/abs/f.jpg\nfile:///abs/100%+4.jpg\n";
        let paths = super::parse_list(text, std::path::Path::new("/lists"));
        assert_eq!(
            paths,
            [
                "/lists/b.jpg",
                "/lists/sub/a b.jpg",
                "/abs/c.png",
                "/abs/with space.jpg",
                "/Pictures/d.jpg",
                "/abs/e.jpg",
                "/abs/100%+4.jpg"
            ]
            .map(PathBuf::from)
        );
        assert!(super::is_list(std::path::Path::new("x.M3U")));
        assert!(!super::is_list(std::path::Path::new("x.gqv")));
    }

    /// Plain lists broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_lists() {
        let seed = b"#EXTM3U\nb.jpg\n\"sub/a.jpg\"\nfile:///x%2Fy%zz.jpg\n";
        for bytes in crate::support::test_support::broken(seed) {
            super::parse_list(&bytes, std::path::Path::new("/"));
        }
    }

    /// Returns the fingerprint of the collection `file`, as Rename Collection takes it.
    fn picked(file: &std::path::Path) -> crate::files::file_io::PickedFingerprints {
        let picked = crate::files::file_io::PickedFiles::new(vec![file.to_path_buf()]);
        picked.wait_for_fingerprints().1
    }

    /// A rename waits for an edit under way in another window, so the edit lands in the file
    /// before the rename, not in a new file of the old name.
    #[test]
    fn rename_waits_for_an_edit() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-rename-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trip.gqv");
        std::fs::write(&file, "#GQview collection\n\"/a.jpg\"\n").unwrap();
        let held = crate::files::file_io::file_lock(&file).unwrap();
        let renaming = {
            let file = file.clone();
            std::thread::spawn(move || super::rename(&file, "trip 2024", &picked(&file)))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(file.exists(), "renamed while the edit held the lock");
        drop(held);
        let new = renaming.join().unwrap().unwrap();
        assert!(!file.exists() && new.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Renamed in its own folder, never over another collection or to a name that is no file.
    #[test]
    fn renames_in_place() {
        let dir = std::env::temp_dir().join(format!("glimt-test-coll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.gqv"), dir.join("b.gqv"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        assert!(super::rename(&a, "b", &picked(&a)).is_err());
        assert!(super::rename(&a, "x/y", &picked(&a)).is_err());
        assert_eq!(
            super::rename(&a, " Travel ", &picked(&a)).unwrap(),
            dir.join("Travel.gqv")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Travel.gqv")).unwrap(),
            "a"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_and_writes_geeqie_collections() {
        let paths = [
            PathBuf::from("/a b/c.jpg"),
            PathBuf::from("/x/\"quoted\".png"),
        ];
        let text = super::to_text(&paths, &[]);
        assert!(text.starts_with(b"#GQview collection\n"));
        assert_eq!(super::parse(&text), paths);
        // As Geeqie writes it, with its geometry line; relative paths and a second line of the
        // same file are left out.
        let geeqie =
            b"#GQview collection\n#geometry: 0 0 400 300\n\"/p/1.jpg\"\n\"rel.jpg\"\n\"/p/1.jpg\"\n#end\n";
        assert_eq!(super::parse(geeqie), [PathBuf::from("/p/1.jpg")]);
        // Added to: a new one is made, also in a folder not there yet (a new install), and one
        // that cannot be read is left as it is.
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-{}", std::process::id()));
        let file = dir.join("a.gqv");
        assert_eq!(super::append(&file, &paths).unwrap(), 2);
        assert_eq!(super::append(&file, &paths[..1]).unwrap(), 0);
        let new = PathBuf::from("/n.jpg");
        assert_eq!(super::append(&file, &[new.clone(), new]).unwrap(), 1);
        // A link to itself: reading fails, though not as missing.
        let unreadable = dir.join("b.gqv");
        std::os::unix::fs::symlink(&unreadable, &unreadable).unwrap();
        assert!(super::append(&unreadable, &paths).is_err());
        assert!(unreadable.symlink_metadata().unwrap().is_symlink());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two windows' changes to one collection both stay: one takes an image out, the other
    /// renames another; a gone collection is not made again.
    #[test]
    fn keeps_what_other_windows_changed() {
        let p = |s: &str| PathBuf::from(s);
        let dir = std::env::temp_dir().join(format!("glimt-test-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c.gqv");
        std::fs::write(&file, super::to_text(&[p("/a"), p("/b"), p("/c")], &[])).unwrap();
        super::edit(&file, false, |all| all.retain(|x| x != &p("/a"))).unwrap();
        super::edit(&file, false, |all| {
            (all.iter_mut().filter(|x| **x == p("/c"))).for_each(|x| *x = p("/d"))
        })
        .unwrap();
        assert_eq!(super::load(&file).unwrap(), [p("/b"), p("/d")]);
        // A change that changes nothing leaves the file as it was.
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        super::edit(&file, false, |all| all.retain(|_| true)).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            written
        );
        let gone = dir.join("gone.gqv");
        assert!(super::edit(&gone, false, |all| all.clear()).is_err());
        assert!(!gone.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A saved collection that cannot be written after its images moved is told, as every
    /// failed save is. Before, it kept the old names, and the user was not told.
    #[test]
    fn a_saved_collection_that_cannot_follow_its_files_is_told() {
        use std::os::unix::fs::PermissionsExt;
        let dir = super::collections_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let saved = dir.join(format!("read-only-{}.gqv", std::process::id()));
        let from = PathBuf::from(format!("/glimt-test-moved-{}/a.png", std::process::id()));
        std::fs::write(&saved, super::to_text(std::slice::from_ref(&from), &[])).unwrap();
        std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o444)).unwrap();
        crate::files::file_io::take_saves_failed();
        super::follow_in_saved_collections(&[(from.clone(), from.with_file_name("b.png"))], None);
        crate::files::file_io::flush();
        let told = crate::files::file_io::take_saves_failed();
        std::fs::remove_file(&saved).unwrap();
        let name = saved.display().to_string();
        assert!(told.iter().any(|e| e.contains(&name)), "{told:?}");
    }

    /// An entry that follows its file moved to a name with a line break, which a line of the
    /// collection cannot hold, is taken out with a notice. Before, it was left out of the file
    /// without a word.
    #[test]
    fn an_entry_moved_to_a_name_with_a_line_break_is_told() {
        let dir = super::collections_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let saved = dir.join(format!("line-break-{}.gqv", std::process::id()));
        let from = PathBuf::from(format!(
            "/glimt-test-line-break-{}/a.png",
            std::process::id()
        ));
        let kept = from.with_file_name("b.png");
        std::fs::write(&saved, super::to_text(&[from.clone(), kept.clone()], &[])).unwrap();
        crate::files::file_io::take_saves_failed();
        let to = from.parent().unwrap().join("new\nline/a.png");
        super::follow_in_saved_collections(&[(from, to)], None);
        crate::files::file_io::flush();
        let told = crate::files::file_io::take_saves_failed();
        let now = super::load(&saved).unwrap();
        std::fs::remove_file(&saved).unwrap();
        assert_eq!(now, [kept]);
        assert!(told.iter().any(|e| e.contains("line break")), "{told:?}");
    }

    /// An entry written through a link to a folder whose name holds a line break is kept when
    /// another entry is taken out, as the entry is written as it was spelled. Before, it was
    /// refused for the line break in its real path, and taken out of the file with a notice.
    #[test]
    fn an_entry_through_a_link_to_a_name_with_a_line_break_is_kept() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-link-line-break-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("new\nline")).unwrap();
        std::os::unix::fs::symlink(dir.join("new\nline"), dir.join("link")).unwrap();
        let (kept, taken_out) = (dir.join("link/a.png"), dir.join("b.png"));
        let saved = dir.join("c.gqv");
        std::fs::write(
            &saved,
            super::to_text(&[kept.clone(), taken_out.clone()], &[]),
        )
        .unwrap();
        crate::files::file_io::take_saves_failed();
        super::edit(&saved, false, |all| all.retain(|p| *p != taken_out)).unwrap();
        let now = super::load(&saved).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(now, [kept]);
    }

    /// Collection files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let paths = [PathBuf::from("/a b/c.jpg"), PathBuf::from("/x/\"q\".png")];
        for text in crate::support::test_support::broken(&super::to_text(&paths, &[])) {
            super::parse(&text);
        }
    }

    /// F5 reads a plain list's file again: an entry a script added since shows, and one it took
    /// out goes, while an entry the window took out stays out and the window's order stays.
    /// Before, a list was listed from the window's own list and never read again, so neither
    /// F5 nor Auto Refresh showed the script's change, and Delete acted on a file the list no
    /// longer named.
    #[test]
    fn a_plain_list_read_again_shows_its_files_change_and_keeps_the_windows() {
        use crate::support::test_support::{app_on, run_background_until};
        use crate::ui::actions::Action;
        let dir = std::env::temp_dir().join(format!("glimt-test-m3u-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png", "d.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let list = dir.join("show.m3u");
        std::fs::write(&list, "a.png\nb.png\nc.png\n").unwrap();
        let (mut app, ctx) = app_on(&list);
        let names = |app: &crate::App| -> Vec<String> {
            (0..app.list.files().len())
                .map(|i| {
                    app.path(i)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        let refresh = |app: &mut crate::App| {
            app.run(&ctx, Action::Refresh);
            assert!(run_background_until(app, &ctx, |a| !a.folder.is_listing()));
        };
        assert_eq!(names(&app), ["a.png", "b.png", "c.png"]);
        // The window takes b out, and moves c first.
        app.list.select([1].into());
        app.run(&ctx, Action::RemoveFromCollection);
        app.list.select([1].into());
        app.run(&ctx, Action::MoveFirst);
        assert_eq!(names(&app), ["c.png", "a.png"]);
        // A script appends d and takes a out.
        crate::files::file_io::wait_for_queued_writes();
        std::fs::write(&list, "b.png\nc.png\nd.png\n").unwrap();
        refresh(&mut app);
        assert_eq!(names(&app), ["c.png", "d.png"]);
        // The list is never written.
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "b.png\nc.png\nd.png\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Move Earlier under a search moves the image one row as the rows show it, past the image
    /// shown before it, and an image the search hides keeps its place. A search that shows
    /// another order than the collection's refuses the move, and says so. Before, the image
    /// passed the hidden one, and the rows showed no change until a second press.
    #[test]
    fn a_move_under_a_search_moves_a_row_as_shown() {
        use crate::ui::actions::Action;
        let dir = std::env::temp_dir().join(format!("glimt-test-move-rows-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["a1.png", "x.png", "a2.png"];
        let mut gqv = String::from("#GQview collection\n");
        for name in names {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            gqv += &format!("\"{}\"\n", dir.join(name).display());
        }
        std::fs::write(dir.join("rows.gqv"), gqv).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir.join("rows.gqv"));
        let order = |app: &crate::App| -> Vec<String> {
            (0..app.list.files().len())
                .map(|i| {
                    app.path(i)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        assert_eq!(order(&app), names);
        app.search.set_query(r"re:/a\d\.png$");
        app.update_search(&ctx);
        app.list.select([2].into());
        app.run(&ctx, Action::MoveEarlier);
        assert_eq!(order(&app), ["a2.png", "x.png", "a1.png"]);
        // Fuzzy, a2 ranks before a1 here: the rows are not in the collection's order.
        app.search.set_query("a1");
        app.update_search(&ctx);
        app.list.set_matches(Some(vec![2, 0]));
        app.list.select([0].into());
        app.run(&ctx, Action::MoveEarlier);
        assert_eq!(order(&app), ["a2.png", "x.png", "a1.png"]);
        assert!(
            app.notice
                .text()
                .unwrap_or_default()
                .contains("another order")
        );
        crate::files::file_io::wait_for_queued_writes();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Images of a collection that are moved go one place each, never past another moved one,
    /// and to the start or the end in their own order.
    #[test]
    fn moves_in_a_collection() {
        use super::{Action, moved};
        let set = |v: &[usize]| v.iter().copied().collect();
        // 0 1 2 3 4 5 with 1, 2 and 5 picked.
        let picked = set(&[1, 2, 5]);
        let all = [0, 1, 2, 3, 4, 5];
        assert_eq!(
            moved(6, &all, &picked, Action::MoveEarlier),
            [1, 2, 0, 3, 5, 4]
        );
        assert_eq!(
            moved(6, &all, &picked, Action::MoveLater),
            [0, 3, 1, 2, 4, 5]
        );
        assert_eq!(
            moved(6, &all, &picked, Action::MoveFirst),
            [1, 2, 5, 0, 3, 4]
        );
        assert_eq!(
            moved(6, &all, &picked, Action::MoveLast),
            [0, 3, 4, 1, 2, 5]
        );
        // At the edge already: where it was.
        assert_eq!(
            moved(3, &[0, 1, 2], &set(&[0]), Action::MoveEarlier),
            [0, 1, 2]
        );
        assert_eq!(
            moved(0, &[], &set(&[]), Action::MoveLater),
            Vec::<usize>::new()
        );
        // [a1, x, a2] under a search that shows a1 and a2: a2 passes a1, the row before it, and
        // x, which the search hides, keeps its place. Before, a2 passed x alone, and the rows
        // still read a1, a2.
        assert_eq!(
            moved(3, &[0, 2], &set(&[2]), Action::MoveEarlier),
            [2, 1, 0]
        );
        assert_eq!(moved(3, &[0, 2], &set(&[0]), Action::MoveLater), [2, 1, 0]);
        assert_eq!(moved(3, &[0, 2], &set(&[2]), Action::MoveFirst), [2, 1, 0]);
        // Dragged before 4, before 0, to the end, and onto a place of their own.
        use super::dropped;
        assert_eq!(dropped(6, &picked, 4), [0, 3, 1, 2, 5, 4]);
        assert_eq!(dropped(6, &picked, 0), [1, 2, 5, 0, 3, 4]);
        assert_eq!(dropped(6, &picked, 6), [0, 3, 4, 1, 2, 5]);
        assert_eq!(dropped(6, &picked, 2), [0, 1, 2, 5, 3, 4]);
    }
}
