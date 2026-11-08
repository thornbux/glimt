# AGENTS.md

Guidelines for coding agents (and people) adding code to Glimt, a fast, lightweight image viewer
for Linux in Rust/egui, inspired by Geeqie. Read this before changing anything.

## Goals that decide every change

- **Fast and snappy, always.** Nothing heavy on the UI thread: decoding, folder listing, file
  operations, mounting and external programs run in background threads and report back over
  `mpsc` channels, with a repaint request. Show the busy pointer (`busy` counter) while they run.
- **As fast with 500,000 files in a folder as with 10.** No O(n) work per frame, no system call
  per file in the list (use `DirEntry::file_type`), draw lists with `show_rows`, compare file
  names as bytes. Test large folders with hard links to one image, for example
  `mkdir big && for i in $(seq 500000); do ln a.jpg big/$i.jpg; done` on tmpfs.
- **Little memory.** Decode to screen size; full resolution only when zoomed. Preloading has a
  fixed budget (N images on each side). Measure RssAnon before and after changes that touch
  decoding or caches.
- **Simplest thing that works.** No dependency, abstraction, config option or file before it is
  needed. Prefer the standard library, then crates already in `Cargo.toml`, then a few lines of
  code. Heavy formats go through external programs (ffmpeg) rather than linking codecs.

## Behaviour

- Shortcuts and behaviour follow **Geeqie's defaults** (see its keyboard reference). Every
  deliberate deviation goes in the `DIFFERENCES` table in `src/ui/help.rs`, which the Help Manual
  (F1) shows. Remove a row when Glimt starts behaving like Geeqie.
- New user-visible features are described in `src/ui/help.rs` (manual, shortcuts, keyboard map)
  and, where relevant, on the website (the `glimt-web` repository, glimt.pario.no). `README.md`
  keeps only what the website does not have: building from source, the optional features'
  table, the runtime dependencies and restoring a backup.

## Frozen in 1.x

These stay as they are until a 2.0: the command-line options,
the `--remote` commands with their answers and exit codes, the settings file's keys and values
(a list of choices grows at its end only, `config::NAMED`), the files Glimt owns, and the
desktop file formats of tools and added formats. Add; never take away or change a meaning. A
setting that goes out of use keeps being read and ignored.

`src/frozen-1.0.txt` pins, from the code's own tables: the settings' keys; the values of each
`config::NAMED` list in order; the values of `sort` (`SORTS`), `layout` and `layout_turns`
(`LAYOUTS`, also `--layout`'s), `rendering_intent` (`INTENTS`), `input_profile`'s built-in
words (`INPUT_PROFILES`), `print_paper` (`PAPERS`), `print_orientation` (`ORIENTATIONS`),
`theme` (the built-in themes) and `language` (`LANGUAGES` and `lang::ENGLISH`); the window
state's keys (`WindowState`, `LAST_DIR`); the `--remote` commands and the words of their
choices (`on|off`, from `COMMANDS`); `--remote`'s exit codes (`NOT_SENT`, `NOT_TAKEN`); and
the command-line options of `START_OPTIONS` with `--remote` and `--remote-all`. `config::tests::frozen_names_are_kept`
fails when one is taken out or renamed, when a `NAMED` list gains a value anywhere but at its
end, and until a new one is added at the end of that file.
`the_parser_takes_every_frozen_option` fails when an option that the help lists is neither
taken by `Start::parse` nor answered by `cli::answer_without_window` (through `cli::Command::named`) or by
`--remote`. Not pinned, and so kept by hand: the window's own reading of each `--remote`
command (`remote_command` matches the words again), what the commands print, the `%field%`
names of the full screen info, the print caption and the window title (`name`, `path`,
`position`, `dimensions`, `marks`, `keywords`, `comment`, `File size`, `folder`, `app`, and
the EXIF labels that the first two fall through to), the files Glimt owns, and the desktop
file formats of tools and added formats. Never rename an `Action` variant or rely on a changed key
name outside the table below: they are the `keys=` format, pinned by `src/keys-1.0.txt` and
its test, every action (also one without a default key) and every default key; new ones go at
the end of that file, and the test fails until a new action is there.

A frozen name that fails a reader (0.74.0 renamed a few) is renamed only through a table of
old names, one for each kind, beside the code that reads it: `config::RENAMED_SETTINGS`,
`config::RENAMED_STATE`, `actions::RENAMED_ACTIONS` (for `keys=` and `--remote run`),
`remote::RENAMED_COMMANDS` and `tools::RENAMED_FOLDERS` (0.75.0's folders of tools and added
formats, which Glimt opens and creates only under the new name). Glimt reads the old name too, forever, and writes only the new
one; when a settings or state file holds both, the new one counts. The old line stays in the
frozen file and the new name is added at its end, and the tests take an old name only when its
table maps it to a name that is read now.

## Language

English for everything in the repository: UI text, identifiers, comments, commit messages and
documentation.

The interface ships in fifteen languages: English and the translations in `po/*.po` (Czech,
Danish, German, Spanish, Finnish, French, Icelandic, Italian, Dutch, Norwegian bokmål and
nynorsk, Polish, Brazilian Portuguese and Swedish), listed in `lang::LANGUAGES`. **Every new UI
text, `tr("…")` or `tf!("…")`, is translated into all of them in the same commit**:
`po/no_nb.po` first, then the others.
`lang::tests::every_language_has_every_text` fails while one lacks a text of `no_nb.po`, and
`translations_keep_placeholders` while a translation drops a `{name}`. Keep the key names as the
menus show them (Ctrl, Shift, Enter, Esc, Delete), `%field%` template names unchanged, and only
quotation marks Inter has (`«»`, `“”`, `„`, `”`). A noun of `count` has the forms its language's
`plural_form` picks from: two in most, three in Polish and Czech (one, few, many).

`po/glimt.pot` is the template for a new language: no_nb.po with every translation empty,
written by `python3 tools/pot.py` (see *Before every commit*). `lang::tests::
the_template_has_every_text` fails while it lacks a text of no_nb.po. A new language is
added with its `.po` file, a line in `lang::LANGUAGES` and, if it is not one and many, its
rule in `plural_form`.

## Code layout

`src/` holds main.rs (the app and its frame), cli.rs (the command line) and eight folders, in the
order a reader meets them: `model/` (what is open), `viewer/` (the image shown), `ui/` (the window
around it), `features/` (the features with a window of their own), `images/` (from a file to pixels
and back), `metadata/` (what is said about an image), `files/` (how files change on disk) and
`support/` (what the rest is built on). Each folder has a file of the same name beside it that
holds only its `mod` lines, and a new module goes into the folder of what it does
(`source_rules::modules_live_in_their_folder`).

| File | Contents |
|---|---|
| `src/main.rs` | The app: `App` and its parts (each a struct in the module whose code uses it, such as `slideshow::Slideshow` or `file_ops::FileOperations`), starting up, the frame (`eframe::App`) and `main()`. The window's parts are each in a module of their own below, as `impl crate::App` blocks |
| `src/cli.rs` | The command line: the start options (`Start`, `START_OPTIONS`) and the commands that answer without a window (`answer_without_window`: --help, --version, --check-config, --back-up) |
| `src/model.rs` | **The model/ folder:** what is open: the file list, listing and opening folders and collections, the search and the selection |
| `src/model/list.rs` | The model of what is open: `List`, the file list with everything that holds a place in it, and `LoadedImage`, an image decoded for the view |
| `src/model/folder.rs` | Opening a folder, a collection or several folders, listing them, following changes, the sizes summed for the status bar (`send_sizes`, `receive_sizes`) |
| `src/model/selection.rs` | Moving through the list (next, previous, a page, random) and the selection |
| `src/model/listing.rs` | Image extensions, reading a folder or tree, sorting (`Sort`, `natural_cmp`), the Types menu, `pattern_name` for Rename Many |
| `src/model/collections.rs` | Collections: Geeqie `.gqv` files of images from any folders, and putting them in order (`moved`, `dropped`) |
| `src/model/search.rs` | The search: fuzzy names and `exif:`, `re:`, `mark:` and `stars:` terms with `|`, `-` and `( )`, in parallel; the EXIF index (`exif_index`) |
| `src/viewer.rs` | **The viewer/ folder:** the image area and what is done to the image shown: the view, the slideshow, pan view, crop and Save As |
| `src/viewer/slideshow.rs` | The slideshow: its timer, order, fade and the next slide's rule (`next_slide`) |
| `src/viewer/view.rs` | The image area: decoding the shown image and its neighbours, what belongs to the image shown (`Viewing`), zoom and fit, panes (`LAYOUTS`) and Compare, painting, the wheel |
| `src/viewer/crop.rs` | Edit → Crop: the part dragged over the shown image, held by its path and the copy as shown |
| `src/viewer/save_as.rs` | File → Save As: its dialog, and `App::shown_copy`, the copy of the image as shown that Crop and Save As start from |
| `src/viewer/pan.rs` | Pan view: the list by day, its rows, the timeline and the calendar |
| `src/ui.rs` | **The ui/ folder:** the window around the image: actions and keys, menus, dialogs, the side panel, the info panel, Preferences, help, the window, the look, the screen reader and the background work each frame collects |
| `src/ui/actions.rs` | The `Action` enum with its menu labels, the `SHORTCUTS` table, the user's own keys (variants are stored by name, renamed only through `RENAMED_ACTIONS`); `run`, which does each action, and reading the keys |
| `src/ui/dialogs.rs` | The file chooser (zenity or kdialog, `Pick`), `dialog_open`, and the small dialogs: a name asked for (`Prompt`), a delete confirmed, a share's login, a job cut short |
| `src/ui/file_list.rs` | The side panel: the folder list, the file list or icon grid with its thumbnails (their sizes, `THUMB_SIZES`, `MAX_THUMBS`), dragging, the rubber band, and the status bar |
| `src/ui/info_panel.rs` | The info panel (Ctrl+K) and what it edits: EXIF, keywords and the keyword tree, comment, alt text, marks; the EXIF reader thread |
| `src/ui/menus.rs` | The menu bar and the toolbar, the menus that change settings, Tools and Open With, and the helpers every menu is drawn with (`menu_item`, `scroll_menu`, `with_icon`) |
| `src/ui/window.rs` | The window's size, place and title, the pointer hidden in full screen, and the screen saver kept off |
| `src/ui/workers.rs` | The thumbnail workers, what the window collects from every background job each frame (`poll_background`), and whether work is running (`working`) |
| `src/ui/accessibility.rs` | Optional screen reader support: the shown image as text for Orca (AccessKit) |
| `src/ui/help.rs` | Help Manual, Keyboard Shortcuts, Keyboard Map, About, `DIFFERENCES` |
| `src/ui/preferences.rs` | Edit → Preferences: the window and its sections |
| `src/ui/theme.rs` | The look: Inter and Phosphor fonts, themes (`themes/*.theme` built in, own ones in `~/.config/glimt/themes`), the desktop's dark style and accent |
| `src/features.rs` | **The features/ folder:** features with a window or a job of their own: Copy for Sharing, Find Duplicates, Check Images, printing, tools and remote control |
| `src/features/check.rs` | File → Check Images: every image decoded in the background, the broken ones listed |
| `src/features/copy_for_sharing.rs` | File → Copy for Sharing: its dialog (the tags in groups, Remove All, the last choice kept in `$XDG_STATE_HOME/glimt/sharing`) and the job that writes the copies |
| `src/features/duplicates.rs` | Find Duplicates: groups by checksum, name, size, dimensions or likeness, and its window |
| `src/features/tools.rs` | Tools (Geeqie's plugins): desktop files whose command runs on the selected files; `desktop_files` and `desktop_entry_values`, which added_formats.rs reads with too, and `RENAMED_FOLDERS`, the folders' old names (`plugins`, `decoders`), still read |
| `src/features/print.rs` | Printing: a PDF of the images, sent to `lp` or saved, and the print dialog |
| `src/features/remote.rs` | `--remote`, `--remote-all` and `--name`: a socket for each Glimt in `$XDG_RUNTIME_DIR/glimt`, `COMMANDS`, the sender (`client`) and the listener |
| `src/images.rs` | **The images/ folder:** from a file to pixels and back: decoding, RAW, colour, video, added formats, thumbnails and saving |
| `src/images/decode.rs` | An image file to pixels at the size shown: scaled JPEG, animations, the histogram; `read_limited`, `read_text` and `read_head`, the one way a file is read whole |
| `src/images/svg.rs` | SVG and SVGZ through resvg (feature `svg`): `load`, with limits on the text unpacked, the pictures linked and entities that grow |
| `src/images/added_formats.rs` | Added formats: programs in `glimt/formats` that turn other formats into images |
| `src/images/colour.rs` | Colour management: ICC profiles, the screen profile (also from colord), rendering intent |
| `src/images/save.rs` | Saving a turned image with its EXIF and XMP, also lossy WebP; copies of the shown image (`CopyAs`, `copy_bytes`, `shown_image`), `save_copy_with_sidecar`, the one way a copy and its sidecar are saved, and `Derived` with `derived_xmp`, what a turned image, a copy and their sidecars keep of the metadata, also a copy for sharing (`shared_metadata`) |
| `src/images/shared_copy.rs` | Copy for Sharing's copies: JPEG segments, PNG and WebP chunks walked, the parts kept carried byte for byte with a new EXIF and XMP (`copy`, `tags_of`); the tags' groups (`Group`) and the choice (`LeftOut`) |
| `src/images/raw.rs` | Camera RAW: the largest embedded JPEG preview |
| `src/images/thumbs.rs` | Freedesktop thumbnail cache (`~/.cache/thumbnails`) |
| `src/images/video.rs` | Optional video feature |
| `src/metadata.rs` | **The metadata/ folder:** what is said about an image: EXIF, XMP sidecars, marks and stars, and the Lightroom import |
| `src/metadata/exif.rs` | Minimal EXIF (TIFF) parser for the panel, the EXIF window's every field, the overlay and date sorting; writes a JPEG's orientation |
| `src/metadata/lightroom.rs` | Import Lightroom Catalog: a Lightroom Classic `.lrcat` read through `sqlite3`, `Catalog::plan`, the dialog and the import |
| `src/metadata/marks.rs` | Optional marks 1–6 and stars, kept in `$XDG_STATE_HOME/glimt/marks`, and stars in XMP |
| `src/metadata/xmp.rs` | Keywords, comment, IPTC alt text and extended description in XMP sidecars, keyword tree paths; `update_sidecar` (also for stars); sidecars that follow their files |
| `src/files.rs` | **The files/ folder:** how files change on disk: file operations, the trash, the writers, Glimt's own files, and the external programs and portals |
| `src/files/config.rs` | `Config` (the `config!` macro) with the settings' defaults and ranges, the window state, XDG paths, and `--lock-settings` |
| `src/files/backup.rs` | The backup (File → Back Up, `--back-up`): Glimt's own folders (`own_folders`) copied into a new folder (`copy_tree`, `back_up_to`) |
| `src/files/file_ops.rs` | `Job` (a file operation in the background, with its progress and Stop) and `FileOperations`; delete, copy and move, `OperationReport` (what a file operation did, for `receive_operation_report`) and `Undo`, Rename and Rename Many, saving the orientation, as jobs with progress and Stop (`start_job`, `run_in_background`, `write_sidecars`); `rename_files`; Undo |
| `src/files/file_io.rs` | The one way each change on disk is made: `replace_file`/`create_file`/`write_atomic` (whole or not at all), `replace_name` (a link at the name replaced, not followed), `edit_shared` for the files the windows share, `rename_never_over` (the one way to rename) and `put_file` (a move or copy, never over a file); the writer thread (`write_later`, `flush`) |
| `src/files/trash.rs` | The trash: `trash` (gio, or by hand as the trash spec says when the Flatpak's portal refuses a file), `TrashMove` (what a move to the trash did) and `restore`, which puts it back for Undo by reading the trash folders |
| `src/files/external.rs` | External programs: `command()`, `stoppable_command`, `run_limited`/`short_output_of`, `output_of`, `Watchdog`, `in_flatpak`; ffmpeg (HEIC, AVIF, video), gio mounting, open with |
| `src/files/portal.rs` | The desktop portals that need more than gdbus, through zbus (feature `portals`): printing, Open With and keeping the screen on in the Flatpak (`print`, `open_with`, `inhibit_idle`) |
| `src/files/journal.rs` | Rename Many's steps on disk while they run, put right at the next start after a crash |
| `src/support.rs` | **The support/ folder:** what the rest is built on: threads, text for the user, the languages, and what the tests use |
| `src/support/test_hooks.rs` | The release tests' hook (feature `test-hooks`): where a widget with a text was drawn, for `glimt --remote where LABEL`, and `noted`, which every widget the tests click goes through |
| `src/support/test_support.rs` | What tests start from, built only for tests: `app_on` (a `crate::App` on a folder without a window), `run_background_until`, `contract_folder`, and `broken` for the parsers' `survives_broken_*` tests; each module's tests use them, so a test sits beside the code it tests |
| `src/support/threads.rs` | How many threads for each kind of work (the CPU use level, `background_threads`, `thumb_threads`, `small_alloc_threads`), `in_parts`, `Receiving`/`Took` for a background thread's answer, `Working` for work counted while it runs |
| `src/support/format.rs` | Text for the user: `human_size`, `size_text`, `thousands`, `count`, `middle_cut`, `local_time` (the trash's date form too), `fill_template` |
| `src/support/lang.rs` | The interface's languages: the `.po` files, `tr`/`tf!`, each language's plural rule |
| `src/support/source_rules.rs` | Tests of the source itself: the layout below, kept by a test rather than by memory; `sources()`, every source file and translation |
| `po/` | The translations, one gettext `.po` file per language, built into the program (`lang::LANGUAGES`), and `glimt.pot`, the template for a new one |
| `themes/` | The built-in themes |
| `assets/` | Icons, the desktop and AppStream files, the fonts and their licenses |
| `docs/` | The man page `glimt.1`, `screenshots/` for README and the website, and `architecture/`: diagrams of the code's layout, a frame and its threads, a folder to pixels, a change on disk and the checks (update them when what they name changes) |
| `examples/formats/` | Added formats: desktop files (`<format>.desktop`, `<format>-imagemagick.desktop`) and their scripts (`glimt-<format>`) |
| `examples/tools/` | Tools (`<name>.desktop` running `glimt-<name>`: describe a photo with a local model, read the alt text aloud), and `test-model.py`, a stand-in model |
| `flatpak/` | The Flatpak manifest, and `cargo-sources.json`: the crates it builds from without the network |
| `tools/check.sh` | Runs the checks before a commit in Docker, with mold and sccache, a few at a time |
| `tools/sbom.py` | Writes `sbom.cdx.json` and `THIRD-PARTY-LICENSES.txt` |
| `tools/flatpak-sources.py` | Writes `flatpak/cargo-sources.json` from `Cargo.lock` |
| `tools/pot.py` | Writes `po/glimt.pot`, the template for a new language, from `po/no_nb.po` |
| `tools/flathub-manifest.py` | Writes the Flathub repository's manifest and `cargo-sources.json` for a release tag |
| `tools/release-tests/` | `run.sh` builds this and the last release in Docker; `tests.sh` checks them under Xvfb; `samples.txt` lists real files of each format, downloaded once |
| `tools/screenshots/` | `run.sh` makes `docs/screenshots/` of generated pictures with `shots.sh`, using `tests.sh`'s setup (`SHOTS=1`) |

### Within a file

Every `.rs` file has its items in this order, so a reader finds things where they expect them:

1. the `//!` doc, then `use`, `mod` and `macro_rules!`;
2. `const` and `static`;
3. types (`struct`, `enum`, `type`, `trait`), each followed by its own `impl`s;
4. one `impl crate::App` block (with `// ---- Part ----` headings inside when it is long), and
   trait impls for `App`;
5. free functions;
6. `#[cfg(test)] mod tests`, last.

`source_rules::items_are_in_order` fails on an item out of place, and on a second
`impl crate::App` block.

A name is imported from the module that defines it (`use crate::files::config::Config`), never through
main.rs's own imports (`crate::Config`); main.rs names its modules as `crate::ui::actions::…` too
(`source_rules::imports_name_where_defined`).

Comments say why, or what a reader cannot see from the code. They are written for someone new
to the code, in plain English prose: whole sentences with a subject and a verb, with the
articles kept, and one thought to a sentence. Older comments use a clipped style, where a
semicolon or colon joins a fragment (`…; None for no search.`) and punctuation stands in for
"because", "when" and "so". New and changed comments do not. The fourteen files with the most
were rewritten in 0.54.0. The clipped style is left mostly in `exif.rs`, `file_io.rs`,
`collections.rs`, `actions.rs`, `marks.rs`, `slideshow.rs`, `window.rs`, `preferences.rs`,
`lightroom.rs`, `colour.rs`, `duplicates.rs`, `pan.rs`, `trash.rs` and `listing.rs`, and in
smaller amounts in most other files. An older comment is rewritten in the new style when the
code it describes changes, not on its own.

For example, not this:

```rust
/// Files whose keywords or comment changed since they were indexed: read again, alone,
/// at the next "exif:" search.
```

but this:

```rust
/// The files whose keywords or comment changed after they were indexed. The next "exif:"
/// search reads only these files again, not the whole folder.
```

The doc comment of a function starts with what the function does. When its name does not make
the result plain, a sentence starting with "Returns" says what it gives back, and when it gives
back `None`. Comments fit 100 columns (`source_rules::comments_fit_the_width`).

Names are written for the same reader: someone new to the code should know what a variable,
function, type or field is or does from its name alone, without reading its body.

- Use whole, everyday words (`selected_paths`, not `sel` or `sp`). Short names are fine for a
  loop counter or a closure's argument that lives a few lines.
- Do not use a private image or a pet word for what the code does. `land` for "save a copy and its
  sidecar" or `hold` for "hold the files back while other work changes them" must be looked up;
  `save_copy_with_sidecar` and `hold_back_while_writing` need not.
- Name a function for what it does or gives back (`read_sidecar`, `selected_paths`), a `bool`
  as a question that its value answers (`is_hidden`, `has_sidecar`), and a type for what it
  holds. A type that every file operation returns is not named after one of them (`Deleted`).
- One thing has one name throughout: the same word in the type, its fields, its functions and
  the comments.

Older names that broke this were renamed in 0.74.0's naming review, and
`source_rules::retired_names_stay_gone` keeps them out of the code: its list gives each old name
with the one that took its place. `Action` variants and the names in `src/frozen-1.0.txt` keep
their names, or change only through a table of old names, as *Frozen in 1.x* says.

A function is at most 120 lines outside the tests (`source_rules::functions_stay_short`): a
longer one is split into functions named for what each part does, beside it or in the module
of that part. The exceptions are the tables of one arm per action (`Action::english`,
`Action::icon`, `App::run`), whose arms stay a few lines: a longer arm is a method.

## How to add things

- **An action:** add a variant to `Action`, its name at the end of `src/keys-1.0.txt`, its
  menu label in `Action::label`, a key in
  `SHORTCUTS` if Geeqie has one (put Shift/Ctrl+Shift combinations before the plain key: egui
  ignores an extra Shift when matching), handle it in `App::run`, and add it to a menu with
  `menu_item`/`menu_toggle`. Name what it acts on in `Action::target`: the selection (`App::targets`), the
  shown image, or no file. A menu about the selection, such as the file list's right-click menu
  (`file_menu`), offers only actions on the selection. `targets()` returns `None` when the search
  hides every selected file, and the action then stops: the shown image must not stand in for
  the files the user picked. A feature or dialog that selects the files it acts on (Find
  Duplicates, Check Images, Rename Many) does so with `select_for_action`, which refuses while
  the search hides any of them.
- **A setting:** one line `name: Type = default` in `config!`; values are read with `FromStr`
  and written with `Display`. Add a checkbox in Preferences, and clamp ranges in
  `Config::parse`. Settings stay in `Config` even when their feature is not built in, so saving
  never drops them. A setting with a setter (`set_…`, or `toggle_setting` for a View toggle) is
  written only by that one setter, also from Preferences, so it does what the menu does besides
  (`source_rules::settings_change_through_their_setters`).
- **A format:** built in when the `image` crate reads it with no new dependency (its feature in
  `Cargo.toml`, the extension in `listing::EXTS`, a test in `decode.rs`). Otherwise an added
  format in `examples/formats` (`src/images/added_formats.rs`), with a check in `tests.sh`, not code or a
  dependency in Glimt.
- **An optional feature** (chosen when building, not a tool): a Cargo feature in `[features]`
  (also add it to `full`), a module or extension list behind `#[cfg(feature = "...")]`, and its
  settings shown in Preferences only when built in. Never break the default build. "Tool"
  means only the desktop files in Edit → Tools, and "added format" only those in
  `glimt/formats`. Before 0.75.0 they were plugins and decoder plugins, and their old folders
  are still read (`tools::RENAMED_FOLDERS`).
- **A file operation** (one that starts a job or background work) joins
  the contract in file_ops.rs's tests: an entry in `contract_ops()` with what it does to the
  contract's folder and what must hold after (the file, its sidecar, its stars, held back while
  other work changes its files), and its function with that entry's name in `IN_CONTRACT`.
  `every_file_operation_is_in_the_contract` fails until it is there or in `OUTSIDE` with the
  reason it is not, and also when a name in `IN_CONTRACT` has no entry in `contract_ops()`.
- **State for a feature:** a field in the struct of the part it belongs to (`self.slideshow`,
  `self.folder`, `self.file_operations`, …), or a struct of its own in the feature's module held by one
  field in `App`; never loose fields in `App` (`source_rules::app_stays_small`). The fields of
  the parts that `source_rules::parts_keep_their_fields` lists are private: other modules call
  their methods, named for what they do (`self.slideshow.toggle_pause()`,
  `self.panel.is_hidden()`), never write their fields. Every part of `App`, `self.folder` and
  `self.list` among them, is in that list, and a new part joins it.
- **Background work:** spawn a thread, send the result over a channel stored in `App` as a
  `Receiving<T>` (never a bare `Option<Receiver>`), and take it in `update` with `take`, `next`,
  `latest` or `wait`, which tell once when the thread is gone. egui only takes clipboard and
  other output during a frame, so hand results to the UI thread instead of calling the context
  from a worker.
- **A new way to show the list** (as the grid in the image area, G) gets what every view of
  it needs, which 0.65.0's grid missed through five audit rounds:
  - its form from `file_list`'s parameters, never from the settings in the functions that
    draw it, as two views draw at once (`the_list_is_drawn_in_the_form_its_caller_asks`);
  - its keys read only while no dialog is open and no text field has the focus (`typing` in
    `read_keys`), and before the shortcut that would take them;
  - a screen reader's text: each row or cell through `speak_item`, and the shown image read
    out with `speak_image` when the view takes the image area's place
    (`every_clickable_widget_drawn_by_hand_speaks`);
  - a menu item ticked when choosing it turns the view off, and not otherwise;
  - the notices drawn over it, as over the image;
  - drag and drop, the rubber band and the scroll to the shown image (`Panel::start_frame`)
    handled once per frame, whichever views are drawn;
  - a double-click or key that opens a file opens the one clicked, also with Ctrl or Shift;
  - a check in `tests.sh` for each, that compares with a screen at rest (`settled`), not with
    a shot taken while thumbnails still come.
- **State indexed into the file list** (a `usize` or a set of them that means "this file")
  goes into `List` (list.rs), never into `App`: a new list is a new `List`, so such state cannot outlive
  the list it was made for. What should follow a file to the new list is carried over by name
  in `replace_files`.
- **A UI text:** in `tr("…")` or `tf!("…", name)`, with its translation in every `po/*.po`
  (see Language).
- **Non-trivial logic** (a parser, a loop with edge cases) gets one small `#[test]` that fails
  if it breaks. Tests must clean up the files they create.
- **A parser of file contents** also gets a `survives_broken_*` test that feeds it
  `crate::support::test_support::broken(sample)`: 2,000 damaged forms of a good input in every run, five
  million every Sunday (`.github/workflows/parsers.yml`, `GLIMT_FUZZ`).
- **Touching the disk or starting a program** goes through one function each, and
  `clippy.toml` (`disallowed-methods`) refuses the raw calls, so `cargo clippy` fails on a
  bypass:
  - read a file whole with `decode::read_limited` / `read_text` (a size cap, no pipe or device);
  - write whole or not at all: a new file with `file_io::create_file`, a file of the user's
    (a copy saved over one, a sidecar) with `replace_name`, which replaces a link at the name
    itself and refuses a read-only file, Glimt's own files with `replace_file` or
    `write_atomic`, which write through a link to its target, and a file the windows share with
    `file_io::edit_shared` (lock, read, change and write as one; clippy refuses `file_lock`
    elsewhere). `source_rules::only_glimts_own_files_are_written_through_links` lists where
    `replace_file` and its writers are used;
  - rename with `file_io::rename_never_over` (never over a file), move or copy with `put_file`;
  - start Glimt's own program with `external::command` or `stoppable_command` (in a Flatpak's
    sandbox), the user's with `host_command` or `stoppable_host_command` (on the host), and
    wait for it only with `run_limited`, `short_output_of`, `output_of` or a `Watchdog` (never
    `.output()`/`.status()`);
  - make a copy of the shown image from `App::shown_copy` and save it with `save::save_copy_with_sidecar`.
  `clippy.toml` has the whole list, each call with the way to take instead. A place that must
  do otherwise gets `#[allow(clippy::disallowed_methods)]` on the narrowest statement or
  function, with its reason on the same line or the next.
- **A shortcut with a known limit** (work in the UI thread, a whole file rewritten) gets a
  comment `shortcut: <the limit>; <when to do more>`, so the next reader knows it was chosen.

## What must be fixed, and when

Audits always find more; this is the bar their findings are ranked against, so a release is
not spent polishing whatever the last one dug up. 0.35–0.41 had 264 fixes for 30 features, a
third of them fixes of fixes, none reported by a user.

- **Must fix** — blocks a release, and is the only reason for a dot release:
  - a user's file lost, changed or written over wrongly, or its metadata (sidecar, marks and
    stars, collection entry) lost or put on another file;
  - a crash, or a window or operation that hangs, in normal use;
  - a wrong result in normal use: another file shown, saved, moved or deleted than the one
    meant, a feature that does not do what it says;
  - a security hole with a realistic way in: a file or folder that came with a download,
    another user on the computer, a program that can reach Glimt's socket.
- **Should fix** — goes into the next minor release: slowness at the scale in *Goals* (500,000
  files), a risky piece of logic without its check, debt that makes the next bug of a known
  kind likely (see the classes in *How to add things*).
- **Hardening** — goes into the next minor release, never a dot release: what needs something
  planted in Glimt's own folders, a deliberate race between two windows at the same moment,
  scale far past the goals, or a stalled share that only delays. Noted as a GitHub issue
  labelled `hardening` until then.

An audit or review ranks each finding against this bar and says why; one it cannot place is
should fix.

### Fix the root, once, and right the first time

Findings come in classes, and a class fixed one symptom at a time comes back. After an audit
or review, the findings are grouped by what lets them happen before anything is fixed: a rule
held in two places, a way in that goes around the one function for it, a setting of the view
that reaches what is written to a file, an action that does not say which file it acts on, a
new view of the list that misses what every view needs. Each class is then fixed where it
starts, in the one shared place, with a test or a `source_rules` check that fails when new
code opens the class again. The branches of a release follow the classes, not the findings.
New features are built the same way: what they share with the rest goes through the shared
place from the start.

A fix is right the first time when it closes every way in. Before the change, list each path
that reaches the code it guards (its callers, the actions and `--remote` commands that end
there, the background threads and the moments between a start and its answer) and check the
fix against each. 0.65.1's list that named a folder took three audit rounds, because the
first fix closed one way in and the next two were found by the audits.

## Releases: two tracks

- **A minor release (X.Y.0)** gathers new features and structural work: one or two features at
  a time, each on its own branch, never a batch of them. 0.38.0 shipped eleven new things and
  0.40.0 eight, and the next releases were spent fixing them.
- **A dot release (X.Y.Z)** fixes only *must fix* bugs of the release before it, nothing else:
  no hardening, no refactoring, no feature. What else is found waits for the next minor.

## Before every commit

```sh
cargo fmt
cargo clippy --profile quick --all-targets -- -D warnings
cargo test --profile quick
cargo clippy --profile quick --all-targets --all-features -- -D warnings
cargo test --profile quick --all-features
python3 tools/sbom.py        # keeps sbom.cdx.json and THIRD-PARTY-LICENSES.txt in step with Cargo.lock
python3 tools/flatpak-sources.py  # and flatpak/cargo-sources.json, the Flatpak's crates
python3 tools/pot.py         # and po/glimt.pot, after a text is added or changed
```

- **A branch per feature or fix**, named after it (`flatpak`, `slideshow-transitions`), merged
  into `main` when a release is made, so that a release gathers several changes.
- **One feature or fix per commit.** Formatting-only changes in a commit of their own.
- **Every commit passes every check above on its own**, both builds. A commit that breaks the
  build or a test, fixed by the next one, cannot be bisected or reverted on its own; amend it
  before it is pushed instead.
- **A fix lands with its check in the same commit:** a unit test, or for the window code a
  section in `tools/release-tests/tests.sh` that fails on the release before it. A fix without
  one is not finished; 0.37–0.38 showed that a fix nobody checked is the next release's
  regression.
- Use `--profile quick` (Cargo.toml) for clippy and tests: it is optimised as the release, so
  the tests' timings hold, but builds in many parts and incrementally, so a small change builds
  in a fraction of the time. A debug build besides would double the space in `target/`.
- `tools/check.sh` runs the five checks above in Docker (`rust:1` with mold and sccache), for a
  computer without cargo or to run several worktrees at once. CI runs them too on every push to
  any branch, so a branch is checked there without a local build. Each checkout keeps a build
  folder of a few GB in the volume `glimt-target`; after removing worktrees, `tools/check.sh
  --clean` removes the folders of those that are gone (a clone elsewhere is built again). Read
  its exit status directly, not through a pipe such as `| tail`, which hides a failed check.
- A release: the version in `Cargo.toml` and in the header of `docs/glimt.1`, a section in
  `CHANGELOG.md`, a `<release>` line in `assets/io.github.thornbux.Glimt.metainfo.xml`, and a
  tag `vX.Y.Z` on `main` (the release workflow checks each). New command-line options go in the man page too.
- New thumbnails are saved to the shared cache by default (`save_thumbnails`). Test with a
  separate `HOME` and `XDG_CACHE_HOME`, so test runs leave nothing in the real cache.

## Before a release

**Audited before the tag, not after.** A release is tagged only when an audit of its own
changes (everything since the last tag) finds nothing it must or should fix by the bar above.
What it finds is fixed on the release branch, and those fixes are audited again, until a round
finds nothing; hardening findings become issues instead. The audit reads the diff without the
author's conclusions (another agent, or another session). Up to 0.40.0 every release was one
audit round run after its tag, so each release shipped the last round's fixes unaudited and
the next release fixed them; 0.41.0 was audited four rounds before its tag.

Run `tools/release-tests/run.sh` on the release branch, on the very commit that gets the tag:
a change after the run means a new run, and a release the same day as the last one gets the
whole suite too, never a patch release on trust. The run builds the branch and the last
release with every feature in Docker (`rust:1`) and runs `tools/release-tests/tests.sh` there:
a virtual X display, an ordinary user, a separate `HOME` and XDG folders, generated pictures,
and a script named `zenity` early in `PATH` that answers for the file dialog. The checks run in
as many containers at once as Docker's processors and memory allow (two processors and 2.5 GB
each, 1 to 6; `PARTS=N` to choose), each with its own display, `/tmp` and
`HOME`; the timings run alone after them. A part may start at any section, so a section makes
what it needs itself (a function in the head of `tests.sh` for files several use). Each check prints PASS or FAIL; timings and memory are printed for both versions, to
compare. `QUICK=1` leaves out
the large folder and the EXIF timings. Screenshots of the steps are in
`~/.cache/glimt-release-tests/out`.

- **Saving turned:** a PNG, a lossless WebP and a lossy WebP with EXIF orientation 6, turned and
  saved with Auto Rotate on and off: EXIF kept, orientation 1, pixels as the view showed them.
- **Saves at once:** `a.png` and `a.webp` of 6000×4000 turned one after the other: both whole.
- **Subfolders:** with Include Subfolders on and auto refresh off, copy and move to the folder
  and to subfolders; the list must follow.
- **A failed delete** in a read-only subfolder: the file is in the list again.
- **Thumbnails:** with Auto Rotate off, nothing new in `$XDG_CACHE_HOME/thumbnails`.
- **Rename Many failing halfway:** names that swap and one too long (over 255 bytes); every name
  kept, no `.glimt-rename-*` left. A read-only file is no test, as Linux renames it.
- **Find Duplicates:** the checked files go to the trash, but nothing does once the list changed.
- **Pan view:** after another sort order each date keeps its image.
- **Printing** 80 images to a PDF: 80 pages, and memory that does not grow with them.
- **Real files:** each of `samples.txt` (RAW of eight makes, DICOM, FITS, NIfTI, a slide, HEIC,
  AVIF, JPEG XL …), downloaded once by `run.sh` and checked by sha256, is shown.
- **Large folder** (500,000 hard links) and **sorting 3,000 JPEGs by EXIF date:** "first image"
  time and RssAnon of both versions.
- **Known vulnerabilities:** `cargo audit`; the release workflow runs it too and stops on a
  vulnerable crate, while warnings such as an unmaintained crate do not stop it. Between
  releases `.github/workflows/audit.yml` runs it every Monday and when `Cargo.lock` changes,
  and Dependabot (`.github/dependabot.yml`) proposes updates of the crates and pinned actions.
  `.github/workflows/update.yml` opens a pull request every Tuesday with `cargo update` (the
  crates Cargo.toml does not name too) and the files after Cargo.lock, starts CI on it, and says
  when the Flatpak image's digest has moved.

By hand: the Flatpak, with two windows (Ctrl+N), `--remote next` to the focused one,
`--remote-all next` to both and `--remote-all quit`; Delete and Ctrl+Z (the trash portal), and of
a read-only file (which the portal refuses, so Glimt trashes it by hand), File →
Open File (the file chooser portal), Print through the System print dialog (the print portal;
its Print to File shows the PDF), full screen (an Inhibit call to the portal, and Close after,
in `dbus-monitor`), Open With → Choose Application… (the chooser, cancelled), a video and a
HEIC (the runtime's ffmpeg), File → Set as Wallpaper (the wallpaper portal), and a tool, which
without the host permission says how to give it and with it runs; the AppImage, with a second window (Ctrl+N)
that stays when the first one closes.

The window's logic (the list, the selection, file operations and what follows them) is tested
without a window: `crate::support::test_support::app_on(dir)` makes an `App` on `egui::Context::default()`
with settings of its own, listed, in milliseconds. A fix there gets such a test, in the tests of
the module the fix is in, not in main.rs (`source_rules::tests_sit_beside_their_code`).

A fix in the window code that a unit test cannot reach (painting, keys, clicks) gets a check in
`tests.sh`: the steps with keys, `--remote` and clicks by text (`menu File "Open With"`, through
the `test-hooks` build's `glimt --remote where LABEL`; a button or selectable row is drawn through
the `menus` helpers, which note it, as clippy refuses egui's own, and another widget the tests need
gets a `noted(…)` call), and the result read from the files or from a colour in a screenshot.
Screen coordinates only where a click's place is the point. Run it against the last release as
well, to see that it fails there.

## Packages

A tag `vX.Y.Z` pushed to GitHub runs `.github/workflows/release.yml`, which builds against
glibc 2.28 with cargo-zigbuild and publishes a `.tar.gz`, a `.deb`, an `.rpm` and an AppImage.
Both package formats are described in `Cargo.toml` (`[package.metadata.deb]`,
`[package.metadata.generate-rpm]`); new installed files go in both lists, in the Flatpak
manifest and in the tar.gz and AppImage steps. To build and check them locally, after
`cargo build --release --features full`:

```sh
cargo deb --no-build -o /tmp/pkg/          # cargo install cargo-deb
dpkg-deb -I /tmp/pkg/*.deb; dpkg-deb -c /tmp/pkg/*.deb
cargo generate-rpm -o /tmp/pkg/            # cargo install cargo-generate-rpm
```

- Dependencies are written by hand (glibc 2.28): `dpkg-shlibdeps` rounds glibc up to 2.34, and
  cargo-generate-rpm's own list has `[WEAK]` symbol versions that dnf cannot resolve. OpenGL
  (EGL) and xkbcommon are named too: the window loads them as it opens, so no tool finds them.
- `examples/tools` and `examples/formats` are installed as examples, not as active tools:
  in `/usr/share/doc/glimt/examples/` in the `.deb` and `.rpm` (scripts 755, so they work when
  copied), and as `examples/` in the tar.gz. Glimt reads `/usr/share/glimt/{tools,formats}`
  (and the old `{plugins,decoders}`),
  but nothing is put there: each example needs programs the package cannot count on (Ollama,
  spd-say, exiftool, ImageMagick, Python modules), and would sit in the menu for everyone and
  fail. A user copies the ones wanted to `~/.local/share/glimt/{tools,formats}` (Edit →
  Tools → Open the Tools Folder). Not in the AppImage or Flatpak, as no README is: their
  files are out of the user's reach. A new example script goes in by the globs already there.
- A local build links against the build machine's glibc; only the release binary runs on 2.28.
- Test an `.rpm` without rpm tools in a container, with the storage on `/tmp`:
  `podman --root /tmp/pod --runroot /tmp/podrun --storage-driver vfs run --rm -v /tmp/pkg:/pkg:Z
  registry.fedoraproject.org/fedora:latest bash -c 'dnf -y install /pkg/*.rpm && glimt --help'`
  (also `docker.io/library/almalinux:8` for glibc 2.28). Remove the storage afterwards with
  `podman unshare rm -rf /tmp/pod /tmp/podrun`.

Each release also gets `SHA256SUMS` of its files and a build provenance for each (signed by
GitHub through `actions/attest-build-provenance`), which `gh attestation verify FILE -R
thornbux/glimt` checks; a new file to publish goes in both, and the Flatpak's line is added by
its own job.

The AppImage holds the same binary, its desktop file, icons, man page and licenses; OpenGL,
Wayland and X11 come from the system, as for the tar.gz. `appimagetool` and the type2 runtime
(which needs no libfuse2) are pinned releases with their sha256 in `release.yml`; change both
together. Inside the AppImage, `$APPIMAGE` names the file itself: New Window starts that, as
the binary's mount goes when the first window closes; only when the binary is inside
`$APPDIR`, as every program started from an AppImage inherits its `$APPIMAGE`.

The release workflow also builds a Flatpak bundle from `flatpak/io.github.thornbux.Glimt.yml`, in
Flathub's `freedesktop-26.08` image, pinned by its digest (change both with `runtime-version`), and adds it to the
release in a job of its own. It builds without the network, as Flathub does: the crates are
sources in `flatpak/cargo-sources.json`, so a `Cargo.lock` changed without it fails there. Dependabot does not move a container's digest: before a release,
take the tag's current one (the command is in `release.yml`), which brings the image's fixes. To build it locally: its runtime, SDK and Rust extension take about
4 GB, so they can live on `/tmp` through `FLATPAK_USER_DIR`:

```sh
export FLATPAK_USER_DIR=/tmp/fp/flatpak
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    --state-dir=/tmp/fp/state /tmp/fp/build flatpak/io.github.thornbux.Glimt.yml
flatpak run io.github.thornbux.Glimt
```

- **Flathub** builds its own repository, `flathub/io.github.thornbux.Glimt`, not this one: after
  each release tag, `python3 tools/flathub-manifest.py vX.Y.Z <its checkout>` writes the
  manifest with Glimt from the tag (and its commit) and `cargo-sources.json` there, for a pull
  request; Flathub builds it and publishes once it is merged. Check it with Flathub's linter:
  `docker run --rm -v <dir>:/m ghcr.io/flathub/flatpak-builder-lint manifest /m/io.github.thornbux.Glimt.yml`.
  On a local build (`builddir`) the linter's only complaints are the screenshots and icons not
  mirrored, which Flathub's builder does.
- Glimt's own external programs go through `command()` or `stoppable_command()` in
  `src/files/external.rs`, which in the Flatpak run the runtime's, in the sandbox (Flathub allows no
  `flatpak-spawn --host`). The user's own programs, and those only the host has, go through
  `host_command()` or `stoppable_host_command()`: on the host, after `flatpak override --user
  --talk-name=org.freedesktop.Flatpak io.github.thornbux.Glimt`, else an error saying so.
- To run it in a virtual X display, add `--nosocket=wayland --nosocket=fallback-x11
  --socket=x11`, and `--filesystem=/tmp/...` for test files: `/tmp` is private in the sandbox.

## Privacy

Screenshots and examples must never show the maintainer's own folders or files.
`tools/screenshots/run.sh` makes them from generated pictures in a virtual X display; after a
change to the look, run it and compare with `git diff`. The website copies `docs/screenshots/`.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
