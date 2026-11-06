//! The file chooser (zenity or kdialog) and the small dialogs: a name asked for, a delete
//! confirmed, a share's login, a job cut short.

use crate::features::print::{PrintJob, PrintTo};
use crate::files::config::xdg_dir;
use crate::files::external::{Login, command};
use crate::files::file_ops::{Deleted, NOT_ON_FAT, fat_type};
use crate::model::listing::pattern_name;
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took};
use crate::{files::external, files::journal, images::save, model::collections, tf, ui::theme};
use eframe::egui;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// How long the question to delete for good what the trash refused is on screen before Enter
/// answers it. It opens by itself, when the folder has been read again, so an Enter pressed for
/// something else at that moment must not delete files for good unseen.
const ENTER_AFTER: f64 = 0.5;

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
    /// Where to save frame (or page) number .1 of the file .0, as PNG.
    SaveFrame(PathBuf, usize),
    /// Save As: where to save the copy .1 of the file .0, starting at the name .2.
    SaveAs(PathBuf, save::CopyAs, PathBuf),
    /// The folder for every frame (or page) of the file, as PNG files.
    ExportFrames(PathBuf),
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
}

/// The small dialogs: a name asked for, a delete or a folder's trashing confirmed, and the file
/// chooser.
#[derive(Default)]
pub(crate) struct Dialogs {
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm_delete: Option<bool>,
    /// The type of the network or cloud mount whose files the trash refused, when the delete
    /// for good asked about is instead of a move to the trash.
    trash_refused: Option<String>,
    /// When the delete dialog was first drawn, in egui's input time.
    delete_shown: Option<f64>,
    /// What a folder or collection dialog acts on, kept from when it asked: the folder to rename
    /// or, with `confirm_folder`, to trash, the folder New Folder makes its folder in, or the
    /// collection file to rename. A script that opens another folder or collection while the
    /// dialog is open does not change it.
    folder_target: Option<PathBuf>,
    confirm_folder: bool,
    /// The collection file whose move to the trash is being confirmed. It is kept from when
    /// Move Collection to Trash asked, so the answer acts on that collection.
    confirm_collection: Option<PathBuf>,
    /// Answer from an open file dialog (zenity or kdialog).
    file_dialog: Option<Pick>,
    file_dialog_rx: Receiving<Option<PathBuf>>,
    /// The files a dialog asks about, by their names in the list, and where the list was
    /// (`ask_about_targets`). The dialog names them, and its answer acts on them, wherever a
    /// refresh put them.
    asked_for: (u64, Vec<OsString>),
    /// The type of the FAT or exFAT file system that Rename Many renames on, looked up once as
    /// the dialog opens, so that its preview can warn without reading the mount table in every
    /// frame. It is `None` on any other file system.
    fat: Option<String>,
}

impl Dialogs {
    /// Opens the text dialog `prompt`, with `text` typed in already.
    pub(crate) fn ask(&mut self, prompt: Prompt, text: String) {
        self.prompt = Some((prompt, text));
    }

    /// Asks whether to delete the files: to the trash when `trash`, else for good.
    pub(crate) fn confirm_delete(&mut self, trash: bool) {
        self.confirm_delete = Some(trash);
        self.trash_refused = None;
        self.delete_shown = None;
    }

    /// Asks whether to delete the files for good, because the trash refused them on a network
    /// or cloud mount of type `kind`.
    pub(crate) fn confirm_delete_instead(&mut self, kind: String) {
        self.confirm_delete = Some(false);
        self.trash_refused = Some(kind);
        self.delete_shown = None;
    }

    /// Whether Rename or a delete is asking about the files on screen.
    pub(crate) fn asks_about_files(&self) -> bool {
        self.prompt.is_some() || self.confirm_delete.is_some()
    }

    /// The files asked about, by their names, and where the list was when asked.
    pub(crate) fn asked_about(&mut self, place: u64, names: Vec<OsString>) {
        self.asked_for = (place, names);
    }

    /// Returns the names of the files asked about, which the dialog names while it is open.
    pub(crate) fn asked_names(&self) -> &[OsString] {
        &self.asked_for.1
    }

    /// The files asked about, taken for the answer to act on.
    pub(crate) fn take_asked(&mut self) -> (u64, Vec<OsString>) {
        std::mem::take(&mut self.asked_for)
    }

    /// Asks for a new name for the folder `dir`.
    pub(crate) fn ask_rename_folder(&mut self, dir: &Path) {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        self.prompt = Some((Prompt::RenameFolder, name.into_owned()));
        self.folder_target = Some(dir.to_path_buf());
    }

    /// Asks for the name of a new folder in the folder `dir`.
    pub(crate) fn ask_new_folder(&mut self, dir: &Path) {
        self.prompt = Some((Prompt::NewFolder, String::new()));
        self.folder_target = Some(dir.to_path_buf());
    }

    /// Asks for a new name for the collection `file`.
    pub(crate) fn ask_rename_collection(&mut self, file: &Path) {
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        self.prompt = Some((Prompt::RenameCollection, name.into_owned()));
        self.folder_target = Some(file.to_path_buf());
    }

    /// Asks whether to move the collection `file` to the trash.
    pub(crate) fn ask_trash_collection(&mut self, file: &Path) {
        self.confirm_collection = Some(file.to_path_buf());
    }

    /// Asks whether to move the folder `dir` to the trash.
    pub(crate) fn ask_trash_folder(&mut self, dir: &Path) {
        self.folder_target = Some(dir.to_path_buf());
        self.confirm_folder = true;
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
    /// Asks for Rename Many's pattern for the files asked about. The file system of the first
    /// file's folder is looked up here, once, for the preview of its new name.
    pub(crate) fn ask_rename_many(&mut self) {
        let first = (self.dialogs.asked_names().first()).map(|n| self.folder.dir().join(n));
        self.dialogs.fat = first.as_deref().and_then(Path::parent).and_then(fat_type);
        self.dialogs.ask(Prompt::RenameMany, "*_###".into());
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    pub(crate) fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        // One at a time: only one answer is waited for, and a second dialog's would replace the
        // first's, whose copy or move would then never happen.
        if self.dialogs.file_dialog.is_some() {
            self.notice = Some(tr("A file dialog is open already").into());
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
            Pick::CopyTo(_) => tr("Copy to Folder"),
            Pick::MoveTo(_) => tr("Move to Folder"),
            Pick::SavePdf(_) => tr("Save as PDF"),
            Pick::SortFolder => tr("Add to the Sort Manager"),
            Pick::ScreenProfile => tr("Screen Profile"),
            Pick::InputProfile => tr("Profile of Images Without One"),
            Pick::Lightroom => tr("Import Lightroom Catalog"),
            Pick::BackUp => tr("Back Up Glimt's Data To"),
            Pick::SaveFrame(..) => tr("Save This Frame as PNG"),
            Pick::ExportFrames(_) => tr("Export Every Frame To"),
            Pick::SaveAs(..) => tr("Save As"),
        };
        let notices = self.notices_tx.clone();
        std::thread::spawn(move || {
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
            let output = match (external::has("zenity"), external::has("kdialog")) {
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
        self.dialogs.file_dialog_rx = Receiving::of(rx);
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
            Pick::SaveFrame(path, k) => {
                path.with_file_name(save::stem_and(path, &format!("-frame-{}.png", k + 1)))
            }
            Pick::SaveAs(_, _, start) => start.clone(),
            // Made first: the file dialog cannot start in a folder that is not there.
            Pick::Collection => (collections::dir())
                .inspect(|d| drop(std::fs::create_dir_all(d)))
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

    /// The file dialog's answer, for what it was opened for.
    pub(crate) fn receive_picked(&mut self, ctx: &egui::Context) {
        let picked = match self.dialogs.file_dialog_rx.take() {
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
                (Pick::CopyTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, true),
                (Pick::MoveTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, false),
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
                (Pick::SaveFrame(path, k), Some(file)) => {
                    self.export_frames(ctx, path, Some(k), file)
                }
                (Pick::ExportFrames(path), Some(dir)) => self.export_frames(ctx, path, None, dir),
                (Pick::SaveAs(path, how, _), Some(to)) => self.save_as_to(ctx, path, how, to),
            }
        }
    }

    /// A share mounted, or why not: its folder opens, or the error or a login is asked for.
    pub(crate) fn receive_mount(&mut self, ctx: &egui::Context) {
        let done = match self.folder.take_mounted() {
            Took::Nothing => return,
            Took::Gone => return self.stopped(tr("Connecting")),
            Took::Answer(done) => done,
        };
        {
            // "Connecting to …" goes: the folder, the error or the login takes its place.
            self.notice = None;
            match done {
                (Ok(path), _) => self.visit(ctx, path),
                (Err(Some(e)), _) => self.notice = Some(e),
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
                    "Title of this window (%name%, %path%, %folder%, %app%; empty: as in \
                     Preferences):",
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
            let fat = self.dialogs.fat.clone();
            let mut answer = None;
            let modal = egui::Modal::new(egui::Id::unique("prompt")).show(ctx, |ui| {
                ui.label(title);
                let edit = ui.text_edit_singleline(&mut text);
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // Focus when the dialog opens (after reading Enter, which ends the focus).
                if !enter && ui.memory(|m| m.focused().is_none()) {
                    edit.request_focus();
                }
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
                self.answer_prompt(ctx, prompt, text.trim());
            } else if !modal.should_close() {
                self.dialogs.prompt = Some((prompt, text));
            }
        }
    }

    /// Renames the collection Rename Collection asked about to `text`.
    fn rename_collection(&mut self, ctx: &egui::Context, text: &str) {
        let Some(old) = self.dialogs.take_folder() else {
            return;
        };
        match collections::rename(&old, text) {
            // The window follows only when it still shows the collection it asked about.
            Ok(new) if self.folder.collection() == Some(old.as_path()) => {
                self.folder.collection_renamed(new);
                self.title(ctx);
            }
            Ok(_) => {}
            Err(e) => self.notice = Some(e),
        }
    }

    /// The text dialog was answered with `text`.
    pub(crate) fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if let Prompt::WindowTitle = prompt {
            return self.set_window_title(ctx, text);
        }
        if let Prompt::NewCollection = prompt {
            // The images it asked about, wherever a refresh put them, not the selection now.
            let Some(found) = self.asked_targets() else {
                return;
            };
            self.set_selection(ctx, found.into_iter().collect());
            return match collections::new_file(text) {
                Ok(file) => self.add_to_collection(ctx, &file),
                Err(e) => self.notice = Some(e),
            };
        }
        if let Prompt::RenameCollection = prompt {
            return self.rename_collection(ctx, text);
        }
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(tf!("Not a valid name: \"{text}\"", text));
            return;
        }
        if let Prompt::SaveTheme = prompt {
            let file = theme::dir().map(|d| d.join(format!("{text}.theme")));
            let saved = file.ok_or(tr("no home folder").to_owned()).and_then(|f| {
                // Never over a theme there, hand-edited perhaps, nor in the place of a built-in
                // one, which a file of its name would hide for good.
                if theme::built_in(text) {
                    return Err(tf!("{text} is a built-in theme: choose another name", text));
                }
                let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
                // Whole or not at all: a full disk left half a theme, whose name was then taken.
                let made = crate::files::file_io::create_file(&f, theme::Theme::file_from(ctx));
                made.map(|_| f).map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => {
                        tf!("{text} exists already: choose another name", text)
                    }
                    _ => e.to_string(),
                })
            });
            self.notice = Some(match saved {
                Ok(f) => {
                    self.config.theme = text.to_owned();
                    self.config.save();
                    tf!(
                        "Saved as {file}: edit it, then choose it again",
                        file = f.display()
                    )
                }
                Err(e) => tf!("Could not save the theme: {e}", e),
            });
            return;
        }
        if let Prompt::RenameFolder = prompt {
            return self.rename_folder(ctx, text);
        }
        if matches!(prompt, Prompt::Rename | Prompt::RenameMany) {
            let Some(found) = self.asked_targets() else {
                return;
            };
            match prompt {
                Prompt::Rename => self.go(ctx, found[0]),
                _ => self.set_selection(ctx, found.into_iter().collect()),
            }
        }
        if matches!(prompt, Prompt::Rename)
            && !self.list.files().is_empty()
            && !self.none_saving(&[self.path(self.list.cur())])
        {
            return;
        }
        // A renamed file stays in its own folder, also a subfolder in the list with subfolders.
        let target = match prompt {
            Prompt::Rename if !self.list.files().is_empty() => {
                self.path(self.list.cur()).with_file_name(text)
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
                let deleted_tx = self.work.deleted_sender();
                self.background(ctx, Vec::new(), move || {
                    if let Err(e) = std::fs::create_dir(&target) {
                        return Some(format!("{display}: {e}", display = target.display()));
                    }
                    let _ = deleted_tx.send(Deleted {
                        folder_made: Some(target),
                        ..Default::default()
                    });
                    None
                });
            }
            Prompt::Rename if self.list.files().is_empty() => {}
            Prompt::Rename if target == self.path(self.list.cur()) => {}
            Prompt::RenameMany => self.rename_many(ctx, text),
            Prompt::NewCollection
            | Prompt::RenameCollection
            | Prompt::SaveTheme
            | Prompt::RenameFolder
            | Prompt::WindowTitle => {}
            // As Rename Many: in its thread, never over a file or another file's sidecar
            // there; `renamed` puts the list right.
            Prompt::Rename => {
                self.rename_in_background(ctx, vec![self.path(self.list.cur())], vec![target]);
            }
        }
    }

    /// Renames cut short by a crash or a power cut: put the old names back, or finish them, in
    /// the background; Later asks again at the next start.
    pub(crate) fn cut_short_dialog(&mut self, ctx: &egui::Context) {
        let cut_short = self.work.cut_short();
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
                    self.work.take_cut_short();
                }
            });
        });
        let Some(undo) = choice else { return };
        // Asked before the plans are taken: one held back is asked about again.
        let touches: Vec<PathBuf> = (self.work.cut_short().iter())
            .flat_map(|(_, moves, _)| moves.iter().flat_map(|(a, b)| [a.clone(), b.clone()]))
            .collect();
        if !self.none_saving(&touches) {
            return;
        }
        let plans = self.work.take_cut_short();
        let deleted = self.work.deleted_sender();
        self.background(ctx, touches, move || {
            let (mut renamed, mut left) = (0, 0);
            let mut moved = Vec::new();
            for (plan, moves, done) in &plans {
                // Put right by another window: nothing done here, nothing to tell.
                let Some((r, l)) = journal::settle(plan, moves, *done, undo) else {
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
            let _ = deleted.send(Deleted {
                relist: true,
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
            let modal = egui::Modal::new(egui::Id::unique("login")).show(ctx, |ui| {
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
                    if login.password.is_empty() && ui.memory(|m| m.focused().is_none()) {
                        edit.request_focus();
                    }
                    connect = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
            }
        }
    }

    /// Returns what the delete dialog names: the one file it asks about, or how many. These are
    /// the files asked about, not the selection now, which a refresh or a script may have
    /// changed while the dialog was open.
    fn delete_what(&self) -> String {
        match self.dialogs.asked_names() {
            [name] => format!("\"{}\"", self.name_shown(name)),
            many => count(many.len(), "file"),
        }
    }

    /// Asks before files go to the trash or are deleted for good.
    pub(crate) fn confirm_delete_dialog(&mut self, ctx: &egui::Context) {
        if let Some(trash) = self.dialogs.confirm_delete {
            let what = self.delete_what();
            let mut delete = false;
            let now = ctx.input(|i| i.time);
            let shown = *self.dialogs.delete_shown.get_or_insert(now);
            let enter_answers = self.dialogs.trash_refused.is_none() || now - shown >= ENTER_AFTER;
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
                // Only a new press answers, not a repeat of an Enter held since before the
                // question opened by itself.
                let pressed = |e: &egui::Event| {
                    matches!(e, egui::Event::Key { key: egui::Key::Enter, pressed: true, repeat: false, .. })
                };
                ui.horizontal(|ui| {
                    delete = crate::ui::menus::button(ui, tr("Delete (Enter)")).clicked()
                        || enter_answers && ui.input(|i| i.events.iter().any(pressed));
                    if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if !enter_answers {
                // The frame that lets Enter answer comes without another event.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(ENTER_AFTER));
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
        if self.dialogs.confirm_folder {
            let name = (self.dialogs.folder_target.as_deref())
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut trash = false;
            let modal = egui::Modal::new(egui::Id::unique("confirm-folder")).show(ctx, |ui| {
                ui.label(tf!(
                    "Move the folder \"{name}\" and all in it to the trash?",
                    name
                ));
                ui.horizontal(|ui| {
                    trash = crate::ui::menus::button(ui, tr("Move to Trash (Enter)")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if trash {
                self.dialogs.confirm_folder = false;
                self.trash_folder(ctx);
            } else if modal.should_close() {
                self.dialogs.confirm_folder = false;
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
        let modal = egui::Modal::new(egui::Id::unique("confirm-collection")).show(ctx, |ui| {
            ui.label(question.as_str());
            ui.horizontal(|ui| {
                trash = crate::ui::menus::button(ui, tr("Move to Trash (Enter)")).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter));
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

    /// Returns whether Preferences or a modal dialog is open. The keys then go there, not to
    /// the shortcuts.
    pub(crate) fn dialog_open(&self) -> bool {
        self.prefs.is_open() || self.modal_open()
    }

    /// Returns whether a modal dialog is open, which takes Esc before anything under it. This
    /// is the one list of them, and Preferences leaves Esc to it.
    pub(crate) fn modal_open(&self) -> bool {
        self.dialogs.confirm_delete.is_some()
            || self.dialogs.confirm_collection.is_some()
            || self.dialogs.confirm_folder
            || self.dialogs.prompt.is_some()
            || self.folder.asks_login()
            || self.print_window.is_some()
            || self.lightroom.is_some()
            || self.save_as.is_some()
            || !self.work.cut_short().is_empty()
    }
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

    /// Rename Many's preview warns when the first new name has a character that the camera card
    /// of the files refuses. The card's file system is looked up once, as the dialog opens.
    #[test]
    fn rename_many_warns_of_what_fat_refuses() {
        use crate::files::file_ops::TEST_MOUNT;
        use std::ffi::OsStr;
        let d = std::env::temp_dir().join(format!("glimt-test-fat-many-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&d);
        let listed = |a: &crate::App| !a.list.files().is_empty();
        assert!(crate::support::test_support::settle(&mut app, &ctx, listed));
        app.ask_about_targets();
        TEST_MOUNT.set(Some("ext4".into()));
        app.ask_rename_many();
        assert_eq!(app.dialogs.fat, None);
        TEST_MOUNT.set(Some("vfat".into()));
        app.ask_rename_many();
        TEST_MOUNT.set(None);
        let fat = app.dialogs.fat.as_deref();
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
        app.dialogs.asked_about(0, vec!["a.png".into()]);
        app.dialogs.confirm_delete_instead("fuse.rclone".into());
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
        enter_at(&mut app, 10.0 + super::ENTER_AFTER, &[(true, true)]);
        assert!(
            app.dialogs.confirm_delete.is_some(),
            "a held Enter's repeat answered"
        );
        // The Enter is let go and pressed again.
        enter_at(
            &mut app,
            10.6 + super::ENTER_AFTER,
            &[(false, false), (true, false)],
        );
        assert!(
            app.dialogs.confirm_delete.is_none(),
            "a later Enter did not answer"
        );
        std::fs::remove_dir_all(dir).unwrap();
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
        app.answer_prompt(&ctx, super::Prompt::NewCollection, &name);
        assert_eq!(
            app.list.selected().iter().copied().collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(app.list.range_end(), None);
        // The collection is written in the background, and removed once it is.
        let file = crate::model::collections::dir()
            .unwrap()
            .join(format!("{name}.gqv"));
        let written = |a: &crate::App| !a.working_besides_image();
        assert!(crate::support::test_support::settle(
            &mut app, &ctx, written
        ));
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
        assert_eq!(app.delete_what(), "\"a.png\"");
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
        app.open(&ctx, list);
        let start = app.dialog_start(&super::Pick::SaveFrame(image.clone(), 2));
        assert_eq!(start, images.join("a-frame-3.png"));
        std::fs::remove_dir_all(d).unwrap();
    }
}
