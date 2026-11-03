//! The info panel (Ctrl+K) and what it edits: EXIF, keywords and the keyword tree, comment, alt
//! text, and marks.

use crate::colour::colour_source;
use crate::files::config::keyword_tree_path;
use crate::files::external::open_url;
#[cfg(feature = "marks")]
use crate::marks;
use crate::support::format::{count, human_size, thousands};
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::xmp::Meta;
use crate::{decode, exif, files::config, tf, xmp};
use eframe::egui;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

/// A file's EXIF from the reader thread.
pub(crate) struct ExifRead {
    path: PathBuf,
    /// The EXIF panel's rows, and every field for the EXIF window.
    rows: Vec<(&'static str, String)>,
    all: Vec<(&'static str, String, String)>,
    /// Keywords and comment, their keyword tree paths, and the alt text and description.
    meta: xmp::Meta,
    paths: Vec<String>,
    access: xmp::Access,
}

/// Keywords, comment, alt text and extended description of the shown image as being edited in
/// the EXIF panel, and as last read or saved, to tell whether they changed.
pub(crate) struct MetaEdit {
    pub(crate) path: PathBuf,
    keywords: String,
    comment: String,
    pub(crate) saved: xmp::Meta,
    /// Its keyword tree paths, which tick the tree's boxes with `saved`'s keywords.
    paths: Vec<String>,
    /// The IPTC alt text and extended description as edited, and as last read or saved.
    alt: String,
    extended: String,
    pub(crate) access: xmp::Access,
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
                let response = noted(&ui.checkbox(&mut on, name), name);
                if response.changed() {
                    *tick = Some((path.clone(), on));
                }
                response.context_menu(|ui| {
                    let remove = ui.button(tr("Remove from the tree"));
                    if noted(&remove, tr("Remove from the tree")).clicked() {
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
pub(crate) struct Info {
    /// To the EXIF reader thread, and its answers.
    pub(crate) exif_jobs: mpsc::Sender<PathBuf>,
    pub(crate) exif_rx: mpsc::Receiver<ExifRead>,
    /// The file the EXIF panel asked for last, and the panel's rows as read for a file.
    pub(crate) exif_asked: Option<PathBuf>,
    pub(crate) exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Every EXIF field of the shown image, for the EXIF window (Ctrl+E).
    pub(crate) exif_all: Vec<(&'static str, String, String)>,
    /// View → EXIF Window is open.
    pub(crate) exif_window: bool,
    /// Keywords, comment, alt text and extended description in the EXIF panel; saved when a field
    /// loses the focus, or another image is shown.
    pub(crate) meta: Option<MetaEdit>,
    /// Alt+K: put the focus in the keywords field when the panel shows next.
    pub(crate) focus_keywords: bool,
    /// The keyword tree (Geeqie's), as paths "Places|Norway|Oslo", sorted; in the EXIF panel.
    pub(crate) keyword_tree: Vec<String>,
    /// A path being typed to add to the keyword tree.
    pub(crate) new_keyword: String,
}

impl Info {
    /// The EXIF reader started, and the keyword tree read from its file.
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        let (exif_jobs, exif_rx) = spawn_exif_reader(ctx);
        Info {
            exif_jobs,
            exif_rx,
            exif_asked: None,
            exif: None,
            exif_all: Vec::new(),
            exif_window: false,
            meta: None,
            focus_keywords: false,
            keyword_tree: (keyword_tree_path().and_then(|p| crate::decode::read_text(p).ok()))
                .map_or_else(Vec::new, |t| xmp::keyword_paths(t.lines())),
            new_keyword: String::new(),
        }
    }
}

impl crate::App {
    // ---- EXIF, keywords, comment and marks ----

    /// Ask the EXIF reader for the shown image (once per image) and take what has arrived.
    pub(crate) fn want_exif(&mut self, ctx: &egui::Context) {
        let path = self.path(self.list.cur);
        if self.info.exif_asked.as_ref() != Some(&path) {
            let _ = self.info.exif_jobs.send(path.clone());
            self.info.exif_asked = Some(path);
        }
        let done: Vec<ExifRead> = self.info.exif_rx.try_iter().collect();
        for ExifRead {
            path,
            rows,
            all,
            meta,
            paths,
            access,
        } in done
        {
            // Kept while being edited: the reader may answer for the image again.
            if self.info.meta.as_ref().is_none_or(|m| m.path != path) {
                self.save_meta(ctx);
                self.info.meta = Some(MetaEdit {
                    path: path.clone(),
                    keywords: meta.keywords_text(),
                    comment: meta.comment.clone(),
                    saved: meta,
                    paths,
                    alt: access.alt.clone(),
                    extended: access.extended.clone(),
                    access,
                });
            }
            self.info.exif = Some((path, rows));
            self.info.exif_all = all;
        }
    }

    /// File and EXIF details of the shown image, read in the background (Ctrl+K).
    pub(crate) fn exif_panel(&mut self, ui: &mut egui::Ui) {
        self.want_exif(ui.ctx());
        egui::Panel::right("exif")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("exif rows").num_columns(2).show(ui, |ui| {
                        ui.weak(tr("File"));
                        ui.label(self.name(self.list.cur));
                        ui.end_row();
                        if let Some(s) = self.list.slots.get(&self.list.cur) {
                            ui.weak(tr("Dimensions"));
                            ui.label(format!("{} × {}", s.orig[0], s.orig[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not
                        // flicker.
                        for (label, value) in self.info.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(*label);
                            // The place opens on a map in the browser.
                            if *label == "GPS" {
                                let link = ui.link(value).on_hover_text(tr("Show on OpenStreetMap"));
                                if link.clicked()
                                    && let Some((lat, lon)) = value.split_once(", ")
                                {
                                    open_url(&format!(
                                        "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=15/{lat}/{lon}"
                                    ));
                                }
                            } else {
                                ui.label(value);
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
        let path = self.path(self.list.cur);
        let Some(edit) = self.info.meta.as_mut().filter(|m| m.path == path) else {
            return;
        };
        ui.weak(tr("Keywords"));
        let keywords = ui.add(
            egui::TextEdit::singleline(&mut edit.keywords)
                .hint_text(tr("Separated by commas"))
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.info.focus_keywords) {
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
            egui::TextEdit::multiline(&mut edit.alt)
                .hint_text(tr("What the picture shows, for someone who cannot see it"))
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.weak(tr("Extended description"));
        let extended = ui.add(
            egui::TextEdit::multiline(&mut edit.extended)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let add = Meta::parse_keywords(&edit.keywords);
        let n = self.list.selected.len();
        if n > 1
            && !add.is_empty()
            && ui
                .button(tf!(
                    "Add the keywords to the {n} selected",
                    n = thousands(n)
                ))
                .on_hover_text(tr("Each file keeps its own keywords and gets these too"))
                .clicked()
        {
            self.add_keywords(ui.ctx(), add);
        }
        if [keywords, comment, alt, extended]
            .iter()
            .any(egui::Response::lost_focus)
        {
            self.save_meta(ui.ctx());
        }
    }

    /// The keyword tree as in Geeqie's sidebar: a box for each keyword, ticked when the shown
    /// image has it; a click sets it on the selected images (or the shown one), or takes it off.
    pub(crate) fn keyword_tree(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.list.cur);
        let Some((have, paths)) = (self.info.meta.as_ref())
            .filter(|m| m.path == path)
            .map(|m| (m.saved.keywords.clone(), m.paths.clone()))
        else {
            return;
        };
        ui.separator();
        ui.weak(tr("Keyword tree"));
        let mut root = KeywordNode::default();
        for p in &self.info.keyword_tree {
            root.insert(p);
        }
        let (mut tick, mut drop) = (None, None);
        root.show(ui, "", (&have, &paths), &mut tick, &mut drop);
        // The button first, from the right, so the field takes exactly what is left: a field
        // too wide makes the panel grow a little on every frame.
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        let layout = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            let add = ui.button(tr("Add")).clicked();
            let field = egui::TextEdit::singleline(&mut self.info.new_keyword)
                .hint_text(tr("Places|Norway|Oslo"))
                .desired_width(ui.available_width());
            let enter = ui.add(field).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (add || enter) && !self.info.new_keyword.trim().is_empty() {
                let new = std::mem::take(&mut self.info.new_keyword);
                self.change_keyword_tree(vec![new], None);
            }
        });
        // The image's own keywords that the tree does not have yet, at its top.
        let names: HashSet<String> = (self.info.keyword_tree.iter())
            .map(|p| p.rsplit('|').next().unwrap_or(p).to_lowercase())
            .collect();
        let missing: Vec<String> = (have.iter())
            .filter(|k| !names.contains(&k.to_lowercase()))
            .cloned()
            .collect();
        if !missing.is_empty()
            && ui
                .small_button(tr("Add this image's keywords to the tree"))
                .clicked()
        {
            self.change_keyword_tree(missing, None);
        }
        if let Some(p) = drop {
            self.change_keyword_tree(Vec::new(), Some(p));
        }
        if let Some((tag, on)) = tick {
            self.tag_files(ui.ctx(), tag, on);
        }
    }

    /// Add `add` to the keyword tree in `$XDG_CONFIG_HOME/glimt/keywords`, or take `remove` and
    /// the paths below it out: this change alone, on the file as it is now, so what another
    /// window changed meanwhile stays.
    pub(crate) fn change_keyword_tree(&mut self, add: Vec<String>, remove: Option<String>) {
        // Locked settings (--lock-settings, a kiosk): the tree changes for this session only,
        // as the other settings do, and the file is not written.
        let Some(p) = keyword_tree_path().filter(|_| !config::locked()) else {
            self.info.keyword_tree =
                xmp::keyword_tree_changed(&self.info.keyword_tree, add, remove);
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
            Ok(tree) => self.info.keyword_tree = tree,
            Err(e) => self.notice = Some(tf!("Could not save the keyword tree: {e}", e)),
        }
    }

    /// Set the keyword `tag` (a tree path) on the selected images, or the shown one, or take it
    /// off, in the background; the shown image's keywords show it at once.
    pub(crate) fn tag_files(&mut self, ctx: &egui::Context, tag: String, on: bool) {
        self.save_meta(ctx);
        let targets = self.targets();
        if targets.contains(&self.list.cur)
            && let Some(edit) = &mut self.info.meta
        {
            xmp::set_tag(&mut edit.saved.keywords, &mut edit.paths, &tag, on);
            edit.keywords = edit.saved.keywords_text();
        }
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let (doing, verb) = match on {
            true => (tr("Setting keywords"), tr("set on")),
            false => (tr("Taking keywords off"), tr("taken off")),
        };
        let (n, written) = (paths.len(), tag.clone());
        self.write_sidecars(
            ctx,
            doing,
            paths,
            move |path| xmp::tag_file(path, &written, on).is_ok(),
            move |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "The keyword could not be saved for {count}",
                    count = count(failed, "file")
                )),
                _ if job.stopped() => Some(job.stopped_text()),
                _ if n > 1 => Some(format!("{tag} {verb} {count}", count = count(n, "file"))),
                _ => None,
            },
        );
    }

    /// Save the keywords, comment, alt text and extended description being edited, if they changed:
    /// in the thread that writes this window's sidecars, which closing waits for, not in the
    /// window's, as a sidecar on a slow share would hold the window up for its read, write and
    /// sync. The fields show what was saved at once; a failure is told.
    pub(crate) fn save_meta(&mut self, ctx: &egui::Context) {
        let Some(edit) = &mut self.info.meta else {
            return;
        };
        let access = xmp::Access {
            alt: edit.alt.trim().to_owned(),
            extended: edit.extended.trim().to_owned(),
        };
        // Saved only when edited, as the keywords: browsing writes nothing.
        if (edit.alt != edit.access.alt || edit.extended != edit.access.extended)
            && access != edit.access
        {
            (edit.alt, edit.extended) = (access.alt.clone(), access.extended.clone());
            edit.access = access.clone();
            let (path, tx) = (edit.path.clone(), self.notices_tx.clone());
            // A sidecar job like the keywords': Move, Rename and Delete wait for it.
            self.write_sidecars(
                ctx,
                tr("Saving the alt text"),
                vec![path],
                move |p| match xmp::write_access(p, &access) {
                    Ok(()) => true,
                    Err(e) => {
                        let _ = tx.send(tf!("Could not save the alt text: {e}", e));
                        false
                    }
                },
                |_, _| None,
            );
        }
        let Some(edit) = &mut self.info.meta else {
            return;
        };
        // Fields as they were shown are no change, also when splitting them again at commas
        // would read them differently ("Smith, John" as one keyword, a comment's last newline).
        if edit.keywords == edit.saved.keywords_text() && edit.comment == edit.saved.comment {
            return;
        }
        // Only what was edited: keywords set on this file in the background meanwhile stay.
        let keywords = (edit.keywords != edit.saved.keywords_text())
            .then(|| Meta::parse_keywords(&edit.keywords));
        let comment = (edit.comment != edit.saved.comment).then(|| edit.comment.trim().to_owned());
        if let Some(k) = &keywords {
            edit.saved.keywords = k.clone();
        }
        if let Some(c) = &comment {
            edit.saved.comment = c.clone();
        }
        edit.keywords = edit.saved.keywords_text();
        edit.comment = edit.saved.comment.clone();
        let (path, tx) = (edit.path.clone(), self.notices_tx.clone());
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
        // The shown image's own edit first; it is read again afterwards, with the added ones.
        self.save_meta(ctx);
        self.info.meta = None;
        self.info.exif_asked = None;
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
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
                _ if job.stopped() => Some(job.stopped_text()),
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
        if self.info.exif_window && !self.list.files.is_empty() {
            self.want_exif(ctx);
            let mut open = true;
            let title = format!("EXIF: {}", self.name(self.list.cur));
            egui::Window::new(title)
                .id(egui::Id::unique("exif window"))
                .open(&mut open)
                .default_size([560.0, 600.0])
                .vscroll(true)
                .show(ctx, |ui| {
                    if self.info.exif_all.is_empty() {
                        ui.weak(tr("No EXIF data"));
                    }
                    egui::Grid::new("exif all")
                        .striped(true)
                        .num_columns(3)
                        .show(ui, |ui| {
                            for (group, name, value) in &self.info.exif_all {
                                ui.weak(*group);
                                ui.label(name);
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                });
            self.info.exif_window = open;
        }
    }
}

/// The EXIF reader: one thread that skips to the newest request, so fast browsing never
/// queues up; it sends the panel's rows, every field, and the keywords and comment.
pub(crate) fn spawn_exif_reader(
    ctx: &egui::Context,
) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<ExifRead>) {
    let (exif_jobs, paths_rx) = mpsc::channel::<PathBuf>();
    let (exif_tx, exif_rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        while let Ok(mut path) = paths_rx.recv() {
            path = paths_rx.try_iter().last().unwrap_or(path);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            let mut rows = vec![("File size", human_size(size))];
            let (mut all, mut meta, mut paths, mut access) = Default::default();
            // Not opened unless it is a file: a pipe would block this thread for good. A decoder
            // that panics on the file leaves the panel without EXIF for it, not for good.
            if decode::is_file(&path) {
                let read = std::panic::catch_unwind(|| {
                    let mut rows = vec![("Colour profile", colour_source(&path))];
                    let (exif_rows, all) = exif::read_and_all(&path);
                    rows.extend(exif_rows);
                    let (meta, paths, access) = xmp::read_all(&path);
                    (rows, all, meta, paths, access)
                });
                if let Ok((more, a, m, p, x)) = read {
                    rows.extend(more);
                    (all, meta, paths, access) = (a, m, p, x);
                }
            }
            let read = ExifRead {
                path,
                rows,
                all,
                meta,
                paths,
                access,
            };
            if exif_tx.send(read).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    (exif_jobs, exif_rx)
}
