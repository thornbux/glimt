//! The info panel (Ctrl+K) and what it edits: EXIF, keywords and the keyword tree, comment, alt
//! text, and marks.

use crate::files::config::keyword_tree_path;
use crate::files::external::open_url;
use crate::files::file_io::{Identity, ListedFile, is_another_file};
use crate::images::colour::colour_source;
#[cfg(feature = "marks")]
use crate::metadata::marks;
use crate::metadata::xmp::{KeywordChange, KeywordsAndComment};
use crate::support::format::{count, human_size, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took};
use crate::ui::dialogs::entered;
use crate::{files::config, metadata::exif, metadata::xmp, tf};
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// A file's EXIF from the reader thread.
pub(crate) struct ExifRead {
    path: PathBuf,
    /// The identity of the file at the path, taken before it was read.
    identity: Identity,
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
    /// The identity of the file it was read from, when the list knows the file by one. Its
    /// save is refused when another file has taken the name since.
    pub(crate) identity: Identity,
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

/// A keyword of the keyword tree, by its whole path, and the keywords below it by name. The
/// root has an empty path.
#[derive(Default)]
pub(crate) struct KeywordNode {
    path: String,
    children: std::collections::BTreeMap<String, KeywordNode>,
}

impl KeywordNode {
    /// Builds the tree of the keyword tree's `paths`, with a keyword for each level of a path
    /// also when the tree has no line of its own for it.
    fn of(paths: &[String]) -> Self {
        let mut root = KeywordNode::default();
        for path in paths {
            let mut node = &mut root;
            for level in path.split('|') {
                let whole = match node.path.is_empty() {
                    true => level.to_owned(),
                    false => format!("{}|{level}", node.path),
                };
                node = (node.children.entry(level.to_owned())).or_insert_with(|| KeywordNode {
                    path: whole,
                    children: Default::default(),
                });
            }
        }
        root
    }

    /// The rows of the tree as it is shown: each keyword with its depth and name, and the
    /// keywords below one only when it is in `open`.
    fn rows<'a>(&'a self, open: &HashSet<String>) -> Vec<(usize, &'a str, &'a KeywordNode)> {
        let mut rows = Vec::new();
        let mut stack: Vec<_> = (self.children.iter().rev())
            .map(|(name, node)| (0, name.as_str(), node))
            .collect();
        while let Some((depth, name, node)) = stack.pop() {
            rows.push((depth, name, node));
            if open.contains(&node.path) {
                let below = node.children.iter().rev();
                stack.extend(below.map(|(name, child)| (depth + 1, name.as_str(), child)));
            }
        }
        rows
    }
}

/// The info panel and the EXIF window: what is read of the shown image, and what is being edited.
pub(crate) struct InfoPanel {
    /// To the EXIF reader thread, and its answers.
    exif_requests: mpsc::Sender<PathBuf>,
    exif_answers: mpsc::Receiver<ExifRead>,
    /// The file the info panel asked for last, with the identity the list knew it by then, and
    /// the panel's rows as read for a file.
    exif_asked: Option<(PathBuf, Identity)>,
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
    /// The keyword tree's levels, built when the tree changes and not in every frame, because
    /// a Lightroom import can give it thousands of keywords.
    keyword_nodes: KeywordNode,
    /// The last name of each path in the keyword tree, in lower case, built with the levels.
    keyword_names: HashSet<String>,
    /// The paths of the keywords opened in the tree, which show the keywords below them.
    open_keywords: HashSet<String>,
    /// A path being typed to add to the keyword tree.
    new_keyword: String,
    /// The keyword tree as its file has it once this window's last change is written, with
    /// what other windows wrote there meanwhile.
    keyword_tree_written: Receiving<Vec<String>>,
}

impl InfoPanel {
    /// The EXIF reader started, with the keyword tree as `main` read it (`read_keyword_tree`).
    pub(crate) fn new(ctx: &egui::Context, keyword_tree: Vec<String>) -> Self {
        let (exif_requests, exif_answers) = spawn_exif_reader(ctx);
        let mut panel = InfoPanel {
            exif_requests,
            exif_answers,
            exif_asked: None,
            exif: None,
            exif_all: Vec::new(),
            reread_after_write: None,
            is_exif_window_open: false,
            metadata_edit: None,
            keywords_take_focus: false,
            keyword_tree: Vec::new(),
            keyword_nodes: KeywordNode::default(),
            keyword_names: HashSet::new(),
            open_keywords: HashSet::new(),
            new_keyword: String::new(),
            keyword_tree_written: Receiving::none(),
        };
        panel.set_keyword_tree(keyword_tree);
        panel
    }

    /// Takes the keyword tree as its file has it once this window's last change is written.
    /// So another window's keywords show here after this window's next change, as they did
    /// when the window wrote the file itself.
    pub(crate) fn receive_keyword_tree(&mut self) {
        if let Took::Answer(tree) = self.keyword_tree_written.take() {
            self.set_keyword_tree(tree);
        }
    }

    /// Makes `tree` the keyword tree, and builds its levels and names once for the frames that
    /// draw it.
    pub(crate) fn set_keyword_tree(&mut self, tree: Vec<String>) {
        self.keyword_nodes = KeywordNode::of(&tree);
        self.keyword_names = (tree.iter())
            .map(|p| p.rsplit('|').next().unwrap_or(p).to_lowercase())
            .collect();
        self.keyword_tree = tree;
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
        // A file whose name another file has taken since is another image, read anew.
        let identity = self.list.identity(self.list.shown());
        let is_another = |file: &Path, of: Identity| file != path || is_another_file(of, identity);
        // The edit of the image before is saved as soon as another image is shown, before this
        // one's read is asked for. Saved only at the next answer, it could still wait when the
        // user came back, and the panel then read the file as it was before the edit.
        if (self.info_panel.metadata_edit.as_ref()).is_some_and(|e| is_another(&e.path, e.identity))
        {
            self.save_metadata_edit(ctx);
        }
        let asked_again =
            (self.info_panel.exif_asked.as_ref()).is_none_or(|(file, of)| is_another(file, *of));
        // An image shown again while its own save still waits behind a long job (a Lightroom
        // import) is read once the save is written, or the panel would show it without the save.
        if asked_again && self.info_panel.reread_after_write.is_none() {
            let writes = self.file_operations.jobs_writing_sidecar_of(&path);
            if !writes.is_empty() {
                // The wait ends by forgetting the panel, so the image before keeps its edit.
                self.save_metadata_edit(ctx);
                self.info_panel.reread_after_write = Some((path.clone(), writes));
            }
        }
        if asked_again && self.info_panel.reread_after_write.is_none() {
            let _ = self.info_panel.exif_requests.send(path.clone());
            self.info_panel.exif_asked = Some((path, identity));
        }
        let done: Vec<ExifRead> = self.info_panel.exif_answers.try_iter().collect();
        for ExifRead {
            path,
            identity: read_identity,
            rows,
            all,
            meta,
            paths,
            alt_text,
        } in done
        {
            // A file the list knows by no identity (on a FAT card, say) is not told apart by the
            // one read, which such a file system may make up anew at each look.
            let read_identity = read_identity.filter(|_| identity.is_some());
            // Kept while being edited: the reader may answer for the image again. An answer of
            // another file under the name replaces it.
            if (self.info_panel.metadata_edit.as_ref())
                .is_none_or(|m| m.path != path || is_another_file(m.identity, read_identity))
            {
                self.save_metadata_edit(ctx);
                self.info_panel.metadata_edit = Some(MetadataEdit {
                    path: path.clone(),
                    identity: read_identity,
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
        let (mut tick, mut drop, mut toggled) = (None, None, None);
        let panel = &self.info_panel;
        // shortcut: the open rows are listed again in each frame, without a String for each; keep
        // them until the tree or an opened keyword changes if trees of millions are opened.
        let rows = panel.keyword_nodes.rows(&panel.open_keywords);
        // Only the rows on screen are laid out, in a box as high as the panel shows.
        let row_h = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("keyword tree")
            .max_height(ui.clip_rect().height().max(8.0 * row_h))
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for &(depth, name, node) in &rows[range] {
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * ui.spacing().indent);
                        if node.children.is_empty() {
                            ui.add_space(ui.spacing().icon_width);
                        } else {
                            let open = panel.open_keywords.contains(&node.path);
                            if crate::ui::menus::caret(ui, open, name).clicked() {
                                toggled = Some(node.path.clone());
                            }
                        }
                        let mut on = xmp::has_tag(&have, &paths, &node.path);
                        let response = crate::ui::menus::checkbox(ui, &mut on, name);
                        if response.changed() {
                            tick = Some((node.path.clone(), on));
                        }
                        response.context_menu(|ui| {
                            let remove = tr("Remove from the tree");
                            if crate::ui::menus::button(ui, remove).clicked() {
                                drop = Some(node.path.clone());
                            }
                        });
                    });
                }
            });
        if let Some(path) = toggled
            && !self.info_panel.open_keywords.remove(&path)
        {
            self.info_panel.open_keywords.insert(path);
        }
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
        let names = &self.info_panel.keyword_names;
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
    /// window changed meanwhile stays. The window's tree changes at once, and the file in the
    /// writer's thread, as a home on a share that stopped answering would hold the window.
    pub(crate) fn change_keyword_tree(&mut self, add: Vec<String>, remove: Option<String>) {
        let tree =
            xmp::keyword_tree_changed(&self.info_panel.keyword_tree, add.clone(), remove.clone());
        self.info_panel.set_keyword_tree(tree);
        // Locked settings (--lock-settings, a kiosk): the tree changes for this session only,
        // as the other settings do, and the file is not written.
        let Some(p) = keyword_tree_path().filter(|_| !config::locked()) else {
            return;
        };
        // The tree as written comes back, with what other windows wrote there meanwhile.
        let (tx, rx) = mpsc::channel();
        self.info_panel.keyword_tree_written = Receiving::of(rx);
        let ctx = self.ctx.clone();
        crate::files::file_io::write_later(move || {
            // A file that cannot be read is not written over, because its keywords would be
            // lost.
            let change = |old: &[u8]| xmp::keyword_tree_file_changed(old, add, remove);
            match crate::files::file_io::edit_shared(&p, change) {
                Ok(tree) => {
                    let _ = tx.send(tree);
                    ctx.request_repaint();
                }
                Err(e) => crate::files::file_io::save_failed(tf!(
                    "Could not save the keyword tree: {e}",
                    e
                )),
            }
        });
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
        let files: Vec<_> = targets.iter().map(|&i| (self.listed_file(i), ())).collect();
        let activity = match on {
            true => tr("Setting keywords"),
            false => tr("Taking keywords off"),
        };
        let (n, written) = (files.len(), tag.clone());
        self.write_sidecars(
            ctx,
            activity,
            files,
            move |path, _| xmp::tag_file(path, &written, on),
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
            let file = ListedFile::new(edit.path.clone(), edit.identity, self.folder.list_is_old());
            // A sidecar job like the keywords': Move, Rename and Delete wait for it.
            self.write_sidecars(
                ctx,
                tr("Saving the alt text"),
                vec![(file, ())],
                move |p, _| {
                    xmp::write_alt_text(p, &alt_text)
                        .map_err(|e| tf!("Could not save the alt text: {e}", e))
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
        // Only what was edited is sent: the keywords added and taken out, not the whole list.
        // The writer applies them to the file as it is then, so keywords that another window or
        // a background job set since the panel read it stay. The change is made from the saved
        // keywords themselves, so one that holds a comma ("Smith, John") stays whole when it was
        // not touched, and is taken out whole when it was.
        let keywords = (edit.keywords != edit.saved.keywords_text()).then(|| {
            KeywordChange::between(
                &edit.saved.keywords,
                &KeywordsAndComment::parse_keywords(&edit.keywords),
            )
        });
        let comment = (edit.comment != edit.saved.comment).then(|| edit.comment.trim().to_owned());
        if let Some(change) = &keywords {
            change.apply(&mut edit.saved.keywords);
        }
        if let Some(c) = &comment {
            edit.saved.comment = c.clone();
        }
        edit.keywords = edit.saved.keywords_text();
        edit.comment = edit.saved.comment.clone();
        let file = ListedFile::new(edit.path.clone(), edit.identity, self.folder.list_is_old());
        // A sidecar job, so that a Move, Rename or Delete of the file waits until it has
        // landed, instead of leaving the new keywords behind under the old name.
        self.write_sidecars(
            ctx,
            tr("Saving keywords"),
            vec![(file, ())],
            move |p, _| match xmp::write_changed(p, keywords.clone(), comment.clone()) {
                Ok(_) => Ok(()),
                Err(e) => Err(tf!("Could not save the keywords: {e}", e)),
            },
            |_, _| None,
        );
    }

    /// Saves the panel's edit under the new name of its file, when a move or rename in `moved`
    /// took the file there, and lets the panel read the file again under that name. Without
    /// this, an edit typed while Rename Many swapped two names was saved under the old name,
    /// which then belonged to the other photo.
    pub(crate) fn follow_metadata_edit(
        &mut self,
        ctx: &egui::Context,
        moved: &std::collections::HashMap<&Path, &Path>,
    ) {
        let Some(edit) = &mut self.info_panel.metadata_edit else {
            return;
        };
        let Some(to) = crate::files::file_ops::new_place(&edit.path, moved) else {
            return;
        };
        edit.path = to;
        self.save_metadata_edit(ctx);
        self.info_panel.metadata_edit = None;
        self.info_panel.exif = None;
        self.info_panel.exif_asked = None;
        self.info_panel.reread_after_write = None;
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
        let files: Vec<_> = targets.iter().map(|&i| (self.listed_file(i), ())).collect();
        let n = files.len();
        // Through the sidecar writer, with keywords ticked and stars: in the order asked.
        self.write_sidecars(
            ctx,
            tr("Adding keywords"),
            files,
            move |path, _| xmp::add_keywords(path, &keywords),
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
    // No busy pointer: the EXIF reader, which lasts as long as the window. The panel
    // shows each answer when it comes.
    std::thread::spawn(move || {
        while let Ok(mut path) = paths_rx.recv() {
            path = paths_rx.try_iter().last().unwrap_or(path);
            let identity = crate::files::file_io::identity_of(&path);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            let mut rows = vec![("File size", human_size(size))];
            let (mut all, mut meta, mut paths, mut alt_text) = Default::default();
            // Not opened unless it is a file: a pipe would block this thread for good. A decoder
            // that panics on the file leaves the panel without EXIF for it, not for good.
            if crate::files::file_io::is_surely_file(&path) {
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
                identity,
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
/// Returns the keyword tree as its file has it. It waits for the disk, so `main` calls it before
/// the window opens.
pub(crate) fn read_keyword_tree() -> Vec<String> {
    // Parsed by `keyword_tree_of`, which xmp's survives_broken_keyword_tree feeds broken input.
    (keyword_tree_path().and_then(|p| crate::images::decode::read_limited(p).ok()))
        .map_or_else(Vec::new, |file| xmp::keyword_tree_of(&file))
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, run_background_until};

    /// Another window's keyword, written to the tree's file meanwhile, shows in this window's
    /// tree once this window's own change is written, as it did when the window's thread wrote
    /// the file and took the tree it wrote.
    #[test]
    fn another_windows_keywords_show_after_a_change() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-kw-other-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (other, mine) = (format!("GlimtTestOther{id}"), format!("GlimtTestMine{id}"));
        let file = crate::files::config::keyword_tree_path().unwrap();
        let change = |add: Vec<String>, remove: Option<String>| {
            let edit =
                |old: &[u8]| crate::metadata::xmp::keyword_tree_file_changed(old, add, remove);
            crate::files::file_io::edit_shared(&file, edit).unwrap();
        };
        change(vec![other.clone()], None);
        app.change_keyword_tree(vec![mine.clone()], None);
        let has_both = |a: &crate::App| {
            let tree = &a.info_panel.keyword_tree;
            tree.contains(&other) && tree.contains(&mine)
        };
        let shown = run_background_until(&mut app, &ctx, has_both);
        crate::files::file_io::flush();
        change(Vec::new(), Some(other.clone()));
        change(Vec::new(), Some(mine.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(shown, "the other window's keyword is not in the tree");
    }

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

    /// A keyword tree of thousands of keywords, as a Lightroom import makes, is laid out only
    /// where it is seen. It was laid out whole in every frame, a checkbox for each keyword,
    /// which made each frame slow while the info panel was open.
    #[test]
    fn a_large_keyword_tree_is_laid_out_only_where_it_is_seen() {
        use eframe::egui;
        let dir = std::env::temp_dir().join(format!("glimt-test-kw-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        read_panel(&mut app, &ctx);
        let tree: Vec<String> = (0..10_000).map(|i| format!("Keyword {i:05}")).collect();
        app.info_panel.set_keyword_tree(tree);
        let mut height = 0.0;
        let _ = ctx.run_ui(Default::default(), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                app.keyword_tree(ui);
                height = ui.min_rect().height();
            });
        });
        let row = ctx.global_style().spacing.interact_size.y;
        assert!(height > 0.0 && height < 2_000.0 * row, "{height}");
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

    /// Another window's Rename Many that swaps two names leaves the same names in the folder.
    /// Once the folder is read again, the panel reads the shown name again, as it holds the
    /// other photo now. It kept the keywords it had read of the photo before, and an edit of
    /// them was saved onto the other photo.
    #[test]
    fn the_panel_reads_again_a_name_that_another_file_has_taken() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kw-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, keyword) in [("a.png", "first"), ("b.png", "second")] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
            crate::metadata::xmp::add_keywords(&dir.join(name), &[keyword.into()]).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let before = app.list.identity(0);
        assert!(before.is_some(), "the listing gave no identity");
        read_panel(&mut app, &ctx);
        let keywords = |a: &crate::App| {
            let edit = a.info_panel.metadata_edit.as_ref();
            edit.map(|e| e.saved.keywords.clone())
        };
        assert_eq!(keywords(&app), Some(vec!["first".to_owned()]));
        let side = crate::metadata::xmp::sidecar;
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        for (from, to) in [(&a, &c), (&b, &a), (&c, &b)] {
            std::fs::rename(from, to).unwrap();
            std::fs::rename(side(from), side(to)).unwrap();
        }
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| a.list.identity(0) != before));
        let read_again = (0..500).any(|_| {
            app.request_and_take_exif(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
            keywords(&app) == Some(vec!["second".to_owned()])
        });
        assert!(
            read_again,
            "the panel shows the keywords of the photo before"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An edit typed in the panel while Rename Many swaps two names is saved to its photo under
    /// the new name. Before, it was saved under the old name, which then held the other photo.
    #[test]
    fn an_edit_follows_its_photo_through_a_rename() {
        let d = crate::support::test_support::contract_folder("panel-edit-rename");
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        // The fixture's sidecars only say whose they are, which is not XMP to write into.
        for p in [&a, &b] {
            std::fs::remove_file(crate::metadata::xmp::sidecar(p)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        app.show_file(&ctx, 0);
        read_panel(&mut app, &ctx);
        assert_eq!(app.info_panel.metadata_edit.as_ref().unwrap().path, a);
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![a.clone(), b.clone()]),
            vec![b.clone(), a.clone()],
            crate::files::file_io::PickedFiles::new(vec![a.clone(), b.clone()]),
        );
        app.info_panel.metadata_edit.as_mut().unwrap().comment = "Red".into();
        let comment = |p: &std::path::Path| crate::metadata::xmp::read(p).comment;
        assert!(run_background_until(&mut app, &ctx, |_| comment(&b) == "Red"));
        assert_eq!(comment(&a), "");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Coming back to an image whose edit was not saved yet, while the next image is still being
    /// read, shows the edit. Before, the edit was saved only when the next image's answer came,
    /// after this image's read was asked for, so the panel showed the file as it was before.
    #[test]
    fn an_edit_is_saved_when_its_image_is_left() {
        let dir = std::env::temp_dir().join(format!("glimt-test-edit-left-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        for p in [&a, &dir.join("b.png")] {
            image::RgbImage::new(4, 4).save(p).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        app.show_file(&ctx, 0);
        read_panel(&mut app, &ctx);
        // A long sidecar job of another file holds the one sidecar thread, so the save of the
        // edit cannot land before the panel reads the image again, however fast the computer.
        let (release, released) = std::sync::mpsc::channel::<()>();
        let other = vec![(dir.join("c.png"), ())];
        app.write_sidecars(
            &ctx,
            "Other",
            crate::support::test_support::unlisted_with(other),
            move |_, _| released.recv().map_or(Ok(()), |()| Err(String::new())),
            |_, _| None,
        );
        app.info_panel.metadata_edit.as_mut().unwrap().alt_text = "Red".into();
        app.show_file(&ctx, 1);
        app.request_and_take_exif(&ctx);
        app.show_file(&ctx, 0);
        app.request_and_take_exif(&ctx);
        drop(release);
        let shows_a = |app: &crate::App| {
            !app.file_operations.is_writing_sidecars()
                && app.info_panel.reread_after_write.is_none()
                && (app.info_panel.metadata_edit.as_ref()).is_some_and(|edit| edit.path == a)
        };
        for _ in 0..500 {
            app.request_and_take_exif(&ctx);
            if shows_a(&app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(shows_a(&app), "the panel never read the image again");
        let edit = app.info_panel.metadata_edit.as_ref().unwrap();
        assert_eq!(edit.saved_alt_text.text, "Red");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A keyword written to the shown file behind the panel's back, as another window or a
    /// Lightroom import does, stays when the user then edits the keywords field.
    #[test]
    fn an_edit_in_the_panel_keeps_keywords_written_meanwhile() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kw-behind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&file).unwrap();
        let (mut app, ctx) = app_on(&dir);
        read_panel(&mut app, &ctx);
        crate::metadata::xmp::add_keywords(&file, &["Oslo".into()]).unwrap();
        app.info_panel.metadata_edit.as_mut().unwrap().keywords = "beach".into();
        app.save_metadata_edit(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| {
            !a.file_operations.is_writing_sidecars()
        }));
        assert_eq!(
            crate::metadata::xmp::read(&file).keywords,
            ["Oslo", "beach"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
        let b = vec![(dir.join("b.png"), ())];
        app.write_sidecars(
            &ctx,
            "Other",
            crate::support::test_support::unlisted_with(b),
            move |_, _| hold.recv().map_or(Ok(()), |()| Err(String::new())),
            |_, _| None,
        );
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
        assert_ne!(app.info_panel.exif_asked.as_ref().map(|a| &a.0), Some(&a));
        drop(writing);
        app.request_and_take_exif(&ctx);
        assert!(app.info_panel.reread_after_write.is_none());
        assert_eq!(app.info_panel.exif_asked.as_ref().map(|a| &a.0), Some(&a));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
