//! The info panel (Ctrl+K) and what it edits: EXIF, keywords and the keyword tree, comment, alt
//! text, and marks.

use crate::files::config::keyword_tree_path;
use crate::files::external::open_url;
use crate::images::colour::colour_source;
#[cfg(feature = "marks")]
use crate::metadata::marks;
use crate::metadata::xmp::KeywordsAndComment;
use crate::support::format::{count, human_size, thousands};
use crate::support::lang::tr;
use crate::ui::dialogs::entered;
use crate::{files::config, images::decode, metadata::exif, metadata::xmp, tf};
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// A file's EXIF from the reader thread.
pub(crate) struct ExifRead {
    path: PathBuf,
    /// The info panel's rows, and every field for the EXIF window.
    rows: Vec<(&'static str, String)>,
    all: Vec<(&'static str, String, String)>,
    /// Keywords and comment, their keyword tree paths, and the alt text and description.
    meta: xmp::KeywordsAndComment,
    paths: Vec<String>,
    alt_text: xmp::AltText,
}

/// Keywords, comment, alt text and extended description of the shown image as being edited in
/// the info panel, and as last read or saved, to tell whether they changed.
pub(crate) struct MetadataEdit {
    pub(crate) path: PathBuf,
    keywords: String,
    comment: String,
    pub(crate) saved: xmp::KeywordsAndComment,
    /// Its keyword tree paths, which tick the tree's boxes with `saved`'s keywords.
    paths: Vec<String>,
    /// The IPTC alt text and extended description as edited, and as last read or saved.
    alt_text: String,
    extended_description: String,
    pub(crate) saved_alt_text: xmp::AltText,
}

/// A level of the keyword tree: its children by name.
#[derive(Default)]
pub(crate) struct KeywordNode(std::collections::BTreeMap<String, KeywordNode>);

impl KeywordNode {
    fn insert(&mut self, path: &str) {
        let mut node = self;
        for level in path.split('|') {
            node = node.0.entry(level.to_owned()).or_default();
        }
    }

    /// The boxes of this level's keywords under `prefix`, ticked by `have` (the image's keywords
    /// and keyword tree paths); a box clicked
    /// gives (its path, on) in `tick`, and Remove in its menu gives the path in `drop`.
    fn show(
        &self,
        ui: &mut egui::Ui,
        prefix: &str,
        have: (&[String], &[String]),
        tick: &mut Option<(String, bool)>,
        drop: &mut Option<String>,
    ) {
        for (name, child) in &self.0 {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}|{name}")
            };
            let mut box_of = |ui: &mut egui::Ui| {
                let mut on = xmp::has_tag(have.0, have.1, &path);
                let response = crate::ui::menus::checkbox(ui, &mut on, name);
                if response.changed() {
                    *tick = Some((path.clone(), on));
                }
                response.context_menu(|ui| {
                    if crate::ui::menus::button(ui, tr("Remove from the tree")).clicked() {
                        *drop = Some(path.clone());
                    }
                });
            };
            if child.0.is_empty() {
                box_of(ui);
            } else {
                let id = ui.make_persistent_id(("keyword", &path));
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                )
                .show_header(ui, box_of)
                .body(|ui| child.show(ui, &path, have, tick, drop));
            }
        }
    }
}

/// The info panel and the EXIF window: what is read of the shown image, and what is being edited.
pub(crate) struct InfoPanel {
    /// To the EXIF reader thread, and its answers.
    exif_requests: mpsc::Sender<PathBuf>,
    exif_answers: mpsc::Receiver<ExifRead>,
    /// The file the info panel asked for last, and the panel's rows as read for a file.
    exif_asked: Option<PathBuf>,
    exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Every EXIF field of the shown image, for the EXIF window (Ctrl+E).
    exif_all: Vec<(&'static str, String, String)>,
    /// View → EXIF Window is open.
    is_exif_window_open: bool,
    /// Keywords, comment, alt text and extended description in the info panel; saved when a field
    /// loses the focus, or another image is shown.
    metadata_edit: Option<MetadataEdit>,
    /// The file whose panel is to be read once the sidecar jobs that write it are over
    /// (`read_info_again`, or an image shown while its own save waits in the writer). Read at
    /// once, it could read the file before the writer reached it. The panel's fields cannot be
    /// edited meanwhile, as they show the file as it was.
    reread_after_write: Option<(PathBuf, Vec<std::sync::Weak<crate::files::file_ops::Job>>)>,
    /// Alt+K: put the focus in the keywords field when the panel shows next.
    keywords_take_focus: bool,
    /// The keyword tree (Geeqie's), as paths "Places|Norway|Oslo", sorted; in the info panel.
    keyword_tree: Vec<String>,
    /// A path being typed to add to the keyword tree.
    new_keyword: String,
}

impl InfoPanel {
    /// The EXIF reader started, and the keyword tree read from its file.
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        let (exif_requests, exif_answers) = spawn_exif_reader(ctx);
        InfoPanel {
            exif_requests,
            exif_answers,
            exif_asked: None,
            exif: None,
            exif_all: Vec::new(),
            reread_after_write: None,
            is_exif_window_open: false,
            metadata_edit: None,
            keywords_take_focus: false,
            keyword_tree: (keyword_tree_path()
                .and_then(|p| crate::images::decode::read_text(p).ok()))
            .map_or_else(Vec::new, |t| xmp::keyword_paths(t.lines())),
            new_keyword: String::new(),
        }
    }

    /// The info panel's row `label` of `path`, when that is the file read.
    pub(crate) fn exif_row(&self, path: &Path, label: &str) -> Option<&str> {
        let (_, rows) = self.exif.as_ref().filter(|e| e.0 == path)?;
        rows.iter().find(|r| r.0 == label).map(|r| r.1.as_str())
    }

    /// What the panel edits of `path`, when it is that file's.
    pub(crate) fn metadata_edit_of(&self, path: &Path) -> Option<&MetadataEdit> {
        self.metadata_edit.as_ref().filter(|m| m.path == path)
    }

    /// View → EXIF Window, open or closed.
    pub(crate) fn toggle_exif_window(&mut self) {
        self.is_exif_window_open = !self.is_exif_window_open;
    }

    pub(crate) fn exif_window_open(&self) -> bool {
        self.is_exif_window_open
    }

    /// Alt+K: the keywords field takes the focus when the panel shows next.
    pub(crate) fn focus_keywords(&mut self) {
        self.keywords_take_focus = true;
    }
}

impl crate::App {
    // ---- EXIF, keywords, comment and marks ----

    /// Ask the EXIF reader for the shown image (once per image) and take what has arrived.
    pub(crate) fn request_and_take_exif(&mut self, ctx: &egui::Context) {
        self.reread_when_written();
        let path = self.path(self.list.shown());
        // An image shown again while its own save still waits behind a long job (a Lightroom
        // import) is read once the save is written, or the panel would show it without the save.
        if self.info_panel.exif_asked.as_ref() != Some(&path)
            && self.info_panel.reread_after_write.is_none()
        {
            let writes = self.file_operations.jobs_writing_sidecar_of(&path);
            if !writes.is_empty() {
                // The wait ends by forgetting the panel, so the image before keeps its edit.
                self.save_metadata_edit(ctx);
                self.info_panel.reread_after_write = Some((path.clone(), writes));
            }
        }
        if self.info_panel.exif_asked.as_ref() != Some(&path)
            && self.info_panel.reread_after_write.is_none()
        {
            let _ = self.info_panel.exif_requests.send(path.clone());
            self.info_panel.exif_asked = Some(path);
        }
        let done: Vec<ExifRead> = self.info_panel.exif_answers.try_iter().collect();
        for ExifRead {
            path,
            rows,
            all,
            meta,
            paths,
            alt_text,
        } in done
        {
            // Kept while being edited: the reader may answer for the image again.
            if self
                .info_panel
                .metadata_edit
                .as_ref()
                .is_none_or(|m| m.path != path)
            {
                self.save_metadata_edit(ctx);
                self.info_panel.metadata_edit = Some(MetadataEdit {
                    path: path.clone(),
                    keywords: meta.keywords_text(),
                    comment: meta.comment.clone(),
                    saved: meta,
                    paths,
                    alt_text: alt_text.text.clone(),
                    extended_description: alt_text.extended_description.clone(),
                    saved_alt_text: alt_text,
                });
            }
            self.info_panel.exif = Some((path, rows));
            self.info_panel.exif_all = all;
        }
    }

    /// File and EXIF details of the shown image, read in the background (Ctrl+K).
    pub(crate) fn info_panel(&mut self, ui: &mut egui::Ui) {
        self.request_and_take_exif(ui.ctx());
        egui::Panel::right("exif")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("exif rows").num_columns(2).show(ui, |ui| {
                        ui.weak(tr("File"));
                        ui.label(self.name(self.list.shown()));
                        ui.end_row();
                        if let Some(s) = self.list.loaded_images().get(&self.list.shown()) {
                            ui.weak(tr("Dimensions"));
                            ui.label(format!("{} × {}", s.original_size[0], s.original_size[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not
                        // flicker.
                        for (label, value) in self.info_panel.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(tr(label));
                            // The place opens on a map in the browser.
                            if *label == "GPS" {
                                let link = crate::ui::menus::link(ui, value).on_hover_text(tr("Show on OpenStreetMap"));
                                if link.clicked()
                                    && let Some((lat, lon)) = value.split_once(", ")
                                {
                                    open_url(&format!(
                                        "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=15/{lat}/{lon}"
                                    ));
                                }
                            } else {
                                ui.label(exif::shown_value(label, value));
                            }
                            ui.end_row();
                        }
                    });
                    ui.separator();
                    self.meta_fields(ui);
                    self.keyword_tree(ui);
                });
            });
    }

    /// Keywords and comment of the shown image, as in Geeqie's info sidebar, with its alt text and
    /// extended description.
    pub(crate) fn meta_fields(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.list.shown());
        // While the file is to be read again, its fields show it as it was: an edit there would
        // write that over what changed it.
        if self.info_panel.reread_after_write.is_some() {
            ui.disable();
        }
        let Some(edit) = self
            .info_panel
            .metadata_edit
            .as_mut()
            .filter(|m| m.path == path)
        else {
            return;
        };
        ui.weak(tr("Keywords"));
        let keywords = ui.add(
            egui::TextEdit::singleline(&mut edit.keywords)
                .hint_text(tr("Separated by commas"))
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.info_panel.keywords_take_focus) {
            keywords.request_focus();
        }
        ui.weak(tr("Comment"));
        let comment = ui.add(
            egui::TextEdit::multiline(&mut edit.comment)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        // IPTC's accessibility fields, which screen readers and web pages read.
        ui.weak(tr("Alt text"));
        let alt = ui.add(
            egui::TextEdit::multiline(&mut edit.alt_text)
                .hint_text(tr("What the picture shows, for someone who cannot see it"))
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.weak(tr("Extended description"));
        let extended = ui.add(
            egui::TextEdit::multiline(&mut edit.extended_description)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let add = KeywordsAndComment::parse_keywords(&edit.keywords);
        let n = self.list.selected().len();
        if n > 1
            && !add.is_empty()
            && crate::ui::menus::button(
                ui,
                &tf!("Add the keywords to the {n} selected", n = thousands(n)),
            )
            .on_hover_text(tr("Each file keeps its own keywords and gets these too"))
            .clicked()
        {
            self.add_keywords(ui.ctx(), add);
        }
        if [keywords, comment, alt, extended]
            .iter()
            .any(egui::Response::lost_focus)
        {
            self.save_metadata_edit(ui.ctx());
        }
    }

    /// The keyword tree as in Geeqie's sidebar: a box for each keyword, ticked when the shown
    /// image has it; a click sets it on the selected images (or the shown one), or takes it off.
    pub(crate) fn keyword_tree(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.list.shown());
        let Some((have, paths)) = (self.info_panel.metadata_edit.as_ref())
            .filter(|m| m.path == path)
            .map(|m| (m.saved.keywords.clone(), m.paths.clone()))
        else {
            return;
        };
        if self.info_panel.reread_after_write.is_some() {
            ui.disable();
        }
        ui.separator();
        ui.weak(tr("Keyword tree"));
        let mut root = KeywordNode::default();
        for p in &self.info_panel.keyword_tree {
            root.insert(p);
        }
        let (mut tick, mut drop) = (None, None);
        root.show(ui, "", (&have, &paths), &mut tick, &mut drop);
        // The button first, from the right, so the field takes exactly what is left: a field
        // too wide makes the panel grow a little on every frame.
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        let layout = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            let add = crate::ui::menus::button(ui, tr("Add")).clicked();
            let field = egui::TextEdit::singleline(&mut self.info_panel.new_keyword)
                .hint_text(tr("Places|Norway|Oslo"))
                .desired_width(ui.available_width());
            let enter = entered(&ui.add(field));
            if (add || enter) && !self.info_panel.new_keyword.trim().is_empty() {
                let new = std::mem::take(&mut self.info_panel.new_keyword);
                self.change_keyword_tree(vec![new], None);
            }
        });
        // The image's own keywords that the tree does not have yet, at its top.
        let names: HashSet<String> = (self.info_panel.keyword_tree.iter())
            .map(|p| p.rsplit('|').next().unwrap_or(p).to_lowercase())
            .collect();
        let missing: Vec<String> = (have.iter())
            .filter(|k| !names.contains(&k.to_lowercase()))
            .cloned()
            .collect();
        if !missing.is_empty()
            && crate::ui::menus::small_button(ui, tr("Add this image's keywords to the tree"))
                .clicked()
        {
            self.change_keyword_tree(missing, None);
        }
        if let Some(p) = drop {
            self.change_keyword_tree(Vec::new(), Some(p));
        }
        if let Some((tag, on)) = tick {
            self.set_keyword_on_files(ui.ctx(), tag, on);
        }
    }

    /// Add `add` to the keyword tree in `$XDG_CONFIG_HOME/glimt/keywords`, or take `remove` and
    /// the paths below it out: this change alone, on the file as it is now, so what another
    /// window changed meanwhile stays.
    pub(crate) fn change_keyword_tree(&mut self, add: Vec<String>, remove: Option<String>) {
        // Locked settings (--lock-settings, a kiosk): the tree changes for this session only,
        // as the other settings do, and the file is not written.
        let Some(p) = keyword_tree_path().filter(|_| !config::locked()) else {
            self.info_panel.keyword_tree =
                xmp::keyword_tree_changed(&self.info_panel.keyword_tree, add, remove);
            return;
        };
        // One that cannot be read is not written over: its keywords would be lost.
        let change = |old: &[u8]| {
            let on_disk: Vec<String> = String::from_utf8_lossy(old)
                .lines()
                .map(str::to_owned)
                .collect();
            let tree = xmp::keyword_tree_changed(&on_disk, add, remove);
            ((tree.join("\n") + "\n").into_bytes(), tree)
        };
        match crate::files::file_io::edit_shared(&p, change) {
            Ok(tree) => self.info_panel.keyword_tree = tree,
            Err(e) => self.notice = Some(tf!("Could not save the keyword tree: {e}", e)),
        }
    }

    /// Set the keyword `tag` (a tree path) on the selected images, or the shown one, or take it
    /// off, in the background; the shown image's keywords show it at once.
    pub(crate) fn set_keyword_on_files(&mut self, ctx: &egui::Context, tag: String, on: bool) {
        self.save_metadata_edit(ctx);
        let Some(targets) = self.targets() else {
            return;
        };
        if targets.contains(&self.list.shown())
            && let Some(edit) = &mut self.info_panel.metadata_edit
        {
            xmp::set_tag(&mut edit.saved.keywords, &mut edit.paths, &tag, on);
            edit.keywords = edit.saved.keywords_text();
        }
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let activity = match on {
            true => tr("Setting keywords"),
            false => tr("Taking keywords off"),
        };
        let (n, written) = (paths.len(), tag.clone());
        self.write_sidecars(
            ctx,
            activity,
            paths,
            move |path| xmp::tag_file(path, &written, on).is_ok(),
            move |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "The keyword could not be saved for {count}",
                    count = count(failed, "file")
                )),
                _ if job.is_stopped() => Some(job.stopped_text()),
                // One sentence, so each language can put the words in its own order.
                _ if n > 1 => Some(match on {
                    true => tf!("{tag} set on {count}", tag, count = count(n, "file")),
                    false => tf!("{tag} taken off {count}", tag, count = count(n, "file")),
                }),
                _ => None,
            },
        );
    }

    /// Reads the shown image's EXIF, keywords, comment and alt text again, after something else
    /// changed them, such as a tool. An edit in the panel that is not saved yet is saved
    /// first, so it is not lost. While a sidecar job still writes the file, the panel is read
    /// again only once that job is over (`request_and_take_exif`): read at once, it could show
    /// the file as it was before the edit, and a later edit would then save the edit away again.
    pub(crate) fn read_info_again(&mut self, ctx: &egui::Context) {
        self.save_metadata_edit(ctx);
        let path = self.path(self.list.shown());
        let writes = self.file_operations.jobs_writing_sidecar_of(&path);
        self.info_panel.reread_after_write = Some((path, writes));
        self.reread_when_written();
    }

    /// Lets the panel read the shown image again, when it waits for the sidecar jobs that write
    /// it and they are over. Jobs of other files do not hold it up.
    /// Another image shown meanwhile is read for itself, so its edit is never dropped by this.
    fn reread_when_written(&mut self) {
        let Some((file, writes)) = &self.info_panel.reread_after_write else {
            return;
        };
        if *file != self.path(self.list.shown()) {
            self.info_panel.reread_after_write = None;
        } else if writes.iter().all(|w| w.strong_count() <= 1) {
            self.info_panel.reread_after_write = None;
            self.info_panel.metadata_edit = None;
            self.info_panel.exif = None;
            self.info_panel.exif_asked = None;
        }
    }

    /// Save the keywords, comment, alt text and extended description being edited, if they changed:
    /// in the thread that writes this window's sidecars, which closing waits for, not in the
    /// window's, as a sidecar on a slow share would hold the window up for its read, write and
    /// sync. The fields show what was saved at once; a failure is told.
    pub(crate) fn save_metadata_edit(&mut self, ctx: &egui::Context) {
        let Some(edit) = &mut self.info_panel.metadata_edit else {
            return;
        };
        let alt_text = xmp::AltText {
            text: edit.alt_text.trim().to_owned(),
            extended_description: edit.extended_description.trim().to_owned(),
        };
        // Saved only when edited, as the keywords: browsing writes nothing.
        if (edit.alt_text != edit.saved_alt_text.text
            || edit.extended_description != edit.saved_alt_text.extended_description)
            && alt_text != edit.saved_alt_text
        {
            (edit.alt_text, edit.extended_description) =
                (alt_text.text.clone(), alt_text.extended_description.clone());
            edit.saved_alt_text = alt_text.clone();
            let (path, tx) = (edit.path.clone(), self.notice_sender.clone());
            // A sidecar job like the keywords': Move, Rename and Delete wait for it.
            self.write_sidecars(
                ctx,
                tr("Saving the alt text"),
                vec![path],
                move |p| match xmp::write_alt_text(p, &alt_text) {
                    Ok(()) => true,
                    Err(e) => {
                        let _ = tx.send(tf!("Could not save the alt text: {e}", e));
                        false
                    }
                },
                |_, _| None,
            );
        }
        let Some(edit) = &mut self.info_panel.metadata_edit else {
            return;
        };
        // Fields as they were shown are no change, also when splitting them again at commas
        // would read them differently ("Smith, John" as one keyword, a comment's last newline).
        if edit.keywords == edit.saved.keywords_text() && edit.comment == edit.saved.comment {
            return;
        }
        // Only what was edited: keywords set on this file in the background meanwhile stay.
        let keywords = (edit.keywords != edit.saved.keywords_text())
            .then(|| KeywordsAndComment::parse_keywords(&edit.keywords));
        let comment = (edit.comment != edit.saved.comment).then(|| edit.comment.trim().to_owned());
        if let Some(k) = &keywords {
            edit.saved.keywords = k.clone();
        }
        if let Some(c) = &comment {
            edit.saved.comment = c.clone();
        }
        edit.keywords = edit.saved.keywords_text();
        edit.comment = edit.saved.comment.clone();
        let (path, tx) = (edit.path.clone(), self.notice_sender.clone());
        // A sidecar job, so that a Move, Rename or Delete of the file waits until it has
        // landed, instead of leaving the new keywords behind under the old name.
        self.write_sidecars(
            ctx,
            tr("Saving keywords"),
            vec![path],
            move |p| match xmp::write_changed(p, keywords.clone(), comment.clone()) {
                Ok(_) => true,
                Err(e) => {
                    let _ = tx.send(tf!("Could not save the keywords: {e}", e));
                    false
                }
            },
            |_, _| None,
        );
    }

    /// Add `keywords` to every selected file, in the background.
    pub(crate) fn add_keywords(&mut self, ctx: &egui::Context, keywords: Vec<String>) {
        // The shown image's own edit is saved first. The panel then shows the added keywords at
        // once, as `set_keyword_on_files` does. When it read them back from the file instead, it
        // could read the file before the writer reached it, and a later edit saved them away
        // again.
        self.save_metadata_edit(ctx);
        let Some(targets) = self.targets() else {
            return;
        };
        if targets.contains(&self.list.shown())
            && let Some(edit) = &mut self.info_panel.metadata_edit
        {
            xmp::add_to(&mut edit.saved.keywords, &keywords);
            edit.keywords = edit.saved.keywords_text();
        }
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let n = paths.len();
        // Through the sidecar writer, with keywords ticked and stars: in the order asked.
        self.write_sidecars(
            ctx,
            tr("Adding keywords"),
            paths,
            move |path| xmp::add_keywords(path, &keywords).is_ok(),
            move |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "Keywords could not be saved for {count}",
                    count = count(failed, "file")
                )),
                _ if job.is_stopped() => Some(job.stopped_text()),
                _ => Some(tf!("Keywords added to {count}", count = count(n, "file"))),
            },
        );
    }

    /// Stars and marks of file `i` as text ("★★★  1 4"). Returns an empty text without the marks
    /// feature.
    #[cfg(feature = "marks")]
    pub(crate) fn mark_text(&self, i: usize) -> String {
        if !self.config.show_marks {
            return String::new();
        }
        marks::text(self.marks.get(&self.path(i)))
    }

    #[cfg(not(feature = "marks"))]
    pub(crate) fn mark_text(&self, _: usize) -> String {
        String::new()
    }

    /// View → EXIF Window (Ctrl+E): every EXIF field of the shown image.
    pub(crate) fn exif_fields_window(&mut self, ctx: &egui::Context) {
        if self.info_panel.is_exif_window_open && !self.list.files().is_empty() {
            self.request_and_take_exif(ctx);
            let mut open = true;
            let title = tf!("EXIF: {name}", name = self.name(self.list.shown()));
            egui::Window::new(title)
                .id(egui::Id::unique("exif window"))
                .open(&mut open)
                .default_size([560.0, 600.0])
                .vscroll(true)
                .show(ctx, |ui| {
                    if self.info_panel.exif_all.is_empty() {
                        ui.weak(tr("No EXIF data"));
                    }
                    egui::Grid::new("exif all")
                        .striped(true)
                        .num_columns(3)
                        .show(ui, |ui| {
                            for (group, name, value) in &self.info_panel.exif_all {
                                ui.weak(tr(group));
                                ui.label(name);
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                });
            self.info_panel.is_exif_window_open = open;
        }
    }
}

/// The EXIF reader: one thread that skips to the newest request, so fast browsing never
/// queues up; it sends the panel's rows, every field, and the keywords and comment.
pub(crate) fn spawn_exif_reader(
    ctx: &egui::Context,
) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<ExifRead>) {
    let (exif_requests, paths_rx) = mpsc::channel::<PathBuf>();
    let (exif_tx, exif_answers) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        while let Ok(mut path) = paths_rx.recv() {
            path = paths_rx.try_iter().last().unwrap_or(path);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            let mut rows = vec![("File size", human_size(size))];
            let (mut all, mut meta, mut paths, mut alt_text) = Default::default();
            // Not opened unless it is a file: a pipe would block this thread for good. A decoder
            // that panics on the file leaves the panel without EXIF for it, not for good.
            if decode::is_regular_file(&path) {
                let read = crate::support::threads::catch_panic(|| {
                    let mut rows = vec![("Colour profile", colour_source(&path))];
                    let (exif_rows, all) = exif::read_and_all(&path);
                    rows.extend(exif_rows);
                    let (meta, paths, alt_text) = xmp::read_all(&path);
                    (rows, all, meta, paths, alt_text)
                });
                if let Some((more, a, m, p, x)) = read {
                    rows.extend(more);
                    (all, meta, paths, alt_text) = (a, m, p, x);
                }
            }
            let read = ExifRead {
                path,
                rows,
                all,
                meta,
                paths,
                alt_text,
            };
            if exif_tx.send(read).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    (exif_requests, exif_answers)
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, run_background_until};

    /// Keywords added to the selection show in the panel at once, and they stay after the
    /// writer is done, so a later edit in the panel keeps them.
    #[test]
    fn added_keywords_show_in_the_panel_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-test-add-kw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        // The panel asks in each frame it is drawn, and takes the answer then.
        for _ in 0..500 {
            app.request_and_take_exif(&ctx);
            if app.info_panel.metadata_edit.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.add_keywords(&ctx, vec!["beach".into()]);
        let shown = |app: &crate::App| {
            let edit = app.info_panel.metadata_edit.as_ref().unwrap();
            edit.saved.keywords == ["beach"] && edit.keywords.contains("beach")
        };
        assert!(shown(&app));
        let written =
            |_: &crate::App| crate::metadata::xmp::read(&dir.join("a.png")).keywords == ["beach"];
        assert!(run_background_until(&mut app, &ctx, written));
        assert!(shown(&app));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Waits for the info panel to read the shown file, as it does in each frame it is drawn.
    fn read_panel(app: &mut crate::App, ctx: &eframe::egui::Context) {
        for _ in 0..500 {
            app.request_and_take_exif(ctx);
            if app.info_panel.metadata_edit.is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the panel never read the file");
    }

    /// When something else changes the shown file, such as a tool that describes it, the
    /// panel shows its new keywords, not those it read before.
    #[test]
    fn a_changed_shown_file_is_read_again_in_the_info_panel() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-info-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&file).unwrap();
        let (mut app, ctx) = app_on(&dir);
        read_panel(&mut app, &ctx);
        crate::metadata::xmp::add_keywords(&file, &["beach".into()]).unwrap();
        app.files_changed(&ctx, std::slice::from_ref(&file));
        read_panel(&mut app, &ctx);
        assert_eq!(
            app.info_panel
                .metadata_edit
                .as_ref()
                .unwrap()
                .saved
                .keywords,
            ["beach"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An edit not saved yet when a tool changes the shown file is saved, and the panel keeps
    /// showing it, not to be edited, until no sidecar job runs, then reads the file again with
    /// the edit in. Another image shown meanwhile ends the wait.
    #[test]
    fn a_reread_waits_for_the_panels_own_save() {
        let name = format!("glimt-test-reread-save-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&file).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("b.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(crate::support::test_support::run_background_until(
            &mut app,
            &ctx,
            |a| !a.folder.is_listing()
        ));
        app.show_file(&ctx, 0);
        read_panel(&mut app, &ctx);
        // A long sidecar job of another file runs meanwhile. It holds the one sidecar thread
        // until `other` is dropped, so the panel's own save waits behind it however fast the
        // computer is. A job that is only counted let the save end before the panel looked.
        let (other, hold) = std::sync::mpsc::channel::<()>();
        let b = vec![dir.join("b.png")];
        app.write_sidecars(&ctx, "Other", b, move |_| hold.recv().is_err(), |_, _| None);
        app.info_panel.metadata_edit.as_mut().unwrap().keywords = "sea".into();
        app.files_changed(&ctx, std::slice::from_ref(&file));
        app.request_and_take_exif(&ctx);
        assert!(
            app.info_panel.metadata_edit.is_some() && app.info_panel.reread_after_write.is_some()
        );
        // Its fields show the file as it was, so they cannot be edited while it waits.
        let mut enabled = true;
        let _ = ctx.run_ui(Default::default(), |ui| {
            app.meta_fields(ui);
            enabled = ui.is_enabled();
        });
        assert!(!enabled, "the waiting panel's fields can be edited");
        drop(other);
        let written = |a: &crate::App| !a.file_operations.is_writing_sidecars();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, written
        ));
        app.request_and_take_exif(&ctx);
        assert!(app.info_panel.reread_after_write.is_none());
        read_panel(&mut app, &ctx);
        assert_eq!(
            app.info_panel
                .metadata_edit
                .as_ref()
                .unwrap()
                .saved
                .keywords,
            ["sea"]
        );
        // Another image shown while the wait runs ends it, and its own panel is read.
        let other = app.start_job_of("Other", 1, true, Vec::new()).unwrap();
        app.files_changed(&ctx, std::slice::from_ref(&file));
        app.show_file(&ctx, 1);
        app.request_and_take_exif(&ctx);
        assert!(app.info_panel.reread_after_write.is_none());
        drop(other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The panel waits only for the sidecar jobs that write the shown file: a long job of
    /// other files does not hold up its reread, and an image shown again while a job still
    /// writes it is read once that job is over.
    #[test]
    fn the_panel_waits_only_for_writes_of_its_file() {
        let name = format!("glimt-test-reread-own-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(4, 4).save(&a).unwrap();
        image::RgbImage::new(4, 4).save(&b).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(crate::support::test_support::run_background_until(
            &mut app,
            &ctx,
            |a| !a.folder.is_listing()
        ));
        app.show_file(&ctx, 0);
        read_panel(&mut app, &ctx);
        let other = app.start_job_of("Other", 1, true, vec![b.clone()]).unwrap();
        app.files_changed(&ctx, std::slice::from_ref(&a));
        app.request_and_take_exif(&ctx);
        assert!(
            app.info_panel.reread_after_write.is_none(),
            "it waits for b.png's job"
        );
        drop(other);
        let writing = app
            .start_job_of("Saving", 1, true, vec![a.clone()])
            .unwrap();
        app.show_file(&ctx, 1);
        app.request_and_take_exif(&ctx);
        assert!(
            app.info_panel.reread_after_write.is_none(),
            "b.png waits for a.png's job"
        );
        app.show_file(&ctx, 0);
        app.request_and_take_exif(&ctx);
        assert!(
            app.info_panel.reread_after_write.is_some(),
            "a.png is read before its save"
        );
        assert_ne!(app.info_panel.exif_asked.as_ref(), Some(&a));
        drop(writing);
        app.request_and_take_exif(&ctx);
        assert!(app.info_panel.reread_after_write.is_none());
        assert_eq!(app.info_panel.exif_asked.as_ref(), Some(&a));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
