//! The file chooser (zenity or kdialog) and the small dialogs: a name asked for, a delete
//! confirmed, a share's login, a job cut short.

use crate::features::copy_for_sharing::SharingDialog;
use crate::features::print::{PrintJob, PrintTo};
use crate::files::config::xdg_dir;
use crate::files::external::{Login, command};
use crate::files::file_ops::{NOT_ON_FAT, OperationReport, fat_file_system_type};
use crate::model::listing::pattern_name;
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::{files::external, files::journal, images::save, model::collections, tf, ui::theme};
use eframe::egui;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// How long the question to delete for good what the trash refused is on screen before Enter
/// answers it. It opens by itself, when the folder has been read again, so an Enter pressed for
/// something else at that moment must not delete files for good unseen.
const ENTER_IGNORED_FOR: f64 = 0.5;

/// What a file dialog (zenity or kdialog) was opened for.
pub(crate) enum Pick {
    Open,
    /// Several folders to show as one list.
    Folders,
    /// A folder with the images of every folder below it.
    Recursive,
    /// Open a collection: the dialog starts in the collections folder.
    Collection,
    CopyTo(Vec<PathBuf>),
    MoveTo(Vec<PathBuf>),
    /// Where to save the PDF of these images.
    SavePdf(PrintJob),
    /// A folder for the sort manager.
    SortFolder,
    /// An ICC profile for the screen.
    ScreenProfile,
    /// An ICC profile for images without one.
    InputProfile,
    /// A Lightroom Classic catalog to import.
    Lightroom,
    /// The folder to put a backup of Glimt's own files in.
    BackUp,
    /// Where to save frame (or page) number .1 of the file .0, as PNG, turned as the view
    /// showed it (.2).
    SaveFrame(PathBuf, usize, save::CopyAs),
    /// Save As: where to save the copy .1 of the file .0, starting at the name .2.
    SaveAs(PathBuf, save::CopyAs, PathBuf),
    /// The folder for every frame (or page) of the file .0, as PNG files turned as the view
    /// showed it (.1).
    ExportFrames(PathBuf, save::CopyAs),
    /// Copy for Sharing: the folder for the copies of these images, which keep the tags named.
    CopyForSharing(Vec<PathBuf>, std::collections::HashSet<String>),
}

/// What the one-line text dialog is asking for.
#[derive(Clone, Copy)]
pub(crate) enum Prompt {
    /// A new folder in the folder in `Dialogs::folder_target`.
    NewFolder,
    /// Rename the folder in `Dialogs::folder_target`.
    RenameFolder,
    /// Rename the shown image.
    Rename,
    /// Rename the selected files with a pattern: * = old name, # = number.
    RenameMany,
    /// A new collection with the selected files.
    NewCollection,
    /// A new name for the collection file in `Dialogs::folder_target`.
    RenameCollection,
    /// Save the look shown as a theme file of this name.
    SaveTheme,
    /// This window's own title, a template as in Preferences.
    WindowTitle,
    /// This window's --name, which `--remote` reaches it by.
    WindowName,
}

/// The files a dialog asks about, by their names in the list, where the list was, and how many
/// selected files the search hid and the question leaves out. `App::ask_about_targets` makes it,
/// and a dialog about files opens only with one. So no such dialog opens without the files its
/// answer acts on.
pub(crate) struct Asked {
    opened_folder_hash: u64,
    names: Vec<OsString>,
    left_out: Option<String>,
}

impl Asked {
    /// The files `names` asked about while the list showed what `opened_folder_hash` stands
    /// for, with `left_out`, the text that tells of the selected files the question leaves out.
    pub(crate) fn new(
        opened_folder_hash: u64,
        names: Vec<OsString>,
        left_out: Option<String>,
    ) -> Asked {
        Asked {
            opened_folder_hash,
            names,
            left_out,
        }
    }
}

/// Save as Theme's answer: the theme's name, and its file or why it was not saved.
type SavedTheme = (String, Result<PathBuf, String>);

/// The small dialogs: a name asked for, a delete or a folder's trashing confirmed, and the file
/// chooser.
#[derive(Default)]
pub(crate) struct Dialogs {
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// The text the dialog opened with. An answer left as it was does nothing, because the
    /// name shown is not always the name on disk: bytes that are not UTF-8 are shown as a
    /// replacement character, and the answer is trimmed of the spaces a name may start or end
    /// with.
    opened_with: String,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm_delete: Option<bool>,
    /// The type of the network or cloud mount whose files the trash refused, when the delete
    /// for good asked about is instead of a move to the trash.
    trash_refused: Option<String>,
    /// When the delete dialog was first drawn, in egui's input time.
    delete_dialog_opened_at: Option<f64>,
    /// What a folder or collection dialog acts on, kept from when it asked: the folder to rename
    /// or, with `is_confirming_folder_trash`, to trash, the folder New Folder makes its folder
    /// in, or the collection file to rename. A script that opens another folder or collection
    /// while the dialog is open does not change it.
    folder_target: Option<PathBuf>,
    is_confirming_folder_trash: bool,
    /// The collection file whose move to the trash is being confirmed. It is kept from when
    /// Move Collection to Trash asked, so the answer acts on that collection.
    confirm_collection: Option<PathBuf>,
    /// Answer from an open file dialog (zenity or kdialog).
    file_dialog: Option<Pick>,
    picked_path: Receiving<Option<PathBuf>>,
    /// Save as Theme's answer while its file is written.
    saved_theme: Receiving<(SavedTheme, Working)>,
    /// The files a dialog asks about, by their names in the list, where the list was
    /// (`ask_about_targets`), and how many selected files the search hid and the question
    /// leaves out. The dialog names them, and its answer acts on them, wherever a refresh put
    /// them. What it leaves out is kept as said when it asked, so that a selection changed
    /// while it is open does not change it.
    asked: Option<Asked>,
    /// The type of the FAT or exFAT file system that Rename Many renames on, looked up once as
    /// the dialog opens, so that its preview can warn without reading the mount table in every
    /// frame. It is `None` on any other file system, and until the look-up has answered.
    fat_file_system_type: Option<String>,
    /// The look-up of `fat_file_system_type`, in the background, because the mount table is
    /// read for it and the files' folder is looked at.
    fat_file_system_answer: Receiving<Option<String>>,
    /// File → Copy for Sharing's dialog, while it is open.
    copy_for_sharing: Option<SharingDialog>,
    /// The files that Open With Default App asks about before it opens them, or none while it
    /// does not ask.
    files_to_open: Vec<PathBuf>,
}

impl Dialogs {
    /// Takes the type of the file system that Rename Many renames on, once its look-up in the
    /// background has answered.
    fn receive_fat_file_system_type(&mut self) {
        if let Took::Answer(kind) = self.fat_file_system_answer.take() {
            self.fat_file_system_type = kind;
        }
    }

    /// Opens the text dialog `prompt`, with `text` typed in already. A dialog about files
    /// opens through `ask_about` instead, which takes the files it asks about.
    fn ask(&mut self, prompt: Prompt, text: String) {
        self.opened_with.clone_from(&text);
        self.prompt = Some((prompt, text));
    }

    /// Whether `text`, as typed, is what the dialog `prompt` opened with. Rename Many's is
    /// a pattern to apply, not a name already there, so it is never unchanged.
    fn is_unchanged(&self, prompt: Prompt, text: &str) -> bool {
        !matches!(prompt, Prompt::RenameMany) && text == self.opened_with
    }

    /// Opens the text dialog `prompt` about the files `asked`, with `text` typed in already.
    pub(crate) fn ask_about(&mut self, asked: Asked, prompt: Prompt, text: String) {
        self.asked = Some(asked);
        self.ask(prompt, text);
    }

    /// Asks for the name of the theme to save the look shown as.
    pub(crate) fn ask_theme_name(&mut self) {
        self.ask(Prompt::SaveTheme, String::new());
    }

    /// Asks for this window's own title, with `title` typed in already.
    pub(crate) fn ask_window_title(&mut self, title: String) {
        self.ask(Prompt::WindowTitle, title);
    }

    /// Asks for this window's --name, with `name` typed in already.
    pub(crate) fn ask_window_name(&mut self, name: String) {
        self.ask(Prompt::WindowName, name);
    }

    /// Opens the Copy for Sharing dialog `dialog`, or keeps it open after a frame drew it.
    pub(crate) fn show_sharing(&mut self, dialog: SharingDialog) {
        self.copy_for_sharing = Some(dialog);
    }

    /// The Copy for Sharing dialog, taken to be drawn, or `None` when it is not open.
    pub(crate) fn take_sharing(&mut self) -> Option<SharingDialog> {
        self.copy_for_sharing.take()
    }

    /// Asks whether to open `files`, each in its default application.
    pub(crate) fn ask_open_files(&mut self, files: Vec<PathBuf>) {
        self.files_to_open = files;
    }

    /// Asks whether to delete the files `asked`: to the trash when `trash`, else for good.
    pub(crate) fn confirm_delete(&mut self, asked: Asked, trash: bool) {
        self.asked = Some(asked);
        self.confirm_delete = Some(trash);
        self.trash_refused = None;
        self.delete_dialog_opened_at = None;
    }

    /// Asks whether to delete the files `asked` for good, because the trash refused them on a
    /// network or cloud mount of type `kind`.
    pub(crate) fn confirm_delete_instead(&mut self, asked: Asked, kind: String) {
        self.asked = Some(asked);
        self.confirm_delete = Some(false);
        self.trash_refused = Some(kind);
        self.delete_dialog_opened_at = None;
    }

    /// Whether Rename or a delete is asking about the files on screen.
    pub(crate) fn asks_about_files(&self) -> bool {
        self.prompt.is_some() || self.confirm_delete.is_some()
    }

    /// Returns how many selected files the search hid and the question leaves out, as said when
    /// it was asked, or `None` when it leaves none out.
    pub(crate) fn asked_left_out(&self) -> Option<&str> {
        self.asked.as_ref()?.left_out.as_deref()
    }

    /// Returns the names of the files asked about, which the dialog names while it is open.
    pub(crate) fn asked_names(&self) -> &[OsString] {
        self.asked.as_ref().map_or(&[], |asked| &asked.names)
    }

    /// The files asked about, taken for the answer to act on.
    pub(crate) fn take_asked(&mut self) -> (u64, Vec<OsString>) {
        (self.asked.take()).map_or_else(Default::default, |a| (a.opened_folder_hash, a.names))
    }

    /// Asks for a new name for the folder `dir`.
    pub(crate) fn ask_rename_folder(&mut self, dir: &Path) {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        self.ask(Prompt::RenameFolder, name.into_owned());
        self.folder_target = Some(dir.to_path_buf());
    }

    /// Asks for the name of a new folder in the folder `dir`.
    pub(crate) fn ask_new_folder(&mut self, dir: &Path) {
        self.ask(Prompt::NewFolder, String::new());
        self.folder_target = Some(dir.to_path_buf());
    }

    /// Asks for a new name for the collection `file`.
    pub(crate) fn ask_rename_collection(&mut self, file: &Path) {
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        self.ask(Prompt::RenameCollection, name.into_owned());
        self.folder_target = Some(file.to_path_buf());
    }

    /// Asks whether to move the collection `file` to the trash.
    pub(crate) fn ask_trash_collection(&mut self, file: &Path) {
        self.confirm_collection = Some(file.to_path_buf());
    }

    /// Asks whether to move the folder `dir` to the trash.
    pub(crate) fn ask_trash_folder(&mut self, dir: &Path) {
        self.folder_target = Some(dir.to_path_buf());
        self.is_confirming_folder_trash = true;
    }

    /// The folder the answer acts on, taken.
    pub(crate) fn take_folder(&mut self) -> Option<PathBuf> {
        self.folder_target.take()
    }

    /// The folder an answer acts on, without a dialog, for a test of the answer alone.
    #[cfg(test)]
    pub(crate) fn act_on_folder(&mut self, dir: PathBuf) {
        self.folder_target = Some(dir);
    }
}

impl crate::App {
    /// Asks for Rename Many's pattern for the files `asked`. The file system of the first
    /// file's folder is looked up once, in the background, for the preview of its new name.
    pub(crate) fn ask_rename_many(&mut self, asked: Asked) {
        let first = (asked.names.first()).map(|n| self.folder.dir().join(n));
        let folder = first
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(folder.and_then(|f| fat_file_system_type(&f)));
            ctx.request_repaint();
        });
        self.dialogs.fat_file_system_type = None;
        self.dialogs.fat_file_system_answer = Receiving::of(rx);
        self.dialogs
            .ask_about(asked, Prompt::RenameMany, "*_###".into());
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    pub(crate) fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        // One at a time: only one answer is waited for, and a second dialog's would replace the
        // first's, whose copy or move would then never happen.
        if self.dialogs.file_dialog.is_some() {
            self.notice.give(tr("A file dialog is open already").into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start = self.dialog_start(&purpose);
        let save = matches!(
            purpose,
            Pick::SavePdf(_) | Pick::SaveFrame(..) | Pick::SaveAs(..)
        );
        let several = matches!(purpose, Pick::Folders);
        let ctx = ctx.clone();
        let title = match purpose {
            Pick::Open if folder => tr("Open Folder"),
            Pick::Open => tr("Open File"),
            Pick::Folders => tr("Open Folders (Ctrl+click for several)"),
            Pick::Recursive => tr("Open Folder with Its Subfolders"),
            Pick::Collection => tr("Open Collection"),
            Pick::CopyTo(_) | Pick::CopyForSharing(..) => tr("Copy to Folder"),
            Pick::MoveTo(_) => tr("Move to Folder"),
            Pick::SavePdf(_) => tr("Save as PDF"),
            Pick::SortFolder => tr("Add to the Sort Manager"),
            Pick::ScreenProfile => tr("Screen Profile"),
            Pick::InputProfile => tr("Profile of Images Without One"),
            Pick::Lightroom => tr("Import Lightroom Catalog"),
            Pick::BackUp => tr("Back Up Glimt's Data To"),
            Pick::SaveFrame(..) => tr("Save This Frame as PNG"),
            Pick::ExportFrames(..) => tr("Export Every Frame To"),
            Pick::SaveAs(..) => tr("Save As"),
        };
        let notices = self.notice_sender.clone();
        let make_start_first = matches!(purpose, Pick::Collection);
        std::thread::spawn(move || {
            if make_start_first {
                drop(std::fs::create_dir_all(&start));
            }
            #[allow(clippy::disallowed_methods)] // waits for the user, in its own thread
            let zenity = || {
                let mut cmd = command("zenity");
                cmd.arg("--file-selection").arg("--title").arg(title);
                cmd.arg("--filename").arg(&start);
                if save {
                    cmd.arg("--save").arg("--confirm-overwrite");
                }
                if folder {
                    cmd.arg("--directory");
                }
                if several {
                    cmd.arg("--multiple").arg("--separator=\n");
                }
                cmd.output()
            };
            // KDE desktops often have kdialog but not zenity.
            #[allow(clippy::disallowed_methods)] // waits for the user, in its own thread
            let kdialog = || {
                let mode = if save {
                    "--getsavefilename"
                } else if folder {
                    "--getexistingdirectory"
                } else {
                    "--getopenfilename"
                };
                let mut cmd = command("kdialog");
                cmd.arg("--title").arg(title).arg(mode).arg(&start);
                cmd.output()
            };
            let output = match (
                external::has_program("zenity"),
                external::has_program("kdialog"),
            ) {
                (true, _) => zenity(),
                (false, true) => kdialog(),
                (false, false) => Err(std::io::ErrorKind::NotFound.into()),
            };
            let picked = match output {
                Err(e) => {
                    let _ = notices.send(tf!("No file dialog (install zenity or kdialog): {e}", e));
                    None
                }
                // Cancel exits with status 1 in both.
                Ok(o) if !o.status.success() => None,
                Ok(mut o) => {
                    o.stdout.pop_if(|b| *b == b'\n');
                    Some(PathBuf::from(std::ffi::OsString::from_vec(o.stdout)))
                }
            };
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.dialogs.file_dialog = Some(purpose);
        self.dialogs.picked_path = Receiving::of(rx);
    }

    /// Returns where the file dialog for `purpose` starts: the folder shown, or for a file to
    /// save, its name beside the image it is made from.
    fn dialog_start(&self, purpose: &Pick) -> PathBuf {
        match purpose {
            // A name for the file to save: the first image's as a PDF, beside it. In a
            // collection, several folders or with subfolders the folder shown is not the
            // image's.
            Pick::SavePdf(job) => (job.paths.first()).map_or_else(
                || self.folder.dir().join("print.pdf"),
                |p| p.with_extension("pdf"),
            ),
            // "name-frame-3.png" beside the file.
            Pick::SaveFrame(path, k, _) => path.with_file_name(save::stem_with_suffix(
                path,
                &format!("-frame-{}.png", k + 1),
            )),
            Pick::SaveAs(_, _, start) => start.clone(),
            // Made first in the dialog's thread (`pick`): the file dialog cannot start in a
            // folder that is not there.
            Pick::Collection => (collections::collections_dir())
                .unwrap_or_default()
                .join(""),
            // Where colord and the user keep them.
            Pick::ScreenProfile | Pick::InputProfile => {
                (xdg_dir("XDG_DATA_HOME", ".local/share", "icc"))
                    .filter(|d| d.is_dir())
                    .unwrap_or_else(|| "/usr/share/color/icc".into())
                    .join("")
            }
            _ => self.folder.dir().join(""),
        }
    }

    /// Saves the look shown as the theme `text` in the user's themes folder, in a thread of its
    /// own, as a home on a share that stopped answering would hold the window. Its answer is
    /// taken by `receive_saved_theme`.
    fn save_theme(&mut self, ctx: &egui::Context, text: &str) {
        // Never in the place of a built-in theme, which a file of its name would hide for good.
        let file = match theme::own_themes_dir() {
            _ if theme::is_built_in(text) => {
                Err(tf!("{text} is a built-in theme: choose another name", text))
            }
            Some(dir) => Ok(dir.join(format!("{text}.theme"))),
            None => Err(tr("no home folder").to_owned()),
        };
        let file = match file {
            Ok(file) => file,
            Err(e) => return self.notice.give(tf!("Could not save the theme: {e}", e)),
        };
        let (bytes, text) = (theme::Theme::file_from(ctx), text.to_owned());
        let (tx, rx) = answer_channel(&self.busy);
        let (ctx, pointer) = (ctx.clone(), Working::new(&self.busy));
        std::thread::spawn(move || {
            drop(std::fs::create_dir_all(file.parent().unwrap_or(&file)));
            // Whole or not at all: a full disk left half a theme, whose name was then taken. And
            // never over a theme there, hand-edited perhaps.
            let made = crate::files::file_io::create_file(&file, bytes);
            let saved = made.map(|_| file).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    tf!("{text} exists already: choose another name", text)
                }
                _ => e.to_string(),
            });
            let _ = tx.send((text, saved));
            drop(pointer);
            ctx.request_repaint();
        });
        self.dialogs.saved_theme = Receiving::of(rx);
    }

    /// Takes the answer of Save as Theme. The theme saved is the one in use from then on, and
    /// the theme files are read again for it.
    pub(crate) fn receive_saved_theme(&mut self, ctx: &egui::Context) {
        // The busy pointer stays until the answer is handled.
        let ((text, saved), _pointer) = match self.dialogs.saved_theme.take() {
            Took::Answer(answer) => answer,
            Took::Gone => return self.tell_background_stopped(tr("Save as Theme…")),
            Took::Nothing => return,
        };
        self.notice.give(match saved {
            Ok(file) => {
                self.config.theme = text;
                self.config.save();
                self.desktop_settings.read_theme_files_again(ctx);
                tf!(
                    "Saved as {file}: edit it, then choose it again",
                    file = file.display()
                )
            }
            Err(e) => tf!("Could not save the theme: {e}", e),
        });
    }

    /// The file dialog's answer, for what it was opened for.
    pub(crate) fn receive_picked(&mut self, ctx: &egui::Context) {
        let picked = match self.dialogs.picked_path.take() {
            Took::Answer(picked) => picked,
            // A dialog whose thread ended without an answer is over too: the next may open.
            Took::Gone => return self.dialogs.file_dialog = None,
            Took::Nothing => return,
        };
        {
            let Some(purpose) = self.dialogs.file_dialog.take() else {
                return;
            };
            match (purpose, picked) {
                (_, None) => {}
                (Pick::Open | Pick::Collection, Some(path)) => self.visit(ctx, path),
                // As View → Include Subfolders, which turns it off again.
                (Pick::Recursive, Some(path)) => {
                    self.config.show_subfolders = true;
                    self.config.save();
                    self.visit(ctx, path);
                }
                // One per line, from zenity's --multiple.
                (Pick::Folders, Some(paths)) => {
                    use std::os::unix::ffi::OsStringExt;
                    let bytes = paths.into_os_string().into_encoded_bytes();
                    let all = (bytes.split(|&b| b == b'\n'))
                        .filter(|p| !p.is_empty())
                        .map(|p| PathBuf::from(OsString::from_vec(p.to_vec())))
                        .collect();
                    self.open_folders(ctx, all);
                }
                (Pick::CopyTo(paths), Some(dest)) => self.transfer(paths, dest, true),
                (Pick::MoveTo(paths), Some(dest)) => self.transfer(paths, dest, false),
                (Pick::SavePdf(job), Some(file)) => self.print(ctx, job, PrintTo::File(file)),
                (Pick::SortFolder, Some(dir)) => self.add_sort_folder(dir),
                (Pick::ScreenProfile, Some(file)) => {
                    self.set_screen_profile(file.to_string_lossy().into_owned())
                }
                (Pick::InputProfile, Some(file)) => {
                    self.set_input_profile(file.to_string_lossy().into_owned())
                }
                (Pick::Lightroom, Some(file)) => self.read_lightroom(ctx, file),
                (Pick::BackUp, Some(dir)) => self.back_up(ctx, dir),
                (Pick::SaveFrame(path, k, how), Some(file)) => {
                    self.export_frames(ctx, path, Some(k), file, how)
                }
                (Pick::ExportFrames(path, how), Some(dir)) => {
                    self.export_frames(ctx, path, None, dir, how)
                }
                (Pick::SaveAs(path, how, _), Some(to)) => self.save_as_to(ctx, path, how, to),
                (Pick::CopyForSharing(paths, kept), Some(dir)) => {
                    self.copy_for_sharing(paths, dir, kept)
                }
            }
        }
    }

    /// A share mounted, or why not: its folder opens, or the error or a login is asked for.
    pub(crate) fn receive_mount(&mut self, ctx: &egui::Context) {
        let done = match self.folder.take_mounted() {
            Took::Nothing => return,
            Took::Gone => return self.tell_background_stopped(tr("Connecting")),
            Took::Answer(done) => done,
        };
        {
            // "Connecting to …" goes: the folder, the error or the login takes its place.
            self.notice.clear();
            match done {
                (Ok(path), _) => self.visit(ctx, path),
                (Err(Some(e)), _) => self.notice.give(e),
                // Ask for the login; the user name defaults to this one, as gio's does.
                (Err(None), login) => {
                    let user = std::env::var("USER").unwrap_or_default();
                    self.folder.ask_login(Login {
                        user,
                        domain: "WORKGROUP".into(),
                        ..login
                    });
                }
            }
        }
    }

    /// Asks for a name: a new folder, a new name for a folder or file, Rename Many's pattern, a new
    /// collection, a theme to save.
    pub(crate) fn prompt_dialog(&mut self, ctx: &egui::Context) {
        if let Some((prompt, mut text)) = self.dialogs.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => tr("New folder name:").to_owned(),
                Prompt::RenameFolder => tr("New folder name:").to_owned(),
                Prompt::SaveTheme => tr("Name of the new theme:").to_owned(),
                Prompt::WindowTitle => tr(
                    "Title of this window (%name%, %path%, %folder%, %app%, %id%; empty: as \
                     in Preferences):",
                )
                .to_owned(),
                Prompt::WindowName => tr(
                    "Name of this window, which glimt --name NAME --remote reaches it by \
                     (empty: its process ID):",
                )
                .to_owned(),
                Prompt::RenameCollection => tr("New name of the collection:").to_owned(),
                Prompt::Rename => tr("New file name:").to_owned(),
                Prompt::NewCollection => tf!(
                    "Name of a new collection with {count}:",
                    count = count(self.dialogs.asked_names().len(), "image")
                ),
                Prompt::RenameMany => tf!(
                    "New names for {count} (* = old name, # = number, ### = 001):",
                    count = count(self.dialogs.asked_names().len(), "file")
                ),
            };
            // The first new name, so the pattern can be checked before renaming.
            let first = (self.dialogs.asked_names().first()).map(|n| self.folder.dir().join(n));
            self.dialogs.receive_fat_file_system_type();
            let fat = self.dialogs.fat_file_system_type.clone();
            let mut answer = None;
            let mut field = None;
            let modal = egui::Modal::new(egui::Id::unique("prompt")).show(ctx, |ui| {
                ui.label(title);
                let edit = ui.text_edit_singleline(&mut text);
                let enter = entered(&edit);
                field = Some(edit.id);
                if let (Prompt::RenameMany, Some(first)) = (prompt, &first) {
                    let name = pattern_name(text.trim(), first, 1);
                    ui.weak(tf!("First: {name}", name = name.to_string_lossy()));
                    if let Some(warning) = fat_warning(&name, fat.as_deref()) {
                        ui.colored_label(ui.visuals().warn_fg_color, warning);
                    }
                }
                ui.horizontal(|ui| {
                    if crate::ui::menus::button(ui, tr("OK (Enter)")).clicked() || enter {
                        answer = Some(text.clone());
                    }
                    if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if let Some(text) = answer {
                self.take_prompt_answer(ctx, prompt, &text);
            } else if !modal.should_close() {
                self.dialogs.prompt = Some((prompt, text));
                focus_on_open(ctx, field);
            }
        }
    }

    /// Renames the collection Rename Collection asked about to `text`. It is done in the writer
    /// thread for the user's folders, after this window's edits of the collection, because the
    /// rename waits for the collection's lock, and a share that stopped answering would hold
    /// the window. `collection_renamed` makes the window follow it.
    fn rename_collection(&mut self, ctx: &egui::Context, text: &str) {
        let Some(old) = self.dialogs.take_folder() else {
            return;
        };
        let (report_tx, notices) = (
            self.file_operations.report_sender(),
            self.notice_sender.clone(),
        );
        let (text, ctx) = (text.to_owned(), ctx.clone());
        let pointer = crate::support::threads::Working::new(&self.busy);
        crate::files::file_io::write_later_in_user_folders(move || {
            match collections::rename(&old, &text) {
                Ok(new) => drop(report_tx.send(OperationReport {
                    collection_renamed: Some((old, new)),
                    ..Default::default()
                })),
                Err(e) => drop(notices.send(e)),
            }
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Makes the window follow the collection `old`, which was renamed to `new`, when it still
    /// shows that collection.
    pub(crate) fn collection_renamed(&mut self, ctx: &egui::Context, old: &Path, new: PathBuf) {
        if self.folder.collection() == Some(old) {
            self.folder.collection_renamed(new);
            self.start_watching(ctx);
            self.update_window_title(ctx);
        }
    }

    /// The text dialog was answered with `text` as typed. An answer left as the dialog opened
    /// with does nothing, as Cancel: a file's name with bytes that are not UTF-8, or with a
    /// space at its end, was otherwise renamed to the name shown, trimmed.
    pub(crate) fn take_prompt_answer(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if !self.dialogs.is_unchanged(prompt, text) {
            self.act_on_prompt_answer(ctx, prompt, text.trim());
        }
    }

    /// Window → Set Window Name… was answered with `name`. The notice says why when the name is
    /// not taken.
    fn name_window(&mut self, ctx: &egui::Context, name: &str) {
        // The name typed is told in the user's language. The other reasons come from the
        // system, rarely, and are the English ones that `--remote name` prints too.
        if !name.is_empty() && !crate::features::remote::can_be_window_name(name) {
            let why = tr(
                "Could not name this window: a name holds up to 64 letters, digits, \
                 - and _, and not only digits",
            );
            return self.notice.give(why.to_owned());
        }
        if let Err(why) = self.set_window_name(ctx, name) {
            self.notice
                .give(tf!("Could not name this window: {why}", why));
        }
    }

    /// The text dialog was answered with `text`, trimmed.
    pub(crate) fn act_on_prompt_answer(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if let Prompt::WindowTitle = prompt {
            return self.set_window_title(ctx, text);
        }
        if let Prompt::WindowName = prompt {
            return self.name_window(ctx, text);
        }
        if let Prompt::NewCollection = prompt {
            // The images it asked about, wherever a refresh put them, not the selection now.
            let Some(found) = self.asked_targets() else {
                return;
            };
            if !self.select_for_action(ctx, found.into_iter().collect()) {
                return;
            }
            let added_to = collections::CollectionAddedTo::New(text.to_owned());
            return self.add_to_collection(ctx, added_to);
        }
        if let Prompt::RenameCollection = prompt {
            return self.rename_collection(ctx, text);
        }
        let hidden_shown = matches!(prompt, Prompt::SaveTheme) || self.shown_types().hidden;
        if let Some(why) = crate::model::listing::refused_name(text, hidden_shown) {
            return self.notice.give(why);
        }
        if let Prompt::SaveTheme = prompt {
            return self.save_theme(ctx, text);
        }
        if let Prompt::RenameFolder = prompt {
            return self.rename_folder(ctx, text);
        }
        if matches!(prompt, Prompt::Rename | Prompt::RenameMany) {
            let Some(found) = self.asked_targets() else {
                return;
            };
            match prompt {
                Prompt::Rename => self.show_file(ctx, found[0]),
                _ if !self.select_for_action(ctx, found.into_iter().collect()) => return,
                _ => {}
            }
        }
        if matches!(prompt, Prompt::Rename)
            && !self.list.files().is_empty()
            && !self.are_free_to_change(&[self.path(self.list.shown())])
        {
            return;
        }
        // A renamed file stays in its own folder, also a subfolder in the list with subfolders.
        let target = match prompt {
            Prompt::Rename if !self.list.files().is_empty() => {
                self.path(self.list.shown()).with_file_name(text)
            }
            Prompt::NewFolder => match self.dialogs.take_folder() {
                Some(dir) => dir.join(text),
                None => return,
            },
            _ => self.folder.dir().join(text),
        };
        match prompt {
            // In the background, as a share that stopped answering would hold the window until
            // create_dir returns; `folder_made` reads the folder again.
            Prompt::NewFolder => {
                let report_tx = self.file_operations.report_sender();
                self.run_in_background(ctx, Vec::new(), move || {
                    if let Err(e) = std::fs::create_dir(&target) {
                        return Some(format!("{display}: {e}", display = target.display()));
                    }
                    let _ = report_tx.send(OperationReport {
                        folder_made: Some(target),
                        ..Default::default()
                    });
                    None
                });
            }
            Prompt::Rename if self.list.files().is_empty() => {}
            Prompt::Rename if target == self.path(self.list.shown()) => {}
            Prompt::RenameMany => self.rename_many(ctx, text),
            Prompt::NewCollection
            | Prompt::RenameCollection
            | Prompt::SaveTheme
            | Prompt::RenameFolder
            | Prompt::WindowTitle
            | Prompt::WindowName => {}
            // As Rename Many: in its thread, never over a file or another file's sidecar
            // there; `renamed` puts the list right.
            Prompt::Rename => {
                self.rename_in_background(ctx, vec![self.path(self.list.shown())], vec![target]);
            }
        }
    }

    /// Renames cut short by a crash or a power cut: put the old names back, or finish them, in
    /// the background; Later asks again at the next start.
    pub(crate) fn cut_short_dialog(&mut self, ctx: &egui::Context) {
        let cut_short = self.file_operations.cut_short();
        if cut_short.is_empty() {
            return;
        }
        let files: usize = cut_short.iter().map(|c| c.1.len()).sum();
        let folder = (cut_short.iter())
            .find_map(|c| c.1.first())
            .and_then(|m| m.0.parent())
            .map_or(String::new(), |p| p.display().to_string());
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("cut short")).show(ctx, |ui| {
            ui.strong(tr("Rename Many was cut short"));
            ui.label(tf!(
                "A crash or a power cut stopped it in {folder}. Some files may have their new \
                 names, some their old ones, and some hidden temporary names ({steps} to put \
                 right).",
                folder,
                steps = count(files, "step")
            ));
            ui.horizontal(|ui| {
                if crate::ui::menus::button(ui, tr("Put the old names back")).clicked() {
                    choice = Some(true);
                }
                if crate::ui::menus::button(ui, tr("Finish the renaming")).clicked() {
                    choice = Some(false);
                }
                if crate::ui::menus::button(ui, tr("Later")).clicked() {
                    choice = None;
                    self.file_operations.take_cut_short();
                }
            });
        });
        let Some(undo) = choice else { return };
        // Asked before the plans are taken: one held back is asked about again.
        let held_back_files: Vec<PathBuf> = (self.file_operations.cut_short().iter())
            .flat_map(|(_, moves, _)| moves.iter().flat_map(|(a, b)| [a.clone(), b.clone()]))
            .collect();
        if !self.are_free_to_change(&held_back_files) {
            return;
        }
        let plans = self.file_operations.take_cut_short();
        let report_tx = self.file_operations.report_sender();
        self.run_in_background(ctx, held_back_files, move || {
            let (mut renamed, mut left) = (0, 0);
            let mut moved = Vec::new();
            for (plan, moves, done) in &plans {
                // Put right by another window: nothing done here, nothing to tell.
                let Some((r, l)) = journal::repair_cut_short(plan, moves, *done, undo) else {
                    continue;
                };
                (renamed, left) = (renamed + r, left + l);
                // Finished: marks and collections follow the files, as after a Rename Many.
                if !undo && l == 0 {
                    moved.extend(journal::pairs(moves));
                }
            }
            // One report when the steps are done, which also reads the folder again: read
            // before, the list would show the names of the middle.
            let _ = report_tx.send(OperationReport {
                read_folder_again: true,
                moved,
                ..Default::default()
            });
            let files = count(renamed, "file");
            let mut msg = match undo {
                true => tf!("{files} given their old names back", files),
                false => tf!("{files} renamed", files),
            };
            if left > 0 {
                let files = count(left, "file");
                msg += &tf!(
                    "; {files} could not be, asked again at the next start",
                    files
                );
            }
            Some(msg)
        });
    }

    /// The user name, domain and password for a network folder that asks for them.
    pub(crate) fn login_dialog(&mut self, ctx: &egui::Context) {
        if let Some(mut login) = self.folder.take_login() {
            let mut connect = false;
            let mut password = None;
            let modal = egui::Modal::new(egui::Id::unique("asks login")).show(ctx, |ui| {
                ui.label(tf!("Log in to {uri}", uri = login.uri));
                egui::Grid::new("login fields").show(ui, |ui| {
                    // Sized outright: in the grid of a modal, desired_width left them a few
                    // letters wide.
                    let size = egui::vec2(220.0, ui.spacing().interact_size.y);
                    let field = |text| egui::TextEdit::singleline(text);
                    ui.label(tr("User:"));
                    ui.add_sized(size, field(&mut login.user));
                    ui.end_row();
                    ui.label(tr("Domain:"));
                    ui.add_sized(size, field(&mut login.domain));
                    ui.end_row();
                    ui.label(tr("Password:"));
                    let edit = ui.add_sized(size, field(&mut login.password).password(true));
                    // The password field first, as the rest has defaults.
                    if login.password.is_empty() {
                        password = Some(edit.id);
                    }
                    connect = entered(&edit);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    connect |= crate::ui::menus::button(ui, tr("Connect (Enter)")).clicked();
                    if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if connect {
                self.mount_as(ctx, login, true);
            } else if !modal.should_close() {
                self.folder.ask_login(login);
                focus_on_open(ctx, password);
            }
        }
    }

    /// Returns what the delete dialog names: the one file it asks about, or how many. These are
    /// the files asked about, not the selection now, which a refresh or a script may have
    /// changed while the dialog was open.
    fn delete_dialog_subject(&self) -> String {
        match self.dialogs.asked_names() {
            [name] => format!("\"{}\"", self.name_shown(name)),
            many => count(many.len(), "file"),
        }
    }

    /// Asks before files go to the trash or are deleted for good.
    pub(crate) fn confirm_delete_dialog(&mut self, ctx: &egui::Context) {
        if let Some(trash) = self.dialogs.confirm_delete {
            let what = self.delete_dialog_subject();
            let left_out = self.dialogs.asked_left_out().map(str::to_owned);
            let mut delete = false;
            let now = ctx.input(|i| i.time);
            let shown = *self.dialogs.delete_dialog_opened_at.get_or_insert(now);
            let enter_answers =
                self.dialogs.trash_refused.is_none() || now - shown >= ENTER_IGNORED_FOR;
            let enter = enter_answers && self.answers(ctx, egui::Key::Enter);
            let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
                if trash {
                    ui.label(tf!("Move {what} to the trash?", what));
                } else {
                    if let Some(kind) = &self.dialogs.trash_refused {
                        ui.label(tf!(
                            "The trash does not take files in this folder ({kind}), as on network and cloud folders.",
                            kind
                        ));
                    }
                    ui.label(tf!("Permanently delete {what}?", what));
                    ui.weak(tr("This cannot be undone."));
                }
                if let Some(left_out) = &left_out {
                    crate::support::test_hooks::noted(&ui.weak(left_out), left_out);
                }
                ui.horizontal(|ui| {
                    delete = crate::ui::menus::button(ui, tr("Delete (Enter)")).clicked() || enter;
                    if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if !enter_answers {
                // The frame that lets Enter answer comes without another event.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(ENTER_IGNORED_FOR));
            }
            if delete {
                self.dialogs.confirm_delete = None;
                if let Some(targets) = self.asked_targets() {
                    self.delete_now(ctx, trash, targets);
                }
            } else if modal.should_close() {
                self.dialogs.confirm_delete = None;
            }
        }
    }

    /// Asks before a folder goes to the trash.
    pub(crate) fn confirm_folder_dialog(&mut self, ctx: &egui::Context) {
        if self.dialogs.is_confirming_folder_trash {
            let name = (self.dialogs.folder_target.as_deref())
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut trash = false;
            let enter = self.answers(ctx, egui::Key::Enter);
            let modal =
                egui::Modal::new(egui::Id::unique("is confirming folder trash")).show(ctx, |ui| {
                    ui.label(tf!(
                        "Move the folder \"{name}\" and all in it to the trash?",
                        name
                    ));
                    ui.horizontal(|ui| {
                        trash = crate::ui::menus::button(ui, tr("Move to Trash (Enter)")).clicked()
                            || enter;
                        if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                            ui.close();
                        }
                    });
                });
            if trash {
                self.dialogs.is_confirming_folder_trash = false;
                self.trash_folder(ctx);
            } else if modal.should_close() {
                self.dialogs.is_confirming_folder_trash = false;
                self.dialogs.folder_target = None;
            }
        }
    }

    /// Asks before a collection's file goes to the trash, as before a folder. Its images stay.
    pub(crate) fn confirm_collection_dialog(&mut self, ctx: &egui::Context) {
        let Some(file) = self.dialogs.confirm_collection.clone() else {
            return;
        };
        // A collection is shown by its name without ".gqv".
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        let question = tf!(
            "Move the collection \"{name}\" to the trash? Its images stay where they are.",
            name
        );
        let mut trash = false;
        let enter = self.answers(ctx, egui::Key::Enter);
        let modal = egui::Modal::new(egui::Id::unique("confirm-collection")).show(ctx, |ui| {
            ui.label(question.as_str());
            ui.horizontal(|ui| {
                trash =
                    crate::ui::menus::button(ui, tr("Move to Trash (Enter)")).clicked() || enter;
                if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                    ui.close();
                }
            });
        });
        if trash || modal.should_close() {
            self.dialogs.confirm_collection = None;
        }
        if trash {
            self.trash_collection(ctx, file);
        }
    }

    /// Asks before Open With Default App opens more files than it opens without asking, as
    /// Nautilus asks, because each file starts a program of its own.
    pub(crate) fn confirm_open_dialog(&mut self, ctx: &egui::Context) {
        let n = self.dialogs.files_to_open.len();
        if n == 0 {
            return;
        }
        let question = tf!(
            "Open {files}, each in its default application?",
            files = count(n, "file")
        );
        let mut open = false;
        let enter = self.answers(ctx, egui::Key::Enter);
        let modal = egui::Modal::new(egui::Id::unique("files-to-open")).show(ctx, |ui| {
            ui.label(question.as_str());
            ui.horizontal(|ui| {
                open = crate::ui::menus::button(ui, tr("Open (Enter)")).clicked() || enter;
                if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                    ui.close();
                }
            });
        });
        if open || modal.should_close() {
            let files = std::mem::take(&mut self.dialogs.files_to_open);
            if open {
                self.open_in("", files);
            }
        }
    }

    /// Returns whether Preferences or a modal dialog is open. The keys then go there, not to
    /// the shortcuts.
    pub(crate) fn dialog_open(&self) -> bool {
        self.preferences.is_open() || self.modal_open()
    }

    /// Returns whether a modal dialog is open, which takes Esc before anything under it. This
    /// is the one list of them, and Preferences leaves Esc to it.
    pub(crate) fn modal_open(&self) -> bool {
        self.dialogs.confirm_delete.is_some()
            || self.dialogs.confirm_collection.is_some()
            || self.dialogs.is_confirming_folder_trash
            || self.dialogs.prompt.is_some()
            || self.folder.asks_login()
            || self.print_dialog.is_some()
            || self.lightroom.is_some()
            || self.save_as.is_some()
            || self.dialogs.copy_for_sharing.is_some()
            || !self.dialogs.files_to_open.is_empty()
            || !self.file_operations.cut_short().is_empty()
    }

    /// Returns whether `key`, Enter or Esc, gives the default answer of what is open, and takes
    /// the key when it does. Only a new press answers, not the repeat of a key held down. It
    /// answers only while no widget has the keyboard focus, nor had it as the last frame ended,
    /// and only without Ctrl or Alt, which leave the key to the user's own shortcuts.
    /// A button with the focus then answers with its own click, as egui clicks it on Enter. A
    /// text field keeps its keys too: egui takes its focus away at the start of the frame Esc
    /// is pressed in, so only the last frame's focus shows that Esc was meant for the field.
    /// The image area's focus does not count, because it holds the focus for a screen reader.
    /// Every dialog and window reads its Enter or Esc here, never from the input as a whole
    /// (`source_rules::keys_answer_through_one_place`).
    pub(crate) fn answers(&self, ctx: &egui::Context, key: egui::Key) -> bool {
        if self.focus_held(ctx) {
            return false;
        }
        ctx.input_mut(|i| {
            let new = |e: &egui::Event| {
                matches!(e, egui::Event::Key { key: k, pressed: true, repeat: false, modifiers, .. }
                    if *k == key && !modifiers.ctrl && !modifiers.alt && !modifiers.command)
            };
            let at = i.events.iter().position(new);
            at.map(|at| i.events.remove(at)).is_some()
        })
    }

    /// Returns whether a widget has the keyboard focus, so that the keys of this frame are its
    /// own. egui takes a text field's focus away at the start of the frame Esc is pressed in,
    /// and the field gives it up as it reads Enter, so for a frame with an Enter or Esc press
    /// the focus the last frame ended with counts too. It counts only for those keys, and only
    /// when egui says it held the focus then: a widget gone from the window keeps the focus
    /// until egui notices at the end of a frame, and the next key would be lost to it. The
    /// image area's focus does not count, because it holds the focus for a screen reader.
    /// `answers` and the shortcuts in `read_keys` both ask here, so a key is never the
    /// widget's for one of them and free for the other.
    pub(crate) fn focus_held(&self, ctx: &egui::Context) -> bool {
        let left = |e: &egui::Event| {
            matches!(
                e,
                egui::Event::Key {
                    key: egui::Key::Enter | egui::Key::Escape,
                    pressed: true,
                    ..
                }
            )
        };
        let before = ctx
            .data(|d| d.get_temp::<Option<egui::Id>>(focus_before()))
            .flatten()
            .filter(|&id| ctx.memory(|m| m.had_focus_last_frame(id)))
            .filter(|_| ctx.input(|i| i.events.iter().any(left)));
        let now = ctx.memory(|m| m.focused());
        [before, now]
            .into_iter()
            .flatten()
            .any(|f| Some(f) != self.image_area_id)
    }

    /// Notes the widget with the keyboard focus as the frame ends, for `answers` in the next.
    pub(crate) fn note_focus(&self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused());
        ctx.data_mut(|d| d.insert_temp(focus_before(), focused));
    }
}

/// Gives the text field `field` of a dialog the keyboard focus while no widget has it, as the
/// dialog opens. It is called only for a dialog that stays open: focus given in the frame it
/// closes would stay on a field that is gone, and take the next key.
fn focus_on_open(ctx: &egui::Context, field: Option<egui::Id>) {
    if let Some(id) = field
        && ctx.memory(|m| m.focused().is_none())
    {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

/// Returns the key in egui's memory under which `note_focus` keeps the last frame's focus.
fn focus_before() -> egui::Id {
    egui::Id::unique("glimt focus before this frame")
}

/// Returns whether Enter ended the typing in the text field `field` this frame. A field's own
/// Enter is the one Enter read beside `App::answers`, as the field lets the focus go with it.
pub(crate) fn entered(field: &egui::Response) -> bool {
    field.lost_focus() && field.ctx.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Returns a warning for Rename Many's preview when `name`, the first new name, has a
/// character that the FAT or exFAT file system `fat` of its folder refuses. Returns `None` when
/// the folder is on another file system, or when the name is allowed there.
fn fat_warning(name: &std::ffi::OsStr, fat: Option<&str>) -> Option<String> {
    let kind = fat.filter(|_| name.to_string_lossy().contains(NOT_ON_FAT))?;
    Some(tf!(
        "A {kind} file system, as on a camera card, allows none of : ? * \" < > | \\ in a name.",
        kind
    ))
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;
    use eframe::egui;

    /// A name that a window cannot take is told in a text of its own, which is translated,
    /// and not with the English reason that `--remote name` prints. The name is kept as it was.
    #[test]
    fn a_window_name_refused_is_told_in_the_users_language() {
        let dir = std::env::temp_dir().join(format!("glimt-test-name-told-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.name_window(&ctx, "4242");
        let told = "Could not name this window: a name holds up to 64 letters, digits, - and _, \
                    and not only digits";
        assert_eq!(app.notice.text(), Some(told));
        assert!(include_str!("../../po/no_nb.po").contains(told));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Open With Default App asks before it opens more than five files, each in a program of
    /// its own, and Esc opens none of them. (Five or fewer would start the programs, so they
    /// are not tried here.)
    #[test]
    fn opening_many_files_in_their_default_applications_asks_first() {
        use crate::ui::actions::Action;
        let dir = std::env::temp_dir().join(format!("glimt-test-open-many-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            image::RgbImage::new(2, 2)
                .save(dir.join(format!("{i}.png")))
                .unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.run(&ctx, Action::SelectAll);
        app.run(&ctx, Action::OpenWith);
        assert_eq!(app.dialogs.files_to_open.len(), 6);
        assert!(app.modal_open());
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.confirm_open_dialog(ui.ctx())
        });
        assert_eq!(app.dialogs.files_to_open.len(), 6);
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let input = egui::RawInput {
            events: vec![esc],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| app.confirm_open_dialog(ui.ctx()));
        assert!(app.dialogs.files_to_open.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename and Rename Many refuse a new name that starts with a dot, which the list hides,
    /// so the files would vanish from it. Before, `.#` renamed them to `.1.png` and `.2.png`,
    /// gone from the list. With hidden files shown, such a name is taken.
    #[test]
    fn a_new_name_that_would_be_hidden_is_refused() {
        use super::Prompt;
        use crate::ui::actions::Action;
        let dir = std::env::temp_dir().join(format!("glimt-test-dot-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let told = |name: &str| {
            format!("{name} starts with a dot and would be hidden: choose another name")
        };
        app.run(&ctx, Action::Rename);
        app.act_on_prompt_answer(&ctx, Prompt::Rename, ".a.png");
        assert_eq!(app.notice.text(), Some(told(".a.png").as_str()));
        app.notice.clear();
        app.run(&ctx, Action::SelectAll);
        app.run(&ctx, Action::Rename);
        app.act_on_prompt_answer(&ctx, Prompt::RenameMany, ".#");
        assert_eq!(app.notice.text(), Some(told(".#").as_str()));
        assert!(dir.join("a.png").exists() && dir.join("b.png").exists());
        assert!(include_str!("../../po/no_nb.po").contains(&told("{text}")));
        assert!(crate::model::listing::refused_name(".a.png", true).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename Many's preview warns when the first new name has a character that the camera card
    /// of the files refuses. The card's file system is looked up once, in the background, as the
    /// dialog opens.
    #[test]
    fn rename_many_warns_of_what_fat_refuses() {
        use crate::files::file_ops::test_mount;
        use std::ffi::OsStr;
        let d = std::env::temp_dir().join(format!("glimt-test-fat-many-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&d);
        let listed = |a: &crate::App| !a.list.files().is_empty();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, listed
        ));
        // The file system is looked up in the background.
        let answered = |app: &mut crate::App| {
            (0..250).any(|_| {
                app.dialogs.receive_fat_file_system_type();
                let done = app.dialogs.fat_file_system_answer.is_none();
                if !done {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                done
            })
        };
        test_mount(&d, Some("ext4"));
        let asked = app.ask_about_targets().unwrap();
        app.ask_rename_many(asked);
        assert!(answered(&mut app));
        assert_eq!(app.dialogs.fat_file_system_type, None);
        test_mount(&d, Some("vfat"));
        let asked = app.ask_about_targets().unwrap();
        app.ask_rename_many(asked);
        assert!(answered(&mut app));
        test_mount(&d, None);
        let fat = app.dialogs.fat_file_system_type.as_deref();
        assert_eq!(fat, Some("vfat"));
        let told = super::fat_warning(OsStr::new("12:30_001.png"), fat).unwrap_or_default();
        assert!(told.contains("vfat"), "{told}");
        assert_eq!(super::fat_warning(OsStr::new("a_001.png"), fat), None);
        assert_eq!(super::fat_warning(OsStr::new("12:30_001.png"), None), None);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The question to delete for good what the trash refused opens by itself, so an Enter in
    /// its first moment, meant for something else, does not answer it. Nor does the repeat of
    /// an Enter held down since then. A new press of Enter answers it once it has been on
    /// screen for a moment.
    #[test]
    fn an_enter_as_the_trash_refused_question_opens_does_not_delete() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-enter-after-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        // Only whether the dialog answers is tested here: what it then deletes is
        // file_ops.rs's `asks_to_delete_for_good_what_the_trash_refused_on_a_network_mount`.
        let asked = super::Asked::new(0, vec!["a.png".into()], None);
        app.dialogs
            .confirm_delete_instead(asked, "fuse.rclone".into());
        // Each key is (pressed, repeat). egui takes a press of a key that is down as a repeat.
        let enter_at = |app: &mut crate::App, time: f64, keys: &[(bool, bool)]| {
            let key = |&(pressed, repeat)| egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed,
                repeat,
                modifiers: Default::default(),
            };
            let input = egui::RawInput {
                time: Some(time),
                events: keys.iter().map(key).collect(),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.confirm_delete_dialog(ui.ctx()));
        };
        enter_at(&mut app, 10.0, &[(true, false)]);
        assert!(
            app.dialogs.confirm_delete.is_some(),
            "the first Enter answered"
        );
        enter_at(&mut app, 10.0 + super::ENTER_IGNORED_FOR, &[(true, true)]);
        assert!(
            app.dialogs.confirm_delete.is_some(),
            "a held Enter's repeat answered"
        );
        // The Enter is let go and pressed again.
        enter_at(
            &mut app,
            10.6 + super::ENTER_IGNORED_FOR,
            &[(false, false), (true, false)],
        );
        assert!(
            app.dialogs.confirm_delete.is_none(),
            "a later Enter did not answer"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Enter on the button with the keyboard focus answers with that button. With Tab moved to
    /// "Cancel", Enter cancels and deletes nothing. Before, the dialog also read the Enter from
    /// the whole input and deleted the file for good. Enter with no focus still deletes.
    #[test]
    fn enter_on_a_focused_cancel_deletes_nothing() {
        let dir = std::env::temp_dir().join(format!("glimt-test-focus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let listed = |a: &crate::App| !a.list.files().is_empty();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, listed
        ));
        // Each key is pressed and let go in one frame. None is a frame without a key.
        let press = |app: &mut crate::App, key: Option<egui::Key>| {
            let event = |key, pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            let input = egui::RawInput {
                events: key.map_or(vec![], |k| vec![event(k, true), event(k, false)]),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                app.confirm_delete_dialog(ui.ctx());
                app.note_focus(ui.ctx());
            });
        };
        let done = |a: &crate::App| !a.working_besides_image();
        app.run(&ctx, crate::ui::actions::Action::DeletePermanently);
        // The dialog is drawn once, then Tab moves to "Delete" and to "Cancel".
        for key in [
            None,
            Some(egui::Key::Tab),
            Some(egui::Key::Tab),
            Some(egui::Key::Enter),
        ] {
            press(&mut app, key);
        }
        assert!(
            app.dialogs.confirm_delete.is_none(),
            "Cancel did not answer"
        );
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, done
        ));
        assert!(
            dir.join("a.png").exists(),
            "Enter on Cancel deleted the file"
        );
        // The focus goes with the button once the dialog is no longer drawn.
        press(&mut app, None);
        press(&mut app, None);
        app.run(&ctx, crate::ui::actions::Action::DeletePermanently);
        press(&mut app, Some(egui::Key::Enter));
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, done
        ));
        assert!(
            !dir.join("a.png").exists(),
            "Enter with no focus did not delete"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Esc in a text field only leaves the field. egui takes the field's focus away before the
    /// frame starts, so the window under it (Check Images, a crop, the pan view) was closed too.
    /// The next Esc, with nothing focused, answers.
    #[test]
    fn esc_in_a_text_field_only_leaves_it() {
        let dir = std::env::temp_dir().join(format!("glimt-test-esc-field-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (app, ctx) = app_on(&dir);
        let field = egui::Id::unique("a text field");
        let esc = |focus: bool| {
            let event = |pressed| egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            let input = egui::RawInput {
                events: vec![event(true), event(false)],
                ..Default::default()
            };
            let mut answered = false;
            let _ = ctx.run_ui(input, |ui| {
                answered = app.answers(ui.ctx(), egui::Key::Escape);
                if focus {
                    ui.memory_mut(|m| m.request_focus(field));
                }
                app.note_focus(ui.ctx());
            });
            answered
        };
        esc(true);
        assert!(!esc(false), "Esc that left a text field answered");
        assert!(esc(false), "Esc with nothing focused did not answer");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Enter with Ctrl or Alt is no answer, and stays for the user's own shortcut on it. The
    /// crop and the grid's Enter read only a key without them before.
    #[test]
    fn enter_with_ctrl_or_alt_is_no_answer() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-ctrl-enter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (app, ctx) = app_on(&dir);
        let enter = |modifiers: egui::Modifiers| {
            let event = |pressed| egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            let input = egui::RawInput {
                events: vec![event(true), event(false)],
                ..Default::default()
            };
            let mut answered = false;
            let _ = ctx.run_ui(input, |ui| {
                answered = app.answers(ui.ctx(), egui::Key::Enter);
                app.note_focus(ui.ctx());
            });
            answered
        };
        assert!(!enter(egui::Modifiers::CTRL), "Ctrl+Enter answered");
        assert!(!enter(egui::Modifiers::ALT), "Alt+Enter answered");
        assert!(enter(egui::Modifiers::SHIFT), "Shift+Enter did not answer");
        assert!(enter(egui::Modifiers::NONE), "Enter did not answer");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename, and Rename Folder, answered with the name they opened with rename nothing. The
    /// name shown is not the name on disk when it has bytes that are not UTF-8 or a space at
    /// its start or end, and the file was renamed to the name shown.
    #[test]
    fn an_unchanged_name_renames_nothing() {
        use std::os::unix::ffi::OsStrExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-same-{}", std::process::id()));
        let latin1 = std::ffi::OsStr::from_bytes(b"caf\xe9.png");
        for name in [latin1, " holiday.png".as_ref()] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("trip ")).unwrap();
            image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
            std::fs::rename(dir.join("a.png"), dir.join(name)).unwrap();
            let (mut app, ctx) = app_on(&dir);
            let listed = |a: &crate::App| !a.list.files().is_empty();
            assert!(crate::support::test_support::run_background_until(
                &mut app, &ctx, listed
            ));
            app.run(&ctx, crate::ui::actions::Action::Rename);
            app.dialogs.ask_rename_folder(&dir.join("trip "));
            let (prompt, text) = app.dialogs.prompt.take().unwrap();
            app.take_prompt_answer(&ctx, prompt, &text);
            let done = |a: &crate::App| !a.working_besides_image();
            assert!(crate::support::test_support::run_background_until(
                &mut app, &ctx, done
            ));
            assert!(dir.join("trip ").is_dir(), "the folder was renamed");
            app.run(&ctx, crate::ui::actions::Action::Rename);
            let (prompt, text) = app.dialogs.prompt.take().unwrap();
            app.take_prompt_answer(&ctx, prompt, &text);
            assert!(crate::support::test_support::run_background_until(
                &mut app, &ctx, done
            ));
            assert!(dir.join(name).is_file(), "{name:?} was renamed");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// New Collection selects the files it asked about through set_selection, so what follows
    /// a selection (a Shift range's end ends) happens too, not the selection alone.
    #[test]
    fn new_collection_selects_what_it_asked_about() {
        let dir = std::env::temp_dir().join(format!("glimt-test-new-col-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.list.select([0, 1].into());
        app.run(&ctx, crate::ui::actions::Action::NewCollection);
        app.list.set_range_end(Some(2));
        // A name of its own, so a run never finds the collection of the run before.
        let name = format!("trip-{}", std::process::id());
        app.act_on_prompt_answer(&ctx, super::Prompt::NewCollection, &name);
        assert_eq!(
            app.list.selected().iter().copied().collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(app.list.range_end(), None);
        // When a search hid one of the files meanwhile, the answer makes nothing, as it would
        // act on the files left or on the shown image instead. It says so truly, and the
        // selection stays as it was.
        app.run(&ctx, crate::ui::actions::Action::NewCollection);
        app.list.set_matches(Some(vec![1, 2]));
        let other = format!("{name}-hidden");
        app.notice.clear();
        app.act_on_prompt_answer(&ctx, super::Prompt::NewCollection, &other);
        let hides =
            crate::support::lang::tr("The search hides files this would act on: clear it first");
        assert_eq!(app.notice.text(), Some(hides));
        assert_eq!(app.list.selected_with_hidden(), [0, 1].into());
        let dir_of = crate::model::collections::collections_dir().unwrap();
        assert!(!dir_of.join(format!("{other}.gqv")).exists());
        app.list.set_matches(None);
        // The collection is written in the background, and removed once it is.
        let file = crate::model::collections::collections_dir()
            .unwrap()
            .join(format!("{name}.gqv"));
        let written = |a: &crate::App| !a.working_besides_image();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, written
        ));
        // A name taken already is told too, though it is looked up in the background.
        app.run(&ctx, crate::ui::actions::Action::NewCollection);
        app.act_on_prompt_answer(&ctx, super::Prompt::NewCollection, &name);
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, written
        ));
        let taken = crate::tf!("There is a collection called \"{name}\" already", name);
        assert_eq!(app.notice.text(), Some(taken.as_str()));
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The delete dialog names the files it asked about, also after the selection changed
    /// while it was open, and its answer acts on those files.
    #[test]
    fn the_delete_dialog_names_the_files_it_asked_about() {
        let dir = std::env::temp_dir().join(format!("glimt-test-asked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.list.select([0].into());
        app.config.confirm_trash = true;
        app.request_delete(&ctx, true);
        app.list.select([1, 2].into());
        assert_eq!(app.delete_dialog_subject(), "\"a.png\"");
        assert_eq!(app.asked_targets(), Some(vec![0]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A frame and a PDF are saved beside their image, also in a collection, whose folder is
    /// not the image's.
    #[test]
    fn a_frame_is_saved_beside_its_image() {
        let d = std::env::temp_dir().join(format!("glimt-test-frame-start-{}", std::process::id()));
        let images = d.join("images");
        std::fs::create_dir_all(&images).unwrap();
        let image = images.join("a.png");
        image::RgbImage::new(4, 4).save(&image).unwrap();
        let list = d.join("trip.gqv");
        let entry = format!("#GQview collection\n\"{}\"\n#end\n", image.display());
        std::fs::write(&list, entry).unwrap();
        let (mut app, ctx) = app_on(&d);
        crate::support::test_support::open_now(&mut app, &ctx, list);
        let how = app.shown_copy();
        let start = app.dialog_start(&super::Pick::SaveFrame(image.clone(), 2, how));
        assert_eq!(start, images.join("a-frame-3.png"));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Draws one frame as the window does, with `key` pressed and let go in it, and `draw` for
    /// the windows the test needs. Returns the action the keys ran.
    fn frame(
        app: &mut crate::App,
        ctx: &egui::Context,
        key: Option<egui::Key>,
        mut draw: impl FnMut(&mut crate::App, &egui::Context),
    ) -> Option<crate::ui::actions::Action> {
        let event = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        let input = egui::RawInput {
            events: key.map_or(vec![], |k| vec![event(k, true), event(k, false)]),
            ..Default::default()
        };
        let mut action = None;
        let _ = ctx.run_ui(input, |ui| {
            action = app.read_keys(ui.ctx());
            draw(app, ui.ctx());
            app.note_focus(ui.ctx());
        });
        action
    }

    /// A key after the widget with the focus is gone runs its shortcut. The last frame's focus
    /// counted for every key, and egui lets a widget gone keep the focus to the end of a frame,
    /// so the first key after a dialog closed was lost.
    #[test]
    fn a_key_after_the_focused_widget_is_gone_runs() {
        use crate::ui::actions::Action;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-focus-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let field = egui::Id::unique("a field that is drawn once");
        frame(&mut app, &ctx, None, |_, ctx| {
            ctx.memory_mut(|m| m.request_focus(field))
        });
        frame(&mut app, &ctx, None, |_, _| {});
        let key = Some(egui::Key::ArrowRight);
        assert_eq!(
            frame(&mut app, &ctx, key, |_, _| {}),
            Some(Action::Next),
            "the key after the focused widget was gone did nothing"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Esc in the Rename prompt closes it, and the next key runs its shortcut. The prompt gave
    /// its field the focus again in the frame it closed in, so the field took the next key.
    #[test]
    fn the_key_after_esc_closed_a_prompt_runs() {
        use crate::ui::actions::Action;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-prompt-esc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let listed = |a: &crate::App| !a.list.files().is_empty();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, listed
        ));
        app.run(&ctx, Action::Rename);
        let prompt = |app: &mut crate::App, ctx: &egui::Context| app.prompt_dialog(ctx);
        frame(&mut app, &ctx, None, prompt);
        assert_eq!(frame(&mut app, &ctx, Some(egui::Key::Escape), prompt), None);
        assert!(app.dialogs.prompt.is_none(), "Esc did not close the prompt");
        let key = Some(egui::Key::ArrowRight);
        assert_eq!(
            frame(&mut app, &ctx, key, prompt),
            Some(Action::Next),
            "the key after Esc closed the prompt did nothing"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
