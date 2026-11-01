//! The file chooser (zenity or kdialog) and the small dialogs: a name asked for, a delete confirmed, a share's login, a job cut short.

use crate::Deleted;
use crate::Login;
use crate::PrintJob;
use crate::PrintTo;
use crate::Receiving;
use crate::Took;
use crate::collections;
use crate::command;
use crate::config;
use crate::count;
use crate::external;
use crate::journal;
use crate::noted;
use crate::pattern_name;
use crate::save;
use crate::tf;
use crate::theme;
use crate::tr;
use crate::xdg_dir;
use eframe::egui;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
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
    NewFolder,
    /// Rename the folder in `App::folder_target`.
    RenameFolder,
    /// Rename the shown image.
    Rename,
    /// Rename the selected files with a pattern: * = old name, # = number.
    RenameMany,
    /// A new collection with the selected files.
    NewCollection,
    /// A new name for the open collection.
    RenameCollection,
    /// Save the look shown as a theme file of this name.
    SaveTheme,
    /// This window's own title, a template as in Preferences.
    WindowTitle,
}

impl crate::App {
    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    pub(crate) fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        // One at a time: only one answer is waited for, and a second dialog's would replace the
        // first's, whose copy or move would then never happen.
        if self.file_dialog.is_some() {
            self.notice = Some(tr("A file dialog is open already").into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start = match &purpose {
            // A name for the file to save: the first image's, as a PDF.
            Pick::SavePdf(job) => self.dir.join(
                (job.paths.first())
                    .map_or("print".into(), |p| p.with_extension("pdf"))
                    .file_name()
                    .unwrap_or_default(),
            ),
            // "name-frame-3.png" beside the file.
            Pick::SaveFrame(path, k) => {
                (self.dir).join(save::stem_and(path, &format!("-frame-{}.png", k + 1)))
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
            _ => self.dir.join(""),
        };
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
        self.file_dialog = Some(purpose);
        self.file_dialog_rx = Receiving::of(rx);
    }

    /// The file dialog's answer, for what it was opened for.
    pub(crate) fn receive_picked(&mut self, ctx: &egui::Context) {
        let picked = match self.file_dialog_rx.take() {
            Took::Answer(picked) => picked,
            // A dialog whose thread ended without an answer is over too: the next may open.
            Took::Gone => return self.file_dialog = None,
            Took::Nothing => return,
        };
        {
            let Some(purpose) = self.file_dialog.take() else {
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
        let done = match self.mounting_rx.take() {
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
                    self.login = Some(Login {
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
        if let Some((prompt, mut text)) = self.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => "New folder name:".to_owned(),
                Prompt::RenameFolder => "New folder name:".to_owned(),
                Prompt::SaveTheme => "Name of the new theme:".to_owned(),
                Prompt::WindowTitle => tr(
                    "Title of this window (%name%, %path%, %folder%, %app%; empty: as in \
                     Preferences):",
                )
                .to_owned(),
                Prompt::RenameCollection => "New name of the collection:".to_owned(),
                Prompt::Rename => "New file name:".to_owned(),
                Prompt::NewCollection => tf!(
                    "Name of a new collection with {count}:",
                    count = count(self.first_target().0, "image")
                ),
                Prompt::RenameMany => tf!(
                    "New names for {count} (* = old name, # = number, ### = 001):",
                    count = count(self.first_target().0, "file")
                ),
            };
            // The first new name, so the pattern can be checked before renaming.
            let i = self.first_target().1;
            let first = (i < self.list.files.len()).then(|| self.path(i));
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
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("OK (Enter)")).clicked() || enter {
                        answer = Some(text.clone());
                    }
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if let Some(text) = answer {
                self.answer_prompt(ctx, prompt, text.trim());
            } else if !modal.should_close() {
                self.prompt = Some((prompt, text));
            }
        }
    }

    /// The text dialog was answered with `text`.
    pub(crate) fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if let Prompt::WindowTitle = prompt {
            return self.set_window_title(ctx, text);
        }
        if let Prompt::NewCollection = prompt {
            return match collections::new_file(text) {
                Ok(file) => self.add_to_collection(&file),
                Err(e) => self.notice = Some(e),
            };
        }
        if let Prompt::RenameCollection = prompt {
            let Some(old) = self.collection.clone() else {
                return;
            };
            match collections::rename(&old, text) {
                Ok(new) => {
                    self.collection = Some(new);
                    self.title(ctx);
                }
                Err(e) => self.notice = Some(e),
            }
            return;
        }
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(tf!("Not a valid name: \"{text}\"", text));
            return;
        }
        if let Prompt::SaveTheme = prompt {
            let file = theme::dir().map(|d| d.join(format!("{text}.theme")));
            let saved = file.ok_or("no home folder".to_owned()).and_then(|f| {
                // Never over a theme there, hand-edited perhaps, nor in the place of a built-in
                // one, which a file of its name would hide for good.
                if theme::built_in(text) {
                    return Err(tf!("{text} is a built-in theme: choose another name", text));
                }
                let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
                // Whole or not at all: a full disk left half a theme, whose name was then taken.
                let made = config::create_file(&f, theme::Theme::file_from(ctx));
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
                Prompt::Rename if found[0] != self.list.cur => self.go(ctx, found[0]),
                Prompt::Rename => {}
                _ => self.list.selected = found.into_iter().collect(),
            }
        }
        if matches!(prompt, Prompt::Rename)
            && !self.list.files.is_empty()
            && !self.none_saving(&[self.path(self.list.cur)])
        {
            return;
        }
        // A renamed file stays in its own folder, also a subfolder in the list with subfolders.
        let target = match prompt {
            Prompt::Rename if !self.list.files.is_empty() => {
                self.path(self.list.cur).with_file_name(text)
            }
            _ => self.dir.join(text),
        };
        match prompt {
            // In the background, as a share that stopped answering would hold the window until
            // create_dir returns; `folder_made` reads the folder again.
            Prompt::NewFolder => {
                let deleted_tx = self.deleted_tx.clone();
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
            Prompt::Rename if self.list.files.is_empty() => {}
            Prompt::Rename if target == self.path(self.list.cur) => {}
            Prompt::RenameMany => self.rename_many(ctx, text),
            Prompt::NewCollection
            | Prompt::RenameCollection
            | Prompt::SaveTheme
            | Prompt::RenameFolder
            | Prompt::WindowTitle => {}
            // As Rename Many: in its thread, never over a file or another file's sidecar
            // there; `renamed` puts the list right.
            Prompt::Rename => {
                self.rename_in_background(ctx, vec![self.path(self.list.cur)], vec![target]);
            }
        }
    }

    /// Renames cut short by a crash or a power cut: put the old names back, or finish them, in
    /// the background; Later asks again at the next start.
    pub(crate) fn cut_short_dialog(&mut self, ctx: &egui::Context) {
        if self.cut_short.is_empty() {
            return;
        }
        let files: usize = self.cut_short.iter().map(|c| c.1.len()).sum();
        let folder = (self.cut_short.iter())
            .find_map(|c| c.1.first())
            .and_then(|m| m.0.parent())
            .map_or(String::new(), |p| p.display().to_string());
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("cut short")).show(ctx, |ui| {
            ui.strong(tr("Rename Many was cut short"));
            ui.label(format!(
                "A crash or a power cut stopped it in {folder}. Some files may have their new \
                 names, some their old ones, and some hidden temporary names ({} to put right).",
                count(files, "step")
            ));
            ui.horizontal(|ui| {
                if noted(
                    &ui.button(tr("Put the old names back")),
                    tr("Put the old names back"),
                )
                .clicked()
                {
                    choice = Some(true);
                }
                if noted(
                    &ui.button(tr("Finish the renaming")),
                    tr("Finish the renaming"),
                )
                .clicked()
                {
                    choice = Some(false);
                }
                if noted(&ui.button(tr("Later")), tr("Later")).clicked() {
                    choice = None;
                    self.cut_short.clear();
                }
            });
        });
        let Some(undo) = choice else { return };
        // Asked before the plans are taken: one held back is asked about again.
        let touches: Vec<PathBuf> = (self.cut_short.iter())
            .flat_map(|(_, moves, _)| moves.iter().flat_map(|(a, b)| [a.clone(), b.clone()]))
            .collect();
        if !self.none_saving(&touches) {
            return;
        }
        let plans = std::mem::take(&mut self.cut_short);
        let deleted = self.deleted_tx.clone();
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
        if let Some(mut login) = self.login.take() {
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
                    connect |= ui.button(tr("Connect (Enter)")).clicked();
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if connect {
                self.mount_as(ctx, login, true);
            } else if !modal.should_close() {
                self.login = Some(login);
            }
        }
    }

    /// Asks before files go to the trash or are deleted for good.
    pub(crate) fn confirm_delete_dialog(&mut self, ctx: &egui::Context) {
        if let Some(trash) = self.confirm_delete {
            let what = match self.first_target() {
                (1, i) => format!("\"{}\"", self.name(i)),
                (many, _) => count(many, "file"),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
                if trash {
                    ui.label(tf!("Move {what} to the trash?", what));
                } else {
                    ui.label(tf!("Permanently delete {what}?", what));
                    ui.weak(tr("This cannot be undone."));
                }
                ui.horizontal(|ui| {
                    delete = ui.button(tr("Delete (Enter)")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm_delete = None;
                if let Some(targets) = self.asked_targets() {
                    self.delete_now(ctx, trash, targets);
                }
            } else if modal.should_close() {
                self.confirm_delete = None;
            }
        }
    }

    /// Asks before a folder goes to the trash.
    pub(crate) fn confirm_folder_dialog(&mut self, ctx: &egui::Context) {
        if self.confirm_folder {
            let name = (self.folder_target.as_deref())
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
                    trash = ui.button(tr("Move to Trash (Enter)")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if trash {
                self.confirm_folder = false;
                self.trash_folder(ctx);
            } else if modal.should_close() {
                self.confirm_folder = false;
                self.folder_target = None;
            }
        }
    }
}
