# AGENTS.md

Guidelines for coding agents (and people) adding code to Glimt, a fast, lightweight image viewer
for Linux in Rust/egui, inspired by Geeqie. Read this before changing anything.

## The rules that are never broken

1. A user's file or its metadata is never lost, changed or written over wrongly, and a failed
   write is told to the user (*What must be fixed, and when*; *Errors, panics and `unsafe`*).
2. An action acts on exactly the files the user picked, never on the shown image in their
   place (*How to add things*, "An action").
3. Every file, folder name, sidecar and socket message is untrusted input (*How to add
   things*, "Touching the disk").
4. What is frozen is added to, never taken away or changed in meaning (*Frozen in 1.x*).
5. Every commit passes every check in *Before every commit* on its own, in both builds, the
   translations' tests among them (every new UI text in all fourteen languages, *Language*).
6. A check is never weakened to make it pass (*What an agent decides, and what it asks*).
7. Nothing heavy runs on the UI thread (*Goals that decide every change*).
8. The disk is touched and a program is started only through the one function for each
   (*How to add things*, "Touching the disk").
9. A fix lands with a check that fails without it, in the same commit (*Tests*).
10. A release is audited before its tag, until a round finds nothing (*Releases*).
11. A branch may be pushed for CI at any time. `main`, a tag, a version bump and the website
    change only when the maintainer asks (*Releases*).
12. A check that was not run is reported as not run, never as passed (*What an agent
    decides, and what it asks*).

## Goals that decide every change

- **Fast and snappy, always.** Nothing heavy on the UI thread: decoding, folder listing, file
  operations, mounting and external programs run in background threads and report back over
  `mpsc` channels, with a repaint request. Show the busy pointer while they run (the `busy`
  count, or a `threads::Working` held for the work) when the user started the work and waits
  for it. A look-up that nobody waits for, such as the folder's size in the status bar or the
  list of printers, shows no busy pointer, so the pointer is not busy while the user can go
  on. The UI thread does not even look at a
  user's file or folder (a stat, a real path, a read), because on a share that stopped
  answering that waits for ever and the window hangs.
  Glimt's own files are no exception, because a home folder can be on such a share too: they
  are read before the window opens (`main`) or in a thread.
  `source_rules::the_window_thread_never_waits_for_the_disk` finds such a call in the functions
  of `App` and of its parts, also through every function of the crate that reaches one,
  however deep, and lists the few that stay with their reasons.
- **As fast with 500,000 files in a folder as with 10.** No O(n) work per frame, no system call
  per file in the list (use `DirEntry::file_type`), draw lists with `show_rows`, compare file
  names as bytes. Test large folders with hard links to one image, for example
  `mkdir big && for i in $(seq 500000); do ln a.jpg big/$i.jpg; done` on tmpfs.
- **Little memory.** Decode to screen size; full resolution only when zoomed. Preloading has a
  fixed budget (N images on each side). Measure RssAnon before and after changes that touch
  decoding or caches.
- **The limits a release is held to.** The release tests fail when, in a folder of 500,000
  files, the first image takes more than 1.5 times the last release's time or RssAnon is more
  than 1.5 times the last release's; when the median frame takes more than 10 ms of the window
  thread's processor time with all 500,000 files selected; or when sorting 3,000 JPEGs by EXIF date takes
  more than 1.5 times the last release's time (each with a small slack, `within` in `tests.sh`).
  The maintainer sets these numbers, and an agent does not change them. The other timings and
  memory figures in the release tests (`note` lines) are only printed, to compare: they stop
  nothing.
- **Simplest thing that works.** No dependency, abstraction, config option or file before it is
  needed. Prefer the standard library, then crates already in `Cargo.toml`, then a few lines of
  code. Heavy formats go through external programs (ffmpeg) rather than linking codecs.

## Behaviour

- Shortcuts and behaviour follow **Geeqie's defaults** (see its keyboard reference). Every
  deliberate deviation goes in the `DIFFERENCES` table in `src/ui/help.rs`, which the Help Manual
  (F1) shows. Remove a row when Glimt starts behaving like Geeqie. The keys are Geeqie's up to
  3.1: where Geeqie 3.2 changed a default key, Glimt keeps its own, and `DIFFERENCES` says so.
- New user-visible features are described in `src/ui/help.rs` (manual, shortcuts, keyboard map)
  and, where relevant, on the website (the `glimt-web` repository, glimt.pario.no). `README.md`
  keeps only what the website does not have: building from source, the optional features'
  table, the runtime dependencies and restoring a backup.

## Frozen in 1.x

What users' settings files, scripts and own files depend on is frozen from 0.76, through all of
1.x, and stays as it is until a 2.0: the command-line options, the `--remote` commands with
their answers and exit codes, the settings file's keys and values, the files Glimt owns, and
the desktop file formats of tools and added formats. Add to them; never take one away or change
what it means. A list of choices grows at its end only (`config::NAMED`). A setting that goes
out of use is still read, and ignored.

`src/frozen-1.0.txt` pins these names, taken from the code's own tables:

- the settings' keys; the values of each `config::NAMED` list in order; the values of `sort`
  (`SORTS`), `layout` and `slideshow_one_image_at_a_time` (`LAYOUTS`, also `--layout`'s),
  `rendering_intent` (`INTENTS`), `input_profile`'s built-in words (`INPUT_PROFILES`),
  `print_paper` (`PAPERS`), `print_orientation` (`ORIENTATIONS`), `print_printer`'s own values
  (`print::SYSTEM_DIALOG` and `PDF_FILE`), `theme` (the built-in themes) and `language`
  (`LANGUAGES` and `lang::ENGLISH`); the window state's keys (`WindowState`, `LAST_DIR`);
- the `--remote` commands and the words of their choices (`on|off`, from `COMMANDS`), the
  actions `--remote run` takes (`script` lines, from `SCRIPT_ACTIONS`, which only grows), and
  the exit codes: `--remote`'s `cli::DONE` and `remote::NOT_TAKEN`, `NOT_SENT` and `NOT_DONE`,
  and those of `--check-config` and `--back-up` (`cli::CHECK_CONFIG_PROBLEMS`,
  `BACK_UP_FAILED`, `BACK_UP_NO_FOLDER`);
- the command-line options of `START_OPTIONS` with `--remote` and `--remote-all`;
- the `%field%` names of the templates (`field` lines). Each is checked to be filled, for a test
  image, in the templates it belongs to: the full screen info and the print caption (`name`,
  `path`, `position`, `dimensions`, `marks`, `keywords`, `comment`, and the EXIF labels of the
  default full screen info), the window title (`name`, `path`, `folder`, `app`) and the page
  header and footer (`page`, `pages`). `%%` is a `%` in each of them;
- the keys of a theme file (`theme_key`, from `Theme::file_from`, each read by
  `Theme::parse`);
- the files and folders Glimt owns (`file` lines, from `config_path` and the other functions
  that name them, `LOCK_FILE`, `RENAMED_FOLDERS`, `remote::socket_name` and `FOCUSED_FILE`);
- the words of the groups in the sharing file (`sharing` lines, `Group::key`), and the names
  of the single tags and parts it holds (`sharing_tag` lines, from
  `shared_copy::names_of_single_tags`): each EXIF tag's name, one of each form a tag or part
  without a name of its own gets ("Image 0x…", "JPEG APP…", "PNG chunk …"), and the parts'
  names.

The tests that hold this:

- `config::tests::frozen_names_are_kept` fails when a pinned name is taken out, renamed or no
  longer filled, when a `NAMED` list gains a value anywhere but at its end, and until a new
  name is added at the end of the frozen file.
- `the_parser_takes_every_frozen_option` fails when an option that the help lists is taken
  neither by `Start::parse`, nor by `cli::answer_without_window` (through
  `cli::Command::named`), nor by `--remote`.
- `a_tools_desktop_keys_and_codes_are_kept` and `a_formats_desktop_keys_and_codes_are_kept`
  hold the desktop files of tools and added formats. An added format is read under
  `[Glimt Format]` and `[Glimt Decoder]` alike, and the `%` codes other than `%f %F %u %U %%`
  are reserved for later.
- `keeps_what_it_does_not_understand`: the marks file keeps mark bits and lines it does not
  understand, when a line has two fields and a path, so a later Glimt can add to it.
- The `keys=` format is pinned by `src/keys-1.0.txt` and its test: every action (also one
  without a default key) and every default key, each line with its kind (`action Delete`,
  `key Delete`), which the test checks each on its own. So never rename an `Action` variant or
  change a key's name, except through the tables of old names below. A new action goes at the
  end of that file as `action Name`, and the test fails until it is there.

Every `--remote` command gets an answer. A query is answered with what it asks for. Any other
command is answered with nothing when it was done, and with `not done: <reason>` when it was
not: a `run` refused while a dialog is open or a field has the focus, an action that
`App::why_refused` refuses (a kiosk's, say), or a command or choice the window does not know.
The client then exits with `NOT_DONE` (3). A window from a version before these answers closes
the connection without one, which the client takes as done.

No test pins the following, so they are kept by hand:

- how the window itself reads each `--remote` command: `remote_command` matches the words a
  second time, apart from `COMMANDS`;
- what the commands print;
- the formats inside the files Glimt owns: `key=value` read by name, the marks file's
  `bits\tstars\tpath`, the keyword tree's `A|B|C`, a path to a line;
- the reserved `%` codes of `Exec`, which the manual names.

A frozen name that misleads its reader is renamed only through a table of old names. There is
one table for each kind of name, beside the code that reads it: `config::RENAMED_SETTINGS`,
`config::RENAMED_STATE`, `actions::RENAMED_ACTIONS` (for `keys=` and `--remote run`),
`remote::RENAMED_COMMANDS`, `theme::RENAMED_THEMES` and `tools::RENAMED_FOLDERS`.

- Glimt reads the old name too, forever, and writes only the new one. When a settings or state
  file holds both, the new one counts.
- The old line stays in the frozen file and the new name is added at its end. The tests accept
  an old name only when its table maps it to a name that is read now.
- `theme::RENAMED_THEMES` is for the built-in themes, as the theme setting stores them. A theme
  file of the user's own keeps its old name, and Save as Theme refuses an old name.
- `tools::RENAMED_FOLDERS` is for the folders of tools and added formats. Glimt creates them
  only under the new name.
- A retired setting keeps its key. Its value is read once into the setting that took its place,
  and from then on it is written as its default (`app_name_first`, moved into
  `window_title`).
- Every old value, of a retired setting or a renamed theme, is mapped in one function,
  `config::old_values_mapped`, which `Config::parse`, `parse_checked` and the save's `merged`
  all call.

## Language

English for everything in the repository: UI text, identifiers, comments, commit messages and
documentation.

The interface ships in fifteen languages: English and the translations in `po/*.po` (Czech,
Danish, German, Spanish, Finnish, French, Icelandic, Italian, Dutch, Norwegian bokmål and
nynorsk, Polish, Brazilian Portuguese and Swedish), listed in `lang::LANGUAGES`. **Every new UI
text, `tr("…")` or `tf!("…")`, is translated into each of the fourteen `po/*.po` files in the
same commit**:
`po/no_nb.po` first, then the others.

- `lang::tests::every_language_has_every_text` fails while a language lacks a text of
  `no_nb.po`, and `translations_keep_placeholders` while a translation drops a `{name}`. A
  `tf!` whose text has a `{name}` it is not given, or that is given a name its text lacks,
  does not compile.
- Keep the key names as the menus show them (Ctrl, Shift, Enter, Esc, Delete), and the
  `%field%` template names unchanged.
- Use only the double quotation marks (`«»`, `“”`, `„`, `”`), one style of quotes in every
  language. The single ones (`‘`, `‚`, `‹`, `›`) are left out by choice, as Inter has them;
  the corner brackets (`「」`) also because Inter lacks them.
- A noun counted with `count` has the forms that its language's `plural_form` picks from: two
  in most languages, three in Polish and Czech (one, few, many).

`po/glimt.pot` is the template for a new language: no_nb.po with every translation empty,
written by `python3 tools/pot.py` (see *Before every commit*).
`lang::tests::the_template_has_every_text` fails while it lacks a text of no_nb.po. A new
language is added with its `.po` file, a line in `lang::LANGUAGES` and, unless its plural
rule is "one and many", its rule in `plural_form` and in the table of
`plural_form_follows_each_po_header`, which fails while a `.po`'s Plural-Forms header is not
there or `plural_form` picks other forms than it says.

The translations are written by the agent that adds the text, in all fourteen languages, and
no native reader has read any of them yet. The tests check only that each text is there and
keeps its placeholders, not that it reads well. The maintainer decides who reads which
language, and how a text that a native reader has checked is told apart from one that is not;
until then an agent does not invent a way to mark them. An agent changes another text's
translation only when it is wrong, and says so in the commit message.

## Code layout

`src/` holds main.rs (the app and its frame), cli.rs (the command line) and eight folders, in the
order a reader meets them: `model/` (what is open), `viewer/` (the image shown), `ui/` (the window
around it), `features/` (the features with a window of their own), `images/` (from a file to pixels
and back), `metadata/` (what is said about an image), `files/` (how files change on disk) and
`support/` (what the rest is built on). Each folder has a file of the same name beside it that
holds only its `mod` lines, and a new module goes into the folder of what it does
(`source_rules::modules_live_in_their_folder`).

| File | What a reader finds there |
|---|---|
| `src/main.rs` | `App` and its parts, starting up, the frame (`eframe::App`) and `main()` |
| `src/cli.rs` | The start options (`START_OPTIONS`) and the commands answered without a window |
| `src/model.rs` | **The model/ folder:** what is open: the file list, folders, collections, search, selection |
| `src/model/list.rs` | `List`, the file list and every state indexed into it; `LoadedImage`, an image decoded for the view |
| `src/model/folder.rs` | Opening and listing folders and collections, following changes, the status bar's sizes |
| `src/model/selection.rs` | Moving through the list, and the selection |
| `src/model/listing.rs` | Image extensions (`EXTS`), reading a folder or tree, sorting (`Sort`) |
| `src/model/collections.rs` | Collections: Geeqie `.gqv` files of images from any folders |
| `src/model/search.rs` | The search and its terms, and the EXIF index |
| `src/viewer.rs` | **The viewer/ folder:** the image area and what is done to the image shown |
| `src/viewer/slideshow.rs` | The slideshow |
| `src/viewer/view.rs` | The image area: decoding, zoom, panes (`LAYOUTS`), Compare, painting, the notice (`Notice`) |
| `src/viewer/crop.rs` | Edit → Crop |
| `src/viewer/save_as.rs` | File → Save As, and `App::shown_copy`, the copy of the image as shown |
| `src/viewer/pan.rs` | Pan view: the list by day, the timeline and the calendar |
| `src/ui.rs` | **The ui/ folder:** the window around the image |
| `src/ui/actions.rs` | `Action`, its labels, `SHORTCUTS`, the user's own keys, and `run`, which does each action |
| `src/ui/dialogs.rs` | The file chooser (zenity or kdialog) and the small dialogs |
| `src/ui/file_list.rs` | The side panel: folder list, file list or icon grid, thumbnails, the status bar |
| `src/ui/info_panel.rs` | The info panel (Ctrl+K) and what it edits |
| `src/ui/menus.rs` | The menu bar, the toolbar, and the helpers every menu is drawn with (`menu_item`) |
| `src/ui/window.rs` | The window's size, place and title, and the screen saver kept off |
| `src/ui/workers.rs` | The thumbnail workers, and what each frame collects from background work (`poll_background`) |
| `src/ui/accessibility.rs` | Optional screen reader support (AccessKit) |
| `src/ui/help.rs` | Help Manual, Keyboard Shortcuts, Keyboard Map, About, `DIFFERENCES` |
| `src/ui/preferences.rs` | Edit → Preferences |
| `src/ui/theme.rs` | The look: fonts, themes (`themes/*.theme` and the user's own), the desktop's style |
| `src/features.rs` | **The features/ folder:** features with a window or a job of their own |
| `src/features/check.rs` | File → Check Images |
| `src/features/copy_for_sharing.rs` | File → Copy for Sharing: its dialog and its job |
| `src/features/duplicates.rs` | Find Duplicates |
| `src/features/tools.rs` | Tools (Geeqie's plugins): desktop files whose command runs on the selected files |
| `src/features/print.rs` | Printing: a PDF of the images, sent to `lp` or saved |
| `src/features/remote.rs` | `--remote`, `--remote-all` and `--name`: the socket, `COMMANDS`, the sender and the listener |
| `src/images.rs` | **The images/ folder:** from a file to pixels and back |
| `src/images/decode.rs` | A file to pixels at the size shown, the shared decode budget, and `read_limited`, the one way a file is read whole |
| `src/images/svg.rs` | SVG and SVGZ (feature `svg`), drawn in a child process with limits |
| `src/images/added_formats.rs` | Added formats: programs in `glimt/formats` that turn other formats into images |
| `src/images/colour.rs` | Colour management: ICC profiles, the screen profile, the rendering intent |
| `src/images/save.rs` | Saving a turned image or a copy with its metadata (`save_copy_with_sidecar`, `Derived`) |
| `src/images/shared_copy.rs` | Copy for Sharing's copies: the parts of a file kept byte for byte, with new EXIF and XMP |
| `src/images/raw.rs` | Camera RAW: the largest embedded JPEG preview |
| `src/images/thumbs.rs` | The freedesktop thumbnail cache (`~/.cache/thumbnails`) |
| `src/images/video.rs` | The optional video feature |
| `src/metadata.rs` | **The metadata/ folder:** what is said about an image |
| `src/metadata/exif.rs` | The EXIF (TIFF) reader, and writing a JPEG's orientation |
| `src/metadata/lightroom.rs` | Import Lightroom Catalog, through `sqlite3` |
| `src/metadata/marks.rs` | Marks 1–6 and stars, kept in `$XDG_STATE_HOME/glimt/marks` |
| `src/metadata/xmp.rs` | XMP sidecars: keywords, comment, alt text, stars; sidecars that follow their files |
| `src/files.rs` | **The files/ folder:** how files change on disk |
| `src/files/config.rs` | `Config` (the `config!` macro), the window state, the XDG paths |
| `src/files/backup.rs` | File → Back Up and `--back-up` |
| `src/files/file_ops.rs` | File operations as jobs with progress and Stop (`Job`), and Undo |
| `src/files/file_io.rs` | The one way each change on disk is made (also `sync_folder_of`), the writer threads (`write_later` for Glimt's own files, `write_later_in_user_folders` for collections), `save_failed` |
| `src/files/trash.rs` | The trash, and putting a file back for Undo |
| `src/files/external.rs` | Starting programs (`command`, `host_command`), and ffmpeg, gio and Open With |
| `src/files/portal.rs` | The desktop portals through zbus (feature `portals`) |
| `src/files/journal.rs` | Rename Many's steps on disk, put right at the next start after a crash |
| `src/support.rs` | **The support/ folder:** what the rest is built on |
| `src/support/test_hooks.rs` | The release tests' hook (feature `test-hooks`): where a widget was drawn (`noted`) |
| `src/support/test_support.rs` | What tests start from: `app_on`, `contract_folder`, `broken` |
| `src/support/threads.rs` | Threads: how many for each kind of work, `Receiving`, `AnswerSender`, `catch_panic` |
| `src/support/format.rs` | Text for the user: sizes, counts, times, templates |
| `src/support/lang.rs` | The languages: the `.po` files, `tr`/`tf!`, the plural rules |
| `src/support/source_rules.rs` | Tests of the source itself, the rules of this file kept by tests |
| `po/` | The translations, one `.po` file per language, and `glimt.pot`, the template for a new one |
| `themes/` | The built-in themes |
| `assets/` | Icons, the desktop and AppStream files, the fonts and their licenses |
| `SECURITY.md` | How to report a vulnerability, and what counts as one |
| `docs/` | `RELEASING.md` (the release runbook), the man page `glimt.1`, `screenshots/`, and `architecture/`: diagrams of the code (update them when what they name changes) |
| `examples/formats/` | Added formats: desktop files and their scripts |
| `examples/tools/` | Tools: desktop files and their scripts, and `test-model.py`, a stand-in model |
| `flatpak/` | The Flatpak manifest, and `cargo-sources.json`, the crates it builds from offline |
| `tools/check.sh` | Runs the checks before a commit in Docker |
| `tools/sbom.py` | Writes `sbom.cdx.json` and `THIRD-PARTY-LICENSES.txt` |
| `tools/flatpak-sources.py` | Writes `flatpak/cargo-sources.json` from `Cargo.lock` |
| `tools/package-smoke.sh` | The release workflow's smoke test of the AppImage and the Flatpak bundle |
| `tools/pot.py` | Writes `po/glimt.pot` from `po/no_nb.po` |
| `tools/flathub-manifest.py` | Writes the Flathub repository's manifest for a release tag |
| `tools/release-tests/` | `run.sh` builds this and the last release in Docker; `tests.sh` checks them under Xvfb |
| `tools/screenshots/` | `run.sh` makes `docs/screenshots/` of generated pictures |

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

A name is imported from the module that defines it (`use crate::files::config::Config`),
never through main.rs's own imports (`crate::Config`). main.rs, too, names its modules by their
path, such as `crate::ui::actions::…` (`source_rules::imports_name_where_defined`).

Comments say why, or what a reader cannot see from the code. They are written for someone new
to the code, in plain English prose: whole sentences with a subject and a verb, with the
articles kept, and one thought to a sentence. Many older comments use a clipped style, where a
semicolon or colon joins a fragment (`…; None for no search.`) and punctuation stands in for
"because", "when" and "so". New and changed comments do not. An older comment is rewritten in
the new style when the code it describes changes, not on its own.

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
- Do not use a private image or a pet word for what the code does. A reader must look up
  `land` ("save a copy and its sidecar") or `hold` ("hold the files back while other work
  changes them"), but not `save_copy_with_sidecar` or `hold_back_while_writing`.
- Name a function for what it does or gives back (`read_sidecar`, `selected_paths`), a `bool`
  as a question that its value answers (`is_hidden`, `has_sidecar`), and a type for what it
  holds. A type that every file operation returns is not named after one of them (`Deleted`).
- One thing has one name throughout: the same word in the type, its fields, its functions and
  the comments.

Older names that broke this were renamed in 0.74.0.
`source_rules::retired_names_stay_gone` keeps them out of the code; its list gives each old
name with the one that took its place. `Action` variants and the names in `src/frozen-1.0.txt` keep
their names, or change only through a table of old names, as *Frozen in 1.x* says.

A function is at most 120 lines outside the tests (`source_rules::functions_stay_short`). A
longer one is split into functions named for what each part does, beside it or in the module
of that part. The exceptions are the tables with one arm per action (`Action::english`,
`Action::icon`, `App::run`). Their arms stay a few lines long, and a longer arm becomes a
method.

## How to add things

- **An action:** add a variant to `Action`, `action Name` at the end of `src/keys-1.0.txt`,
  its menu label in `Action::label`, and a key in `SHORTCUTS` if Geeqie has one. Put Shift and
  Ctrl+Shift combinations before the plain key, because egui ignores an extra Shift when it
  matches a key. Handle the action in `App::run`, and add it to a menu with `menu_item` or
  `menu_toggle`.
  - Name what it acts on in `Action::target`: the selection (`App::targets`), the shown image,
    or no file. A menu about the selection, such as the file list's right-click menu
    (`file_menu`), offers only actions on the selection.
  - `targets()` returns `None` when the search hides every selected file, and the action then
    stops: the shown image must not stand in for the files the user picked.
  - A feature or dialog that selects the files it acts on (Find Duplicates, Check Images,
    Rename Many) does so with `select_for_action`, which refuses while the search hides any of
    them.
- **A setting** (asked for first, as it is frozen once released): one line
  `name: Type = default` in `config!`; values are read with `FromStr`
  and written with `Display`. Add a checkbox in Preferences, and clamp ranges in
  `Config::parse`. Settings stay in `Config` even when their feature is not built in, so saving
  never drops them. A setting that has a setter (`set_…`, or `toggle_setting` for a View
  toggle) is changed only through that setter, also from Preferences, so that whatever else the
  menu does happens there too (`source_rules::settings_change_through_their_setters`).
- **A format:** built in when the `image` crate reads it with no new dependency (its feature in
  `Cargo.toml`, the extension in `listing::EXTS`, a test in `decode.rs`). Otherwise it is an
  added format in `examples/formats` (read by `src/images/added_formats.rs`) with a check in
  `tests.sh`, not code or a dependency in Glimt.
- **An optional feature** (chosen when building, not a tool): a Cargo feature in `[features]`
  (also add it to `full`, which `source_rules::full_has_every_feature` checks), a module or
  extension list behind `#[cfg(feature = "...")]`, and its settings shown in Preferences only
  when built in. Never break the default build. "Tool"
  means only the desktop files in Edit → Tools, and "added format" only those in
  `glimt/formats`. They were once plugins and decoder plugins, and their old folders are
  still read (`tools::RENAMED_FOLDERS`).
- **A file operation** (one that starts a job or background work) joins the contract in
  file_ops.rs's tests. It gets an entry in `contract_ops()` that says what it does to the
  contract's folder and what must hold afterwards (the file, its sidecar, its stars, held back
  while other work changes its files), and its function is listed under that entry's name in
  `IN_CONTRACT`. `every_file_operation_is_in_the_contract` fails until the function is there,
  or in `OUTSIDE` with the reason it is not, and also when a name in `IN_CONTRACT` has no entry
  in `contract_ops()`. An operation that leaves an Undo takes `undo_on_the_way` when it starts
  and holds it in its thread until its report is sent, so that Ctrl+Z waits for it
  (`source_rules::operations_that_leave_an_undo_make_undo_wait`).
- **State for a feature:** a field in the struct of the part it belongs to (`self.slideshow`,
  `self.folder`, `self.file_operations`, …), or a struct of its own in the feature's module,
  held by one field in `App`. Never loose fields in `App` (`source_rules::app_stays_small`).
  The parts' fields are private: other modules call their methods, named for what they do
  (`self.slideshow.toggle_pause()`, `self.panel.is_hidden()`), and never write their fields.
  `source_rules::parts_keep_their_fields` checks every part of `App`, `self.folder` and
  `self.list` among them: each struct of the crate that a field of `App` holds, so a new part
  is checked without being listed.
- **Background work:** spawn a thread, send the result over a channel stored in `App` as a
  `Receiving<T>` (never a bare `Option<Receiver>`), and take it in `update` with `take`, `next`,
  `latest` or `wait`, which also tell, once, when the thread is gone. An answer that the window
  must still act on (a report, a path looked up, a file saved) goes through a
  `threads::AnswerSender`, whose answer keeps the busy pointer on until the window has handled
  it, so the window never sees no work running in between
  (`source_rules::answers_keep_the_busy_pointer`). egui takes clipboard and
  other output only during a frame, so hand results to the UI thread instead of calling the
  context from a worker. A mutex is locked only with `threads::lock_even_if_poisoned` (clippy
  refuses `Mutex::lock`), because a panic that `catch_panic` caught may have poisoned it. Do
  not hold a lock across a `catch_panic`, a channel send or a wait. Two windows share only the
  files that `file_io::edit_shared` writes, and the socket.
- **A new way to show the list** (such as the grid in the image area, G) gets what every view
  of the list needs. The first grid missed these through five audit rounds:
  - its form from `file_list`'s parameters, never from the settings inside the functions that
    draw it, because two views can be drawn at once
    (`the_list_is_drawn_in_the_form_its_caller_asks`);
  - its keys read only while no dialog is open and no text field has the focus (`typing` in
    `read_keys`), and before the shortcut that would take them;
  - a screen reader's text: each row or cell through `speak_item`, and the shown image read
    out with `speak_image` when the view takes the image area's place
    (`every_clickable_widget_drawn_by_hand_speaks`);
  - a menu item shown ticked when choosing it would turn the view off, and not otherwise;
  - the notices drawn over it, as over the image;
  - drag and drop, the rubber band and the scroll to the shown image (`Panel::start_frame`)
    handled once per frame, whichever views are drawn;
  - a double-click or key that opens a file opens the one clicked, also with Ctrl or Shift;
  - a check in `tests.sh` for each of these, which compares with a screen at rest
    (`settled`), not with a shot taken while thumbnails are still coming in.
- **State indexed into the file list** (a `usize` or a set of them that means "this file")
  goes into `List` (list.rs), never into `App`. A new list is a new `List`, so such state
  cannot outlive the list it was made for. What should follow a file to the new list is carried
  over by the file's name in `replace_files`.
- **A command-line option or `--remote` command** (asked for first, as it is frozen): its line
  in `START_OPTIONS` or `COMMANDS`, its reading in `Start::parse` or `remote_command`, its name
  at the end of `src/frozen-1.0.txt`, the man page `docs/glimt.1`, and the help.
- **A user-visible change** gets a line under `[Unreleased]` in `CHANGELOG.md`, in its own
  commit.
- **A UI text:** in `tr("…")` or `tf!("…", name)`, with its translation in every `po/*.po`
  (see Language).
- **A notice** (the message over the image) is given with `self.notice.give(text)` and taken
  away with `self.notice.clear()`, never assigned. That way each notice gets a new number, by
  which a screen reader knows that the same text was given again
  (`source_rules::notices_are_given_through_their_setter`).
- **Non-trivial logic** (a parser, a loop with edge cases) gets one small `#[test]` that fails
  if it breaks. Tests clean up the files they create, under temporary names of their own
  (`source_rules::tests_use_temporary_names_of_their_own`).
- **A parser of file contents** also gets a `survives_broken_*` test that feeds it
  `crate::support::test_support::broken(sample)`: 2,000 damaged forms of a good input in every
  run, and five million every Sunday (`.github/workflows/parsers.yml`, `GLIMT_FUZZ`).
- **Touching the disk or starting a program** goes through one function each, and
  `clippy.toml` (`disallowed-methods`) refuses the raw calls, so `cargo clippy` fails on a
  bypass:
  - read a file whole with `decode::read_limited` / `read_text` (a size cap, no pipe or device),
    and open one with `decode::open_regular` (no pipe or device);
  - decode an image only through `decode::from_decoder`, `from_memory` or `scaled`, which
    check `fits_decode_limit` before any pixels are made;
  - write a file whole or not at all:
    - a new file with `file_io::create_file`;
    - a file of the user's (a copy saved over one, a sidecar) with `replace_name`. When the name
      is a symbolic link, it replaces the link itself, not the file the link points to, and it
      refuses a read-only file;
    - Glimt's own files with `replace_file` or `write_atomic`, which follow a symbolic link and
      write its target (`source_rules::only_glimts_own_files_are_written_through_links` lists
      where these are used);
    - a file that several windows share with `file_io::edit_shared`, which locks, reads,
      changes and writes it as one step (clippy refuses `file_lock` elsewhere);
  - rename with `file_io::rename_never_over` (never over a file), and move or copy with
    `put_file` (never `std::fs::copy`). A rename that a later step counts on (an unlink,
    a plan removed) is first made lasting with `sync_folder_of`, which syncs its folder.
    `write_beside` and `put_file`'s move to another disk do this themselves; a plain copy
    and a thumbnail (`write_beside_without_folder_sync`) do not, as nothing counts on them, and
    `source_rules::renames_that_later_steps_count_on_are_synced` lists the renames that
    stand alone;
  - start one of Glimt's own programs with `external::command` or `stoppable_command` (inside
    a Flatpak's sandbox), and one of the user's with `host_command` or `stoppable_host_command`
    (on the host). Wait for it only with `run_limited`, `short_output_of`, `output_of` or a
    `Watchdog`, never with `.output()` or `.status()`;
  - make a copy of the shown image from `App::shown_copy`, and save it with
    `save::save_copy_with_sidecar`.

  Every file, folder name, sidecar, collection, `.lrcat` and socket message is untrusted: what
  is read from it has a size cap, a path taken from it is checked to stay where it should, and a
  program is given it as an argument, never as part of a shell script's text (SECURITY.md).

  `clippy.toml` has the whole list, each call with the way to use instead. A place that must do
  otherwise gets `#[allow(clippy::disallowed_methods)]` on the narrowest statement or function,
  with its reason on the same line or the next.
- **A shortcut with a known limit** (work in the UI thread, a whole file rewritten) gets a
  comment `shortcut: <the limit>; <when to do more>`, so the next reader knows it was chosen.
  A change to the code it names rewrites or removes it, so that it never names a limit that is
  gone or a way out that was turned down.

## Errors, panics and `unsafe`

These rules are enforced: `Cargo.toml`'s `[lints]` deny `unsafe_code`,
`clippy::undocumented_unsafe_blocks`, `clippy::unwrap_used` and `clippy::expect_used`, and
`source_rules::the_lints_stay_on` fails when one is taken out, lowered, or turned off for a
whole module. The tests may unwrap (main.rs allows it for them).

- **A failure is told, never swallowed.** A failed write of the user's data or of Glimt's own
  files goes to `file_io::save_failed`, which prints it and shows it once in the window. The
  result of a writer of file_io is never dropped with `let _ =`, `_ =` or `.ok();`
  (`source_rules::write_results_are_not_dropped`): handle it, or pass it on with `?`.
  A background thread tells a failed write through `save_failed` or
  `NoticeSender::send_failed_write`, never as a plain notice, and the window thread through
  `App::tell_failed_write`. Both keep the window open when a close
  waited for the writing. A Close is sent only by `App::ask_to_close`, and `guard_close` takes
  the failed writes before it lets one through
  (`source_rules::a_close_takes_the_failed_writes_first`).
- **An `unwrap` or `expect` outside the tests** is only where it cannot fail, with
  `#[allow(clippy::unwrap_used)]` (or `expect_used`) on that one statement and the reason on
  the same line. Elsewhere use `?`, a default, or an error the user is told.
- **A mutex is locked with `threads::lock_even_if_poisoned`**, the one way (clippy refuses
  `Mutex::lock`).
- **A panic in a background thread is caught in one place**, `threads::catch_panic`, around
  each file a worker reads or writes, so a broken file costs only that file
  (`source_rules::panics_are_caught_in_one_place`). When it returns `None` for a write, that is
  a failed write like any other, told to the user.
- **Every `unsafe` block has `#[allow(unsafe_code)]`** on that one statement or function, and a
  `// SAFETY:` comment that says why the call is sound. New `unsafe` needs a reason that no safe
  API covers, in the standard library or in a crate already in `Cargo.toml`.

## Dependencies

A new crate is the last choice: the standard library comes first, then a crate already in
`Cargo.toml`, then a few lines of code, and a heavy format goes through an external program
(*Goals that decide every change*). A crate that is still needed must:

- have a license compatible with MIT (*License*);
- be maintained, with recent releases, and have no advisory in `cargo audit`;
- bring few crates of its own (count what it adds to `Cargo.lock`);
- use no `unsafe` beyond what its job needs (a parser of file contents needs none).

The commit that adds it says what it was weighed against: the lines of code or the external
program it saves.

## What must be fixed, and when

Audits always find more; this is the bar their findings are ranked against, so a release is
not spent polishing whatever the last one dug up. Seven early releases had 264 fixes for 30
features, a third of them fixes of fixes, none reported by a user.

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
fix against each. One fix of a list that named a folder took three audit rounds, because the
first fix closed one way in and the next two were found by the audits.

## Releases

The steps of a release, the release tests' list, the checks by hand and the packages are in
[docs/RELEASING.md](docs/RELEASING.md). These are the rules a release follows:

- **A minor release (X.Y.0)** gathers new features and structural work: one or two features at
  a time, each on its own branch, never a batch of them. One release shipped eleven new things
  and another eight, and the next releases were spent fixing them.
- **A dot release (X.Y.Z)** fixes only *must fix* bugs of the release before it, nothing else:
  no hardening, no refactoring, no feature. What else is found waits for the next minor.
- **Audited before the tag, not after.** A release is tagged only when an audit of its own
  changes (everything since the last tag) finds nothing it must or should fix by the bar in
  *What must be fixed, and when*. What the audit finds is fixed on the release branch, and those
  fixes are audited again, until a round finds nothing. Hardening findings become issues
  instead. The audit reads the diff without the author's conclusions (another agent, or another
  session). When the audit ran after the tag, each release shipped the last round's fixes
  unaudited and the next release fixed them.
- **The release tests run on the very commit that gets the tag**
  (`tools/release-tests/run.sh`). A change after the run means a new run, also for a release on
  the same day as the last one.
- **Only the maintainer starts a release.** A branch may be pushed for CI at any time. `main`,
  a tag, a version bump and the website change only when the maintainer asks.

## Before every commit

```sh
cargo fmt
cargo clippy --profile quick --all-targets -- -D warnings
cargo test --profile quick
cargo clippy --profile quick --all-targets --all-features -- -D warnings
cargo test --profile quick --all-features
python3 tools/sbom.py             # sbom.cdx.json and THIRD-PARTY-LICENSES.txt, from Cargo.lock
python3 tools/flatpak-sources.py  # flatpak/cargo-sources.json, the Flatpak's crates
python3 tools/pot.py              # po/glimt.pot, after a text is added or changed
```

- **A branch per feature or fix**, named after it (`flatpak`, `slideshow-transitions`), made
  from the release branch `release-X.Y.Z` and merged into it, so that a release gathers several
  changes. The release branch goes into `main` when the release is made (docs/RELEASING.md).
- **One feature or fix per commit.** Formatting-only changes in a commit of their own.
- **Every commit passes every check above on its own**, in both builds. A commit that breaks
  the build or a test, fixed only by the next commit, cannot be bisected or reverted on its own,
  so amend it before it is pushed.
- **A fix lands with its check in the same commit:** a unit test, or for the window code a
  section in `tools/release-tests/tests.sh` that fails on the release before it. A fix without
  one is not finished; a fix nobody checked has been the next release's regression more than
  once.
- Use `--profile quick` (Cargo.toml) for clippy and tests. It is optimised like the release, so
  the tests' timings hold, but it builds in many parts and incrementally, so a small change
  builds in a fraction of the time. A debug build as well would double the space in `target/`.
- CI also runs `cargo check` in both builds with the oldest Rust that `rust-version` in
  Cargo.toml names, and so does `tools/check.sh`, so use no standard library function newer
  than that. CI runs `cargo fmt --check`, and builds with `--locked`, so a Cargo.toml changed
  without its Cargo.lock fails.
- `tools/check.sh` runs the five cargo checks above in Docker (`rust:1` with mold and sccache),
  and a `cargo check` in both builds with the Rust of `rust-version`, as CI does. Use it on a
  computer without cargo, or for several worktrees at once. CI runs the same checks on every push to any branch.
  Read its exit status directly, not through a pipe such as `| tail`, which hides a failed
  check.
- Each checkout keeps a build folder of a few GB in the Docker volume `glimt-target`. The first
  `tools/check.sh` in a new checkout, and `tools/check.sh --clean`, remove the build folders of
  worktrees that are gone and of clones of this repository elsewhere. So run it from a worktree
  of this repository; in a separate clone, or for `--clean`, ask first.

### Tests

The window's logic (the list, the selection, file operations and what follows them) is tested
without a window: `crate::support::test_support::app_on(dir)` makes an `App` on
`egui::Context::default()` with settings of its own, listed, in milliseconds. A fix there gets
such a test, in the tests of the module the fix is in, not in main.rs
(`source_rules::tests_sit_beside_their_code`).

A fix in the window code that a unit test cannot reach (painting, keys, clicks) gets a check in
`tools/release-tests/tests.sh`. The check does the steps with keys, `--remote` and clicks by
text (`menu File "Open With"`, found through the `test-hooks` build's
`glimt --remote where LABEL`), and reads the result from the files or from a colour in a
screenshot. A button or selectable row is drawn through the `menus` helpers, which note where
it is, because clippy refuses egui's own; another widget the tests need gets a `noted(…)` call.
Use screen coordinates only where a click's place is the point. Run the check against the last
release as well, to see that it fails there.

New thumbnails are saved to the shared cache by default (`save_thumbnails`). Test with a
separate `HOME` and `XDG_CACHE_HOME`, so test runs leave nothing in the real cache.

## What an agent decides, and what it asks

An agent decides on its own, within its task and these rules: the code, its tests and
comments, the names, the commits on its own branch, and how a finding ranks against *What must
be fixed, and when*.

It asks the maintainer before it:

- changes `main`, tags, bumps the version or changes the website (*Releases*); a branch of its
  own may be pushed for CI at any time;
- weakens a check: removes or loosens an assertion, takes a line out of `src/frozen-1.0.txt`
  or `src/keys-1.0.txt`, raises a limit of the tests, changes `clippy.toml`, or adds an entry to
  a check's list of exceptions (`OUTSIDE`, a `source_rules` list, `#[allow(…)]`). A new
  exception that the rules allow carries its reason, said again in the commit message;
- force-pushes, rewrites pushed history, or resets work it did not make;
- adds a dependency, a setting, a command-line option or a `--remote` command, because each is
  frozen once it is released;
- changes behaviour that follows Geeqie or that users rely on;
- removes what it did not make: a branch, a worktree, or the build folders of other checkouts
  (`tools/check.sh --clean`, or a first check in a separate clone).

It reports honestly:

- A check that was not run (no Docker, no X display, no time) is reported as not run, never as
  passed. Read a check's exit status directly.
- A finding that does not hold against the code is reported as such, and not "fixed".
- A test is shown to fail without its fix before the fix is called done.
- A finding outside the task is reported, not fixed on the side.

The checks run in a worktree of this repository, never in a copy made with `git archive`. Git
does not know such a copy, so what it checks is no commit, and its build folder of a few GB is
removed by the next check of any other checkout.

## Privacy

Screenshots and examples must never show the maintainer's own folders or files.
`tools/screenshots/run.sh` makes them from generated pictures in a virtual X display. After a
change to the look, run it and compare with `git diff`. The website copies `docs/screenshots/`.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
