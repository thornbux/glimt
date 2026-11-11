//! Lightroom Classic catalogs (`.lrcat`, an SQLite database), read once through the `sqlite3`
//! program: the collections, the keyword tree, the stars and each image's keywords, for Glimt's
//! own files. Only columns that have been the same for many Lightroom versions are read. Also
//! File → Import Lightroom Catalog's dialog, what it will do (`Catalog::plan`) and the import.

use crate::files::file_io::ListedFile;
#[cfg(feature = "marks")]
use crate::metadata::marks;
use crate::model::listing::{Types, is_listed};
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::{metadata::xmp, model::collections, tf};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The tables `QUERY` reads, in lower case as SQLite matches names.
const TABLES: &str = "'aglibraryrootfolder', 'adobe_images', 'aglibraryfile', 'aglibraryfolder', \
    'aglibrarycollection', 'aglibrarycollectionimage', 'aglibrarykeyword', 'aglibrarykeywordimage'";

/// One query, so the catalog is read in one go: a row for each root folder (r), image (i),
/// collection (c), image in a collection (m), keyword (k) and keyword of an image (w), with five
/// columns each.
const QUERY: &str = "\
SELECT 'r', id_local, absolutePath, '', '' FROM AgLibraryRootFolder
UNION ALL SELECT 'i', i.id_local, f.rootFolder,
    f.pathFromRoot || fi.baseName || CASE WHEN fi.extension = '' THEN '' ELSE '.' || fi.extension END,
    CAST(coalesce(i.rating, 0) AS INTEGER)
    FROM Adobe_images i JOIN AgLibraryFile fi ON fi.id_local = i.rootFile
    JOIN AgLibraryFolder f ON f.id_local = fi.folder
UNION ALL SELECT 'c', id_local, coalesce(parent, 0), coalesce(name, ''), coalesce(creationId, '')
    FROM AgLibraryCollection
UNION ALL SELECT 'm', collection, image, '', '' FROM AgLibraryCollectionImage
UNION ALL SELECT 'k', id_local, coalesce(parent, 0), coalesce(name, ''), '' FROM AgLibraryKeyword
UNION ALL SELECT 'w', image, tag, '', '' FROM AgLibraryKeywordImage;";

/// sqlite3's options for reading a catalog. -init /dev/null: the user's sqliterc (.mode json,
/// .output /dev/null) would change what is printed, and the import would find nothing in it.
const ARGS: [&str; 11] = [
    "-init",
    "/dev/null",
    "-safe",
    "-readonly",
    "-bail",
    "-batch",
    "-noheader",
    "-separator",
    "\x1f",
    "-newline",
    "\x1e",
];

/// Between the columns and after the rows: neither is in a file name or keyword.
const COLUMN_END: u8 = 0x1f;

const ROW_END: u8 = 0x1e;

const NEEDS_SQLITE3: &str = "Reading a Lightroom catalog needs the sqlite3 program";

#[derive(Default)]
pub struct Catalog {
    /// The root folders as the catalog names them, with a slash at the end.
    pub roots: Vec<(i64, String)>,
    /// Image: (root folder, path below it, stars).
    images: HashMap<i64, (i64, String, u8)>,
    /// Collection: (parent, name, kind).
    collections: HashMap<i64, (i64, String, String)>,
    /// (collection, image)
    collection_images: Vec<(i64, i64)>,
    /// Keyword: (parent, name).
    keywords: HashMap<i64, (i64, String)>,
    /// (image, keyword)
    image_keyword_ids: Vec<(i64, i64)>,
    /// The tables read that are something else: (name, type).
    not_tables: Vec<(String, String)>,
}

impl Catalog {
    /// The path of each image, its root folder replaced by `now`, the folder where the user
    /// says it is now (in the order of `roots`).
    pub fn paths(&self, now: &[PathBuf]) -> HashMap<i64, PathBuf> {
        // A root folder the user gave no place is left out.
        let roots: HashMap<i64, &PathBuf> = (self.roots.iter().map(|r| r.0))
            .zip(now)
            .filter(|r| !r.1.as_os_str().is_empty())
            .collect();
        // Only a path below the root: one with .. or an absolute one, in a catalog from
        // elsewhere, would put sidecars outside the folder the user chose.
        let below_root = |below: &String| {
            (Path::new(below).components()).all(|c| matches!(c, std::path::Component::Normal(_)))
        };
        (self.images.iter())
            .filter(|(_, (_, below, _))| below_root(below))
            .filter_map(|(&id, (root, below, _))| Some((id, roots.get(root)?.join(below))))
            .collect()
    }

    /// The plain collections (not smart ones, sets or publish services), named with the sets
    /// they are in ("Travel - Norway"), and their images at `paths` (from `Catalog::paths`).
    pub fn collections(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(String, Vec<PathBuf>)> {
        let mut images: HashMap<i64, Vec<PathBuf>> = HashMap::new();
        for (coll, image) in &self.collection_images {
            if let Some(p) = paths.get(image) {
                images.entry(*coll).or_default().push(p.clone());
            }
        }
        let mut all: Vec<(String, Vec<PathBuf>)> = (self.collections.iter())
            .filter(|(_, c)| c.2 == "com.adobe.ag.library.collection")
            .map(|(id, _)| {
                let names = names_from_top(*id, |i| {
                    self.collections.get(&i).map(|c| (c.0, c.1.as_str()))
                });
                let name = names.join(" - ").replace('/', "-");
                let name = name.trim_start_matches('.').to_owned();
                (name, images.remove(id).unwrap_or_default())
            })
            .filter(|(name, images)| !name.trim().is_empty() && !images.is_empty())
            .collect();
        all.sort();
        all
    }

    /// The keywords as keyword tree paths ("Places|Norway|Oslo").
    pub fn keyword_paths(&self) -> Vec<String> {
        (self.keywords.keys())
            .filter_map(|&id| self.keyword_path(id))
            .collect()
    }

    fn keyword_path(&self, id: i64) -> Option<String> {
        let names = names_from_top(id, |i| self.keywords.get(&i).map(|k| (k.0, k.1.as_str())));
        (!names.is_empty()).then(|| names.join("|"))
    }

    /// The keywords of each image, as keyword tree paths, at `paths` (from `Catalog::paths`).
    /// One list for each file: the keywords of its virtual copies too, each once.
    pub fn image_keywords(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, Vec<String>)> {
        let mut per_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (image, keyword) in &self.image_keyword_ids {
            let (Some(path), Some(tag)) = (paths.get(image), self.keyword_path(*keyword)) else {
                continue;
            };
            let tags = per_file.entry(path.clone()).or_default();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        let mut all: Vec<(PathBuf, Vec<String>)> = per_file.into_iter().collect();
        all.iter_mut().for_each(|(_, tags)| tags.sort());
        all.sort();
        all
    }

    /// The images with stars, at `paths` (from `Catalog::paths`). One value for each file: a
    /// virtual copy is an image of its own on the same file, and the most stars of them count.
    #[cfg_attr(not(feature = "marks"), allow(dead_code))] // stars are kept only with marks
    pub fn stars(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, u8)> {
        let mut best: HashMap<PathBuf, u8> = HashMap::new();
        for (id, i) in self.images.iter().filter(|(_, i)| i.2 > 0) {
            if let Some(p) = paths.get(id) {
                let n = best.entry(p.clone()).or_default();
                *n = (*n).max(i.2);
            }
        }
        best.into_iter().collect()
    }

    /// What importing the catalog with its root folders `now` would do. It is made once, when
    /// Next is clicked, in a thread of its own (`start_planning`), because it looks at every
    /// image of the catalog on disk. How many stars differ from Glimt's is counted in the window
    /// when it comes (`differing_stars`).
    pub(crate) fn plan(&self, now: &[String]) -> Plan {
        let now: Vec<PathBuf> = now.iter().map(|s| typed_root_folder(s)).collect();
        let mut paths = self.paths(&now);
        // Only the images that are there now get stars, keywords and a place in a collection.
        // A catalog from elsewhere could else have a sidecar written beside any file it names,
        // such as ~/.ssh/id_ed25519. The stars of a photo deleted long ago would also wait on
        // its name for the next photo that takes it.
        let every_type = Types {
            images: true,
            raw: true,
            videos: true,
            hidden: true,
        };
        paths.retain(|_, path| is_listed(path, every_type));
        let named = paths.len();
        paths.retain(|_, path| crate::files::file_io::is_surely_file(path));
        let collections = self.collections(&paths);
        let there: HashSet<String> = collections::saved_collections()
            .into_iter()
            .map(|c| c.0)
            .collect();
        let existing = (collections.iter())
            .filter(|c| there.contains(&c.0))
            .map(|c| c.0.clone())
            .collect();
        #[cfg(feature = "marks")]
        let stars = self.stars(&paths);
        #[cfg(not(feature = "marks"))]
        let stars = Vec::new();
        Plan {
            not_found: named - paths.len(),
            collections,
            existing,
            keywords: self.keyword_paths(),
            image_keywords: self.image_keywords(&paths),
            stars,
            differing_stars: 0,
            takes_lightroom_stars: false,
        }
    }
}

/// A Lightroom catalog read, the folders where its root folders are now, and once Next is
/// clicked what importing it will do.
pub(crate) struct Import {
    catalog: Arc<Catalog>,
    root_folders_now: Vec<String>,
    plan: Option<Plan>,
    /// The plan while its thread makes it, with the folders it is made for.
    planning: Receiving<((Vec<String>, Plan), Working)>,
    /// Whether each folder typed for a root folder is there, as last looked at in the
    /// background, with the texts it was looked at for (`look_at_typed_folders`).
    typed_folders_there: (Vec<String>, Vec<bool>),
    /// The look at the typed folders that runs in the background.
    typed_folders_answer: Receiving<(Vec<String>, Vec<bool>)>,
}

impl Import {
    /// Takes the answer of the look at the typed folders, and starts a new look in the
    /// background when a field changed since the last one. A share that stopped answering then
    /// holds only that thread, not the window. Before, the dialog looked at each folder in the
    /// window's thread in every frame.
    fn look_at_typed_folders(&mut self, ctx: &egui::Context) {
        if let Took::Answer(answer) = self.typed_folders_answer.take() {
            self.typed_folders_there = answer;
        }
        if self.typed_folders_answer.is_some()
            || self.typed_folders_there.0 == self.root_folders_now
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (typed, ctx) = (self.root_folders_now.clone(), ctx.clone());
        std::thread::spawn(move || {
            let there = folders_there(&typed);
            let _ = tx.send((typed, there));
            ctx.request_repaint();
        });
        self.typed_folders_answer = Receiving::of(rx);
    }

    /// Returns whether the folder typed in field `i` was there when it was last looked at. A
    /// field changed since is not, until it has been looked at again.
    fn is_typed_folder_there(&self, i: usize) -> bool {
        let (typed, there) = &self.typed_folders_there;
        typed.get(i) == self.root_folders_now.get(i) && there.get(i).copied().unwrap_or(false)
    }
}

pub(crate) struct Plan {
    /// How many images of the catalog are not there now, so that they are left out. Their
    /// drive may not be mounted.
    not_found: usize,
    collections: Vec<(String, Vec<PathBuf>)>,
    /// The names of those that are there already.
    existing: Vec<String>,
    keywords: Vec<String>,
    stars: Vec<(PathBuf, u8)>,
    /// Each image's keywords, as keyword tree paths.
    image_keywords: Vec<(PathBuf, Vec<String>)>,
    /// How many images have other stars in Glimt, and whether Lightroom's replace them.
    differing_stars: usize,
    takes_lightroom_stars: bool,
}

// ---- Import Lightroom Catalog: the dialog, the plan and the import ----

impl crate::App {
    /// Read a Lightroom catalog in the background; the import dialog opens when it is read.
    pub(crate) fn read_lightroom(&mut self, ctx: &egui::Context, file: PathBuf) {
        let (tx, rx) = answer_channel(&self.busy);
        let ctx = ctx.clone();
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let _ = tx.send(read(&file).map(import_dialog_for));
            drop(pointer);
            ctx.request_repaint();
        });
        self.lightroom_catalog_answer = Receiving::of(rx);
    }

    /// File → Import Lightroom Catalog: where the catalog's root folders are now; then what the
    /// import will do, with the collections that are there already and the stars that differ;
    /// then Import.
    pub(crate) fn lightroom_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut lr) = self.lightroom.take() else {
            return;
        };
        self.take_plan_of(&mut lr);
        lr.look_at_typed_folders(ctx);
        let there: Vec<bool> = (0..lr.root_folders_now.len())
            .map(|i| lr.is_typed_folder_there(i))
            .collect();
        let (mut next, mut back, mut go) = (false, false, false);
        let modal = egui::Modal::new(egui::Id::unique("lightroom")).show(ctx, |ui| {
            ui.strong(tr("Import from Lightroom"));
            match &mut lr.plan {
                None => {
                    ui.label(tr("Where the catalog's folders are now (empty: left out):"));
                    egui::Grid::new("lightroom roots").show(ui, |ui| {
                        let roots = lr.catalog.roots.iter().zip(&mut lr.root_folders_now);
                        for (((_, was), now), is_there) in roots.zip(&there) {
                            ui.label(was);
                            // A size of its own: in the grid the field would be as narrow as
                            // its text.
                            let height = ui.spacing().interact_size.y;
                            let field =
                                ui.add_sized([360.0, height], egui::TextEdit::singleline(now));
                            // The screenshots click the field by the folder it stands for.
                            crate::support::test_hooks::noted(&field, was);
                            ui.label(if *is_there { "✓" } else { "" });
                            ui.end_row();
                        }
                    });
                    ui.weak(tr(
                        "Collections are added to Glimt's, keywords to the keyword tree and \
                         stars to the marks. Smart collections are left out.",
                    ));
                    ui.horizontal(|ui| {
                        // Next is not taken again while the plan is being made.
                        next = crate::ui::menus::button(ui, tr("Next")).clicked()
                            && lr.planning.is_none();
                        if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                            ui.close();
                        }
                    });
                }
                Some(plan) => {
                    ui.label(tf!(
                        "{collections}, {keywords} and the stars of {images}.",
                        collections = count(plan.collections.len(), "collection"),
                        keywords = count(plan.keywords.len(), "keyword"),
                        images = count(plan.stars.len(), "image")
                    ));
                    if plan.not_found > 0 {
                        ui.label(tf!(
                            "Not found and left out: {count}. Import again once their drive is \
                             mounted.",
                            count = count(plan.not_found, "image")
                        ));
                    }
                    if !plan.existing.is_empty() {
                        let mut names = plan.existing[..plan.existing.len().min(5)].join(", ");
                        if plan.existing.len() > 5 {
                            let n = plan.existing.len() - 5;
                            names = tf!("{names} and {n} more", names, n);
                        }
                        ui.label(tf!(
                            "There already, so they get only the images they lack: {names}.",
                            names
                        ));
                    }
                    if !plan.image_keywords.is_empty() {
                        let what = tf!(
                            "The keywords of each image, into its XMP sidecar ({count}); those \
                             it has stay",
                            count = count(plan.image_keywords.len(), "image")
                        );
                        crate::ui::menus::checkbox(ui, &mut self.config.lightroom_keywords, &what);
                    }
                    if plan.differing_stars > 0 {
                        ui.label(tf!(
                            "{n} other stars in Glimt:",
                            n = match plan.differing_stars {
                                1 => tr("1 image has").to_owned(),
                                n => tf!("{count} have", count = count(n, "image")),
                            }
                        ));
                        let take = &mut plan.takes_lightroom_stars;
                        crate::ui::menus::radio_choice(ui, take, false, tr("Keep Glimt's stars"));
                        crate::ui::menus::radio_choice(
                            ui,
                            take,
                            true,
                            tr("Take Lightroom's stars"),
                        );
                    }
                    ui.horizontal(|ui| {
                        go = crate::ui::menus::button(ui, tr("Import")).clicked();
                        back = crate::ui::menus::button(ui, tr("Back")).clicked();
                        if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                            ui.close();
                        }
                    });
                }
            }
        });
        if next {
            self.start_planning(ctx, &mut lr);
        }
        if back {
            // No plan still being made may bring the page back.
            lr.plan = None;
            lr.planning.clear();
        }
        match lr.plan {
            Some(plan) if go => self.import_lightroom(ctx, plan),
            _ if !modal.should_close() => self.lightroom = Some(lr),
            _ => {}
        }
    }

    /// Makes the plan of `lr` in a thread of its own, with the busy pointer, because it looks
    /// at each image of the catalog on disk, which takes long on a share. The dialog takes it
    /// when it comes.
    fn start_planning(&mut self, ctx: &egui::Context, lr: &mut Import) {
        let (tx, rx) = answer_channel(&self.busy);
        let (catalog, now) = (lr.catalog.clone(), lr.root_folders_now.clone());
        let (ctx, pointer) = (ctx.clone(), Working::new(&self.busy));
        std::thread::spawn(move || {
            let plan = catalog.plan(&now);
            let _ = tx.send((now, plan));
            drop(pointer);
            ctx.request_repaint();
        });
        lr.planning = Receiving::of(rx);
    }

    /// Takes the plan of `lr` when its thread has made it, with the stars that differ counted.
    /// A plan made for other folders than the fields hold now is dropped, as the user changed
    /// them while it was made, and Next makes a new one.
    fn take_plan_of(&mut self, lr: &mut Import) {
        match lr.planning.take() {
            Took::Answer(((folders, mut plan), _pointer)) if folders == lr.root_folders_now => {
                plan.differing_stars = self.differing_stars(&plan.stars);
                lr.plan = Some(plan);
            }
            Took::Answer(_) => {}
            Took::Gone => self.tell_background_stopped(tr("Import from Lightroom")),
            Took::Nothing => {}
        }
    }

    /// Returns how many of the images in `stars` have other stars in Glimt than in Lightroom.
    /// An image without stars in Glimt does not count.
    fn differing_stars(&self, stars: &[(PathBuf, u8)]) -> usize {
        #[cfg(feature = "marks")]
        return (stars.iter())
            .filter(|(p, n)| ![0, *n].contains(&self.marks.get(p).1))
            .count();
        #[cfg(not(feature = "marks"))]
        {
            let _ = stars;
            0
        }
    }

    /// The keyword tree and stars here; the collections' files, and the stars' XMP when they
    /// go there, in the background.
    pub(crate) fn import_lightroom(&mut self, ctx: &egui::Context, plan: Plan) {
        let Plan {
            collections: all,
            keywords,
            stars,
            takes_lightroom_stars,
            image_keywords,
            ..
        } = plan;
        // The choice is kept for the next import.
        self.config.save();
        let image_keywords: Vec<(PathBuf, Vec<String>)> = match self.config.lightroom_keywords {
            true => image_keywords,
            false => Vec::new(),
        };
        let mut done = vec![count(keywords.len(), "keyword")];
        self.change_keyword_tree(keywords, None);
        #[cfg(feature = "marks")]
        let stars: Vec<(PathBuf, u8)> = (stars.into_iter())
            .filter(|(p, _)| takes_lightroom_stars || self.marks.get(p).1 == 0)
            .collect();
        #[cfg(feature = "marks")]
        {
            for n in 1..=5 {
                let files: Vec<ListedFile> = (stars.iter().filter(|s| s.1 == n))
                    .map(|s| ListedFile::not_from_the_list(s.0.clone()))
                    .collect();
                if !files.is_empty() {
                    self.marks.rate(&files, n);
                }
            }
            done.push(tf!(
                "the stars of {count}",
                count = count(stars.len(), "image")
            ));
        }
        #[cfg(not(feature = "marks"))]
        let _ = (&stars, takes_lightroom_stars);
        let xmp = cfg!(feature = "marks") && self.config.xmp_stars;
        if !image_keywords.is_empty() {
            done.push(tf!(
                "the keywords of {count}",
                count = count(image_keywords.len(), "image")
            ));
        }
        // The stars (as stars set by hand go there with this setting) and the keywords to the
        // sidecars, through the writer that keeps them in order with what is set meanwhile.
        let mut sidecars: HashMap<PathBuf, (u8, Vec<String>)> = HashMap::new();
        if xmp {
            for (path, n) in &stars {
                sidecars.entry(path.clone()).or_default().0 = *n;
            }
        }
        for (path, tags) in image_keywords {
            sidecars.entry(path).or_default().1 = tags;
        }
        let sidecars_written_later = !sidecars.is_empty();
        // The catalog names its photos by path alone, so there is no identity to check.
        let sidecars = (sidecars.into_iter())
            .map(|(path, data)| (ListedFile::not_from_the_list(path), data))
            .collect();
        self.write_sidecars(
            ctx,
            tr("Importing"),
            sidecars,
            move |path, (n, tags)| {
                // Not for photos gone since. One that could not be looked at is tried and told.
                if crate::files::file_io::is_surely_gone(path) {
                    return true;
                }
                #[cfg(feature = "marks")]
                let stars = *n == 0 || marks::write_xmp(path, *n).is_ok();
                #[cfg(not(feature = "marks"))]
                let stars = *n == 0;
                stars && (tags.is_empty() || xmp::add_tags(path, tags).is_ok())
            },
            |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "Lightroom's stars or keywords not written to the sidecars of {count}",
                    count = count(failed, "image")
                )),
                _ if job.is_stopped() => Some(job.stopped_text()),
                _ => None,
            },
        );
        // The collections: one of the same name gets the images it lacks, nothing is lost.
        // The collections' own files, each edited under its lock.
        self.run_in_background(ctx, Vec::new(), move || {
            let dir = collections::collections_dir();
            if let Some(dir) = &dir {
                let _ = std::fs::create_dir_all(dir); // Not told: told below as not saved
            }
            let failed: Vec<&str> = (all.iter())
                .filter(|(name, paths)| {
                    (dir.as_ref()).is_none_or(|d| {
                        collections::append(&d.join(format!("{name}.gqv")), paths).is_err()
                    })
                })
                .map(|(name, _)| name.as_str())
                .collect();
            done.insert(0, count(all.len() - failed.len(), "collection"));
            let mut msg = tf!("Imported from Lightroom: {done}", done = done.join(", "));
            let any_failed = !failed.is_empty();
            if !failed.is_empty() {
                msg += &tf!("; not saved: {names}", names = failed.join(", "));
            }
            if dir.is_none() && !all.is_empty() {
                msg += tr(" (no data folder: $HOME is not set)");
            }
            // The sidecars may still wait for their writer, which tells a failure on its own.
            // Before, they were told as done here.
            if sidecars_written_later {
                msg += tr("; the sidecars are still being written");
            }
            Some(match any_failed {
                true => crate::support::threads::Told::FailedWrite(msg),
                false => crate::support::threads::Told::Notice(msg),
            })
        });
    }
}

/// Before `QUERY`, with -bail: each table it reads that is not a table (a view, or a virtual
/// table such as zipfile('/any/file'), which -safe does not stop) printed as a 'v' row, and then
/// an error (an integer overflow) that ends the run before anything is read from it.
fn non_table_check() -> String {
    let not_tables = format!(
        "FROM pragma_table_list WHERE schema = 'main' AND lower(name) IN ({TABLES}) \
         AND type <> 'table'"
    );
    format!(
        "SELECT 'v', name, type, '', '' {not_tables};\n\
         SELECT abs(-9223372036854775808) {not_tables} LIMIT 1;\n"
    )
}

/// Read `file`; an error says why in words for the user.
pub fn read(file: &Path) -> Result<Catalog, String> {
    if !crate::files::file_io::is_surely_gone(&file.with_extension("lrcat.lock")) {
        return Err(tr("Lightroom has the catalog open: close Lightroom first").into());
    }
    if !crate::files::external::has_program("sqlite3") {
        return Err(tf!(
            "{needs}, which is not installed",
            needs = tr(NEEDS_SQLITE3)
        ));
    }
    // A catalog may come from anywhere: one with a view (or a schema whose rows say otherwise)
    // in the place of a table it reads would have sqlite3 run what the view says, its writefile
    // and readfile too. -safe refuses those wherever they are. An sqlite3 older than 3.37 has no
    // -safe and is not used: before 3.30 nothing else stops them.
    // As every external program: its output read beside its errors, and ended after the usual
    // limit, so a catalog on a share that stalls does not hold the import for ever.
    let mut child = crate::files::external::stoppable_command("sqlite3")
        .args(ARGS)
        .arg(file)
        .arg(non_table_check() + QUERY)
        .spawn()
        .map_err(|e| format!("{NEEDS_SQLITE3}: {e}"))?;
    let (out, why, ok) = crate::files::external::output_of(
        &mut child,
        256 << 20,
        crate::files::external::PROGRAM_TIME_LIMIT,
    );
    let out = out.ok_or_else(|| tr("The catalog is too large to read").to_owned())?;
    let catalog = parse(&out);
    if let Some((name, kind)) = catalog.not_tables.first() {
        return Err(sqlite_error_text(&format!(
            "{name} is of type {kind}, not a table"
        )));
    }
    if !ok {
        return Err(sqlite_error_text(&why));
    }
    Ok(catalog)
}

/// Tells why sqlite3 could not read a catalog, from what it wrote: one too old for -safe, or
/// the catalog. Only the first line is told, because sqlite3 puts the reason there and then
/// echoes the query with a caret under the error.
fn sqlite_error_text(why: &str) -> String {
    let first = (why.lines().map(str::trim)).find(|l| !l.is_empty());
    match why.contains("unknown option: -safe") {
        true => tr("Reading a Lightroom catalog needs sqlite3 3.37 or newer").into(),
        false => tf!(
            "Not a Lightroom Classic catalog Glimt can read: {why}",
            why = first.unwrap_or("")
        ),
    }
}

fn parse(text: &[u8]) -> Catalog {
    let mut c = Catalog::default();
    for row in text.split(|&b| b == ROW_END) {
        let cols: Vec<String> = (row.split(|&b| b == COLUMN_END))
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let [kind, id, a, b, d] = &cols[..] else {
            continue;
        };
        if kind == "v" {
            c.not_tables.push((id.clone(), a.clone()));
            continue;
        }
        let (Ok(id), n) = (id.parse::<i64>(), a.parse::<i64>()) else {
            continue;
        };
        match (kind.as_str(), n) {
            ("r", _) => c.roots.push((id, a.clone())),
            ("i", Ok(n)) => drop(
                c.images
                    .insert(id, (n, b.clone(), d.parse().unwrap_or(0).min(5))),
            ),
            ("c", Ok(n)) => drop(c.collections.insert(id, (n, b.clone(), d.clone()))),
            ("m", Ok(n)) => c.collection_images.push((id, n)),
            ("k", Ok(n)) => drop(c.keywords.insert(id, (n, b.clone()))),
            ("w", Ok(n)) => c.image_keyword_ids.push((id, n)),
            _ => {}
        }
    }
    c
}

/// The names from the top down to `id`, through `parent_and_name`; the nameless root that
/// Lightroom keeps keywords under is left out. A loop in a damaged catalog ends after 32 steps.
fn names_from_top<'a>(
    mut id: i64,
    parent_and_name: impl Fn(i64) -> Option<(i64, &'a str)>,
) -> Vec<String> {
    let mut names = Vec::new();
    for _ in 0..32 {
        let Some((parent, name)) = parent_and_name(id) else {
            break;
        };
        if !name.is_empty() {
            names.push(name.replace('|', "/"));
        }
        id = parent;
    }
    names.reverse();
    names
}

/// The folder typed for a root folder: `~/` is the home folder, and a relative path or nothing
/// is no folder (empty), as a collection keeps absolute paths only.
pub fn typed_root_folder(typed: &str) -> PathBuf {
    let typed = typed.trim();
    let path = match typed.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(typed),
    };
    if path.is_absolute() {
        path
    } else {
        PathBuf::new()
    }
}

/// Returns the import dialog for `catalog`, with the folders where its photos are now worked
/// out and nothing planned yet. It looks at the disk, so it is called in the thread that reads
/// the catalog, never in the window's.
fn import_dialog_for(catalog: Catalog) -> Import {
    let root_folders_now = roots_now(&catalog);
    let there = folders_there(&root_folders_now);
    Import {
        catalog: Arc::new(catalog),
        typed_folders_there: (root_folders_now.clone(), there),
        root_folders_now,
        plan: None,
        planning: Receiving::none(),
        typed_folders_answer: Receiving::none(),
    }
}

/// Returns whether each folder typed for a root folder is there.
fn folders_there(typed: &[String]) -> Vec<bool> {
    typed
        .iter()
        .map(|t| crate::files::file_io::is_surely_dir(&typed_root_folder(t)))
        .collect()
}

/// Where each root folder is now: the same place if it is there, else empty for the user to
/// fill in.
pub fn roots_now(catalog: &Catalog) -> Vec<String> {
    (catalog.roots.iter())
        .map(|(_, r)| {
            if crate::files::file_io::is_surely_dir(Path::new(r)) {
                r.clone()
            } else {
                String::new()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// A catalog as sqlite3 writes it: a root folder on a Mac, two images, a collection in a
    /// set, a smart collection, and keywords under Lightroom's nameless root.
    fn sample() -> Vec<u8> {
        let rows = [
            "r\x1f1\x1f/Volumes/Photos/\x1f\x1f",
            "i\x1f10\x1f1\x1f2024/Oslo/a.jpg\x1f4",
            "i\x1f11\x1f1\x1f2024/b.CR3\x1f0",
            "c\x1f20\x1f0\x1fTravel\x1fcom.adobe.ag.library.group",
            "c\x1f21\x1f20\x1fNorway\x1fcom.adobe.ag.library.collection",
            "c\x1f22\x1f0\x1fFive stars\x1fcom.adobe.ag.library.smart_collection",
            "m\x1f21\x1f10\x1f\x1f",
            "m\x1f21\x1f11\x1f\x1f",
            "m\x1f22\x1f10\x1f\x1f",
            "k\x1f30\x1f0\x1f\x1f",
            "k\x1f31\x1f30\x1fPlaces\x1f",
            "k\x1f32\x1f31\x1fOslo\x1f",
            "w\x1f10\x1f32\x1f\x1f",
            "w\x1f11\x1f31\x1f\x1f",
            "w\x1f12\x1f32\x1f\x1f",
            "w\x1f99\x1f32\x1f\x1f",
        ];
        rows.join("\x1e").into_bytes()
    }

    #[test]
    fn reads_collections_keywords_and_stars() {
        let c = super::parse(&sample());
        assert_eq!(c.roots, [(1, "/Volumes/Photos/".to_owned())]);
        let now = [PathBuf::from("/media/disk")];
        let a = PathBuf::from("/media/disk/2024/Oslo/a.jpg");
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        let mut colls = c.collections(&c.paths(&now));
        colls[0].1.sort();
        assert_eq!(colls, [("Travel - Norway".to_owned(), vec![a.clone(), b])]);
        let mut keywords = c.keyword_paths();
        keywords.sort();
        assert_eq!(keywords, ["Places", "Places|Oslo"]);
        // Paths that would leave the root folder: left out.
        let mut odd = sample();
        odd.extend(
            b"\x1ei\x1f13\x1f1\x1f../../home/u/x.jpg\x1f0\x1ei\x1f14\x1f1\x1f/etc/y.jpg\x1f0",
        );
        let paths = super::parse(&odd).paths(&now);
        assert!(paths.contains_key(&10) && !paths.contains_key(&13) && !paths.contains_key(&14));
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 4)]);
        // Each image's keywords as tree paths; one of an image not in the catalog left out.
        let tags = |c: &super::Catalog| c.image_keywords(&c.paths(&now));
        let tag = |p: &PathBuf, t: &[&str]| (p.clone(), t.iter().map(|s| s.to_string()).collect());
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        assert_eq!(tags(&c), [tag(&a, &["Places|Oslo"]), tag(&b, &["Places"])]);
        // A virtual copy with more stars: one value for the file, the most.
        let mut copy = sample();
        copy.extend(b"\x1ei\x1f12\x1f1\x1f2024/Oslo/a.jpg\x1f5");
        let c = super::parse(&copy);
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 5)]);
        // Its keyword too, once for the file.
        assert_eq!(tags(&c)[0], tag(&a, &["Places|Oslo"]));
        assert_eq!(
            super::typed_root_folder(" /media/disk "),
            PathBuf::from("/media/disk")
        );
        assert_eq!(super::typed_root_folder("Pictures"), PathBuf::new());
    }

    /// The plan holds only image files that are there. A file that is no image (a key in a
    /// root folder that a catalog from elsewhere names) and a photo deleted since get no stars,
    /// keywords or place in a collection, so no sidecar is written beside them.
    #[test]
    fn plans_only_the_images_that_are_there() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lrplan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jpg"), b"").unwrap();
        std::fs::write(dir.join("id_ed25519"), b"").unwrap();
        let rows = [
            format!("r\x1f1\x1f{}/\x1f\x1f", dir.display()),
            "i\x1f10\x1f1\x1fa.jpg\x1f4".to_owned(),
            "i\x1f11\x1f1\x1fid_ed25519\x1f3".to_owned(),
            "i\x1f12\x1f1\x1fgone.jpg\x1f5".to_owned(),
            "c\x1f20\x1f0\x1fTrip\x1fcom.adobe.ag.library.collection".to_owned(),
            "m\x1f20\x1f10\x1f\x1f".to_owned(),
            "m\x1f20\x1f11\x1f\x1f".to_owned(),
            "m\x1f20\x1f12\x1f\x1f".to_owned(),
            "k\x1f30\x1f0\x1fPlaces\x1f".to_owned(),
            "w\x1f10\x1f30\x1f\x1f".to_owned(),
            "w\x1f11\x1f30\x1f\x1f".to_owned(),
            "w\x1f12\x1f30\x1f\x1f".to_owned(),
        ];
        let catalog = super::parse(rows.join("\x1e").as_bytes());
        let plan = catalog.plan(&[dir.display().to_string()]);
        let a = dir.join("a.jpg");
        assert_eq!(plan.collections, [("Trip".to_owned(), vec![a.clone()])]);
        let with_keywords: Vec<&PathBuf> = plan.image_keywords.iter().map(|k| &k.0).collect();
        assert_eq!(with_keywords, [&a]);
        #[cfg(feature = "marks")]
        assert_eq!(plan.stars, [(a.clone(), 4)]);
        // The photo that is gone is told, so its drive can be mounted and the import done again.
        assert_eq!(plan.not_found, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The plan is made in a thread of its own, which looks at each image on disk, and the
    /// window counts the stars that differ when it comes. Before, Next made it in the window's
    /// thread, which stood still for a catalog of photos on a share.
    #[test]
    fn the_plan_is_made_in_the_background() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lrthread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jpg");
        std::fs::write(&a, b"").unwrap();
        let rows = [
            format!("r\x1f1\x1f{}/\x1f\x1f", dir.display()),
            "i\x1f10\x1f1\x1fa.jpg\x1f4".to_owned(),
        ];
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        #[cfg(feature = "marks")]
        app.marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            2,
        );
        let mut lr = super::import_dialog_for(super::parse(rows.join("\x1e").as_bytes()));
        lr.root_folders_now = vec![dir.display().to_string()];
        app.start_planning(&ctx, &mut lr);
        assert!(lr.plan.is_none() && lr.planning.is_some());
        let took = std::time::Instant::now();
        while lr.plan.is_none() {
            assert!(took.elapsed().as_secs() < 5, "no plan came");
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.take_plan_of(&mut lr);
        }
        let plan = lr.plan.unwrap();
        assert_eq!(plan.differing_stars, usize::from(cfg!(feature = "marks")));
        // A folder changed while the plan is made: the plan for the old one is not shown, and
        // Import cannot write onto the files of the folder given first.
        lr.plan = None;
        app.start_planning(&ctx, &mut lr);
        lr.root_folders_now = vec![dir.join("other").display().to_string()];
        let took = std::time::Instant::now();
        while lr.planning.is_some() {
            assert!(took.elapsed().as_secs() < 5, "no plan came");
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.take_plan_of(&mut lr);
        }
        assert!(lr.plan.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Whether each typed folder is there is looked at in the background, and the ✓ comes from
    /// that answer. A field changed since shows no ✓ until it has been looked at again.
    #[test]
    fn the_typed_folders_are_looked_at_in_the_background() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lrtyped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = format!("r\x1f1\x1f{}/\x1f\x1f", dir.join("gone").display());
        let mut lr = super::import_dialog_for(super::parse(rows.as_bytes()));
        assert!(!lr.is_typed_folder_there(0));
        lr.root_folders_now = vec![dir.display().to_string()];
        let ctx = eframe::egui::Context::default();
        let took = std::time::Instant::now();
        while !lr.is_typed_folder_there(0) {
            assert!(took.elapsed().as_secs() < 5, "no answer came");
            lr.look_at_typed_folders(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        lr.root_folders_now = vec![dir.join("gone").display().to_string()];
        assert!(!lr.is_typed_folder_there(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A catalog of tables is read with sqlite3's -safe; one with a view in the place of a table,
    /// in any case, has no file written by its writefile, and one with a virtual table is refused
    /// before it is read.
    #[test]
    fn refuses_a_catalog_with_views() {
        if !crate::files::external::has_program("sqlite3") {
            return eprintln!("no sqlite3: not tested");
        }
        let dir = std::env::temp_dir().join(format!("glimt-test-lrcat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, root: &str| {
            let file = dir.join(name);
            let schema = format!(
                "{root}; CREATE TABLE Adobe_images(id_local, rootFile, rating);
                CREATE TABLE AgLibraryFile(id_local, folder, baseName, extension);
                CREATE TABLE AgLibraryFolder(id_local, rootFolder, pathFromRoot);
                CREATE TABLE AgLibraryCollection(id_local, parent, name, creationId);
                CREATE TABLE AgLibraryCollectionImage(collection, image);
                CREATE TABLE AgLibraryKeyword(id_local, parent, name);
                CREATE TABLE AgLibraryKeywordImage(image, tag);"
            );
            let mut sqlite = crate::files::external::stoppable_command("sqlite3");
            sqlite.arg(&file).arg(schema);
            crate::files::external::run_limited(
                sqlite,
                1 << 16,
                crate::files::external::PROGRAM_TIME_LIMIT,
            )
            .unwrap();
            file
        };
        let tables = make(
            "tables.lrcat",
            "CREATE TABLE AgLibraryRootFolder(id_local, absolutePath);
            INSERT INTO AgLibraryRootFolder VALUES (1, '/Volumes/Photos/')",
        );
        let catalog = super::read(&tables).unwrap();
        assert_eq!(catalog.roots, [(1, "/Volumes/Photos/".to_owned())]);
        let pwned = dir.join("pwned");
        // Named in another case too, which SQLite finds the table by all the same.
        for name in ["AgLibraryRootFolder", "aglibraryrootfolder"] {
            let view = format!(
                "CREATE VIEW {name} AS SELECT 1 AS id_local, \
                 writefile('{}', 'x') AS absolutePath",
                pwned.display()
            );
            assert!(super::read(&make(&format!("{name}.lrcat"), &view)).is_err());
            assert!(!pwned.exists());
        }
        // A virtual table, which -safe lets through: zipfile reads any file it is given.
        let secret = dir.join("secret");
        std::fs::write(&secret, "private").unwrap();
        let zip = format!(
            "CREATE VIRTUAL TABLE AgLibraryRootFolder USING zipfile('{}')",
            secret.display()
        );
        let why = super::read(&make("virtual.lrcat", &zip)).err().unwrap();
        assert!(
            why.contains("AgLibraryRootFolder is of type virtual, not a table"),
            "{why}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A user's sqliterc that changes the output (as JSON) changes nothing read. sqlite3 finds
    /// it in $XDG_CONFIG_HOME/sqlite3 (3.44 and newer), or in the home folder from the user
    /// database, which a test cannot point elsewhere.
    #[test]
    fn ignores_the_users_sqliterc() {
        if !crate::files::external::has_program("sqlite3") {
            return eprintln!("no sqlite3: not tested");
        }
        let config = std::env::temp_dir().join(format!("glimt-test-rc-{}", std::process::id()));
        std::fs::create_dir_all(config.join("sqlite3")).unwrap();
        std::fs::write(config.join("sqlite3/sqliterc"), ".mode json\n").unwrap();
        let mut sqlite = crate::files::external::stoppable_command("sqlite3");
        sqlite
            .env("XDG_CONFIG_HOME", &config)
            .args(super::ARGS)
            .arg(":memory:");
        sqlite.arg("SELECT 'r', 1, '/Photos/', '', '';");
        let out = crate::files::external::run_limited(
            sqlite,
            1 << 16,
            crate::files::external::PROGRAM_TIME_LIMIT,
        )
        .unwrap();
        assert_eq!(super::parse(&out).roots, [(1, "/Photos/".to_owned())]);
        std::fs::remove_dir_all(config).unwrap();
    }

    /// An sqlite3 too old for -safe (before 3.37) is told as such, not as a bad catalog.
    #[test]
    fn tells_an_sqlite3_without_safe_mode() {
        let old = "sqlite3: Error: unknown option: -safe\nUse -help for a list of options.\n";
        assert_eq!(
            super::sqlite_error_text(old),
            "Reading a Lightroom catalog needs sqlite3 3.37 or newer"
        );
        assert!(
            super::sqlite_error_text("Error: file is not a database\n").ends_with("not a database")
        );
        let parse = "Parse error: no such table: Adobe_images\n  SELECT id FROM Adobe_images\n";
        assert!(super::sqlite_error_text(parse).ends_with("no such table: Adobe_images"));
    }

    /// sqlite3 not on the host of a Flatpak, or not in PATH, is told from a catalog it cannot read.
    /// sqlite3 output broken in thousands of ways: read without a panic or an endless loop.
    #[test]
    fn survives_broken_output() {
        let now = [PathBuf::from("/x")];
        for text in crate::support::test_support::broken(&sample()) {
            let c = super::parse(&text);
            let paths = c.paths(&now);
            c.collections(&paths);
            c.keyword_paths();
            c.stars(&paths);
            c.image_keywords(&paths);
        }
    }

    /// The import's notice says that the sidecars are still being written, as their writer
    /// tells only a failure, later. Before, it named their keywords and stars as imported while
    /// they waited for the writer, which might yet fail.
    #[test]
    fn the_import_does_not_tell_sidecars_as_done() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-lightroom-told-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&png).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        app.config.lightroom_keywords = true;
        let plan = super::Plan {
            not_found: 0,
            collections: Vec::new(),
            existing: Vec::new(),
            keywords: Vec::new(),
            stars: Vec::new(),
            image_keywords: vec![(png, vec!["Tag".into()])],
            differing_stars: 0,
            takes_lightroom_stars: false,
        };
        app.import_lightroom(&ctx, plan);
        let imported = std::iter::from_fn(|| {
            app.notices
                .recv_timeout(std::time::Duration::from_secs(5))
                .ok()
        })
        .find_map(|told| match told {
            crate::support::threads::Told::Notice(text) if text.contains("Lightroom") => Some(text),
            _ => None,
        })
        .unwrap();
        assert!(imported.contains("still being written"), "{imported}");
        crate::files::file_io::flush();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
