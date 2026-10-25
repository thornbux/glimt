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
  deliberate deviation goes in the `DIFFERENCES` table in `src/help.rs`, which the Help Manual
  (F1) shows. Remove a row when Glimt starts behaving like Geeqie.
- New user-visible features are described in `src/help.rs` (manual, shortcuts, keyboard map)
  and, where relevant, in `README.md`.

## Frozen in 1.x

These stay as they are until a 2.0: the command-line options,
the `--remote` commands with their answers and exit codes, the settings file's keys and values
(a list of choices grows at its end only, `config::NAMED`), the files Glimt owns, and the
desktop file formats of plugins and decoders. Add; never take away or change a meaning. A
setting that goes out of use keeps being read and ignored. Never rename an `Action` variant or
rely on a changed key name: they are the `keys=` format, pinned by `src/keys-1.0.txt` and its
test; new ones go at the end of that file.

## Language

English for everything in the repository: UI text, identifiers, comments, commit messages and
documentation.

The interface ships in eleven languages: English and the translations in `src/*.po` (Danish,
German, Spanish, Finnish, French, Icelandic, Italian, Dutch, Norwegian bokmål, Polish and
Swedish), listed in `lang::LANGUAGES`. **Every new UI text, `tr("…")` or `tf!("…")`, is
translated into all of them in the same commit**: `src/no_nb.po` first, then the others.
`lang::tests::every_language_has_every_text` fails while one lacks a text of `no_nb.po`, and
`translations_keep_placeholders` while a translation drops a `{name}`. Keep the key names as the
menus show them (Ctrl, Shift, Enter, Esc, Delete), `%field%` template names unchanged, and only
quotation marks Inter has (`«»`, `“”`, `„`, `”`). A noun of `count` has the forms its language's
`plural_form` picks from: two in most, three in Polish (one, few, many).

## Code layout

| File | Contents |
|---|---|
| `src/main.rs` | The app: `App` and its fields by group, its decode, thumbnail and EXIF workers, opening and listing a folder, browsing, painting the image, the file list, menus, panels and dialogs, `run` for every action, and the command line. Also helpers other modules use: threads (`in_parts`, `background_threads`, `small_alloc_threads`), `Job`, text (`count`, `thousands`, `human_size`, `local_time`) and `crate::broken` for tests |
| `src/accessibility.rs` | Optional screen reader support: the shown image as text for Orca (AccessKit) |
| `src/actions.rs` | The `Action` enum with its menu labels, the `SHORTCUTS` table, the user's own keys; variants are stored by name, never rename one |
| `src/config.rs` | `Config` (the `config!` macro) with the settings' defaults and ranges, the window state, XDG paths; the writer thread (`write_later`, `flush`), `file_lock`, `write_atomic`, `--lock-settings`, and the backup (`own_folders`, `copy_tree`, `back_up_to`) |
| `src/decode.rs` | An image file to pixels at the size shown: scaled JPEG, SVG, animations, the histogram; `read_limited`, `read_text` and `read_head`, the one way a file is read whole |
| `src/decoders.rs` | Decoder plugins: programs in `glimt/decoders` that turn other formats into images |
| `src/colour.rs` | Colour management: ICC profiles, the screen profile (also from colord), rendering intent |
| `src/file_ops.rs` | Delete, copy and move, Rename and Rename Many, saving the orientation, as jobs with progress and Stop (`start_job`, `background`, `write_sidecars`); `put_file`, `rename_never_over` (the one way to rename), `rename_files`; Undo, with `Thrown` (what a move to the trash did) and `restore` |
| `src/files.rs` | Image extensions, reading a folder or tree, sorting (`Sort`, `natural_cmp`), the Types menu, `pattern_name` for Rename Many |
| `src/save.rs` | Saving a turned image with its EXIF and XMP, also lossy WebP; copies of the shown image (`CopyAs`, `copy_bytes`, `shown_image`) and `land`, the one way a copy and its sidecar are saved |
| `src/crop.rs` | Edit → Crop: the part dragged over the shown image, held by its path and the copy as shown |
| `src/save_as.rs` | File → Save As: its dialog, and `App::shown_copy`, the copy of the image as shown that Crop and Save As start from |
| `src/lang.rs` | The interface's languages: the `.po` files, `tr`/`tf!`, each language's plural rule |
| `src/external.rs` | External programs: `command()`, `stoppable_command`, `run_limited`/`limited`, `output_of`, `Watchdog`, `trash`; ffmpeg (HEIC, AVIF, video), gio mounting, open with |
| `src/help.rs` | Help Manual, Keyboard Shortcuts, Keyboard Map, About, `DIFFERENCES` |
| `src/collections.rs` | Collections: Geeqie `.gqv` files of images from any folders, and putting them in order (`moved`, `dropped`) |
| `src/duplicates.rs` | Find Duplicates: groups by checksum, name, size, dimensions or likeness, and its window |
| `src/exif.rs` | Minimal EXIF (TIFF) parser for the panel, the EXIF window's every field, the overlay and date sorting; writes a JPEG's orientation |
| `src/journal.rs` | Rename Many's steps on disk while they run, put right at the next start after a crash |
| `src/lightroom.rs` | Import Lightroom Catalog: a Lightroom Classic `.lrcat` read through `sqlite3`, `Catalog::plan`, the dialog and the import |
| `src/marks.rs` | Optional marks 1–6 and stars, kept in `$XDG_STATE_HOME/glimt/marks`, and stars in XMP |
| `src/meta.rs` | Keywords, comment, IPTC alt text and extended description in XMP sidecars, keyword tree paths; `update_sidecar` (also for stars); sidecars that follow their files |
| `src/pan.rs` | Pan view: the list by day, its rows, the timeline and the calendar |
| `src/plugins.rs` | Plugins: desktop files whose command runs on the selected files; `desktop_files` and `values`, which decoders.rs reads with too |
| `src/preferences.rs` | Edit → Preferences: the window and its sections |
| `src/print.rs` | Printing: a PDF of the images, sent to `lp` or saved, and the print dialog |
| `src/raw.rs` | Camera RAW: the largest embedded JPEG preview |
| `src/remote.rs` | `--remote`, `--remote-all` and `--name`: a socket for each Glimt in `$XDG_RUNTIME_DIR/glimt`, `COMMANDS`, the sender (`client`) and the listener |
| `src/search.rs` | The search: fuzzy names and `exif:`, `re:`, `mark:` and `stars:` terms with `|`, `-` and `( )`, in parallel; the EXIF index (`exif_index`) |
| `src/theme.rs` | The look: Inter and Phosphor fonts, themes (`themes/*.theme` built in, own ones in `~/.config/glimt/themes`), the desktop's dark style and accent |
| `src/thumbs.rs` | Freedesktop thumbnail cache (`~/.cache/thumbnails`) |
| `src/video.rs` | Optional video feature |
| `themes/` | The built-in themes |
| `assets/` | Icons, the desktop and AppStream files, the fonts and their licenses |
| `docs/` | The man page `glimt.1`, and `screenshots/` for README and the website |
| `examples/decoders/` | Decoder plugins: desktop files and their scripts |
| `examples/plugins/` | Plugins (describe a photo with a local model, read the alt text aloud), and `test-model.py`, a stand-in model |
| `flatpak/` | The Flatpak manifest |
| `tools/sbom.py` | Writes `sbom.cdx.json` and `THIRD-PARTY-LICENSES.txt` |
| `tools/release-tests/` | `run.sh` builds this and the last release in Docker; `tests.sh` checks them under Xvfb; `samples.txt` lists real files of each format, downloaded once |
| `tools/screenshots/` | `run.sh` makes `docs/screenshots/` of generated pictures with `shots.sh`, using `tests.sh`'s setup (`SHOTS=1`) |

## How to add things

- **An action:** add a variant to `Action`, its menu label in `Action::label`, a key in
  `SHORTCUTS` if Geeqie has one (put Shift/Ctrl+Shift combinations before the plain key: egui
  ignores an extra Shift when matching), handle it in `App::run`, and add it to a menu with
  `menu_item`/`menu_toggle`.
- **A setting:** one line `name: Type = default` in `config!`; values are read with `FromStr`
  and written with `Display`. Add a checkbox in Preferences, and clamp ranges in
  `Config::parse`. Settings stay in `Config` even when their feature is not built in, so saving
  never drops them.
- **A format:** built in when the `image` crate reads it with no new dependency (its feature in
  `Cargo.toml`, the extension in `files::EXTS`, a test in `decode.rs`). Otherwise a decoder
  plugin in `examples/decoders` (`src/decoders.rs`), with a check in `tests.sh`, not code or a
  dependency in Glimt.
- **An optional feature** (chosen when building, not a plugin): a Cargo feature in `[features]`
  (also add it to `full`), a module or extension list behind `#[cfg(feature = "...")]`, and its
  settings shown in Preferences only when built in. Never break the default build. "Plugin"
  means only the desktop files in Edit → Plugins, and decoder plugins.
- **Background work:** spawn a thread, send the result over a channel stored in `App` as a
  `Receiving<T>` (never a bare `Option<Receiver>`), and take it in `update` with `take`, `next`,
  `latest` or `wait`, which tell once when the thread is gone. egui only takes clipboard and
  other output during a frame, so hand results to the UI thread instead of calling the context
  from a worker.
- **State indexed into the file list** (a `usize` or a set of them that means "this file")
  goes into `List`, never into `App`: a new list is a new `List`, so such state cannot outlive
  the list it was made for. What should follow a file to the new list is carried over by name
  in `replace_files`.
- **A UI text:** in `tr("…")` or `tf!("…", name)`, with its translation in every `src/*.po`
  (see Language).
- **Non-trivial logic** (a parser, a loop with edge cases) gets one small `#[test]` that fails
  if it breaks. Tests must clean up the files they create.
- **A parser of file contents** also gets a `survives_broken_*` test that feeds it
  `crate::broken(sample)`: 2,000 damaged forms of a good input in every run, five million
  every Sunday (`.github/workflows/parsers.yml`, `GLIMT_FUZZ`).
- **Touching the disk or starting a program** goes through one function each, and
  `clippy.toml` (`disallowed-methods`) refuses the raw calls, so `cargo clippy` fails on a
  bypass:
  - read a file whole with `decode::read_limited` / `read_text` (a size cap, no pipe or device);
  - write with `config::replace_file`, `create_file` or `write_atomic` (whole or not at all,
    never through a link), and a file the windows share under `config::file_lock` with
    `read_or_none`;
  - rename with `file_ops::rename_never_over` (never over a file), move or copy with `put_file`;
  - start a program with `external::command` or `stoppable_command`, and wait for it only with
    `run_limited`, `limited`, `output_of` or a `Watchdog` (never `.output()`/`.status()`);
  - make a copy of the shown image from `App::shown_copy` and save it with `save::land`.
  A place that must do otherwise gets `#[allow(clippy::disallowed_methods)]` on the narrowest
  statement or function, with its reason on the same line.
- **A shortcut with a known limit** (work in the UI thread, a whole file rewritten) gets a
  comment `shortcut: <the limit>; <when to do more>`, so the next reader knows it was chosen.

## Before every commit

```sh
cargo fmt
cargo clippy --release -- -D warnings
cargo test --release
cargo clippy --release --all-features -- -D warnings
cargo test --release --all-features
python3 tools/sbom.py        # keeps sbom.cdx.json and THIRD-PARTY-LICENSES.txt in step with Cargo.lock
```

- **A branch per feature or fix**, named after it (`flatpak`, `slideshow-transitions`), merged
  into `main` when a release is made, so that a release gathers several changes.
- **One feature or fix per commit.** Formatting-only changes in a commit of their own.
- **A fix lands with its check in the same commit:** a unit test, or for the window code a
  section in `tools/release-tests/tests.sh` that fails on the release before it. A fix without
  one is not finished; 0.37–0.38 showed that a fix nobody checked is the next release's
  regression.
- Use `--release` for tests too; a separate debug build doubles the space in `target/`.
- A release: the version in `Cargo.toml` and in the header of `docs/glimt.1`, a section in
  `CHANGELOG.md`, a `<release>` line in `assets/io.github.thornbux.Glimt.metainfo.xml`, and a
  tag `vX.Y.Z` on `main` (the release workflow checks each). New command-line options go in the man page too.
- New thumbnails are saved to the shared cache by default (`save_thumbnails`). Test with a
  separate `HOME` and `XDG_CACHE_HOME`, so test runs leave nothing in the real cache.

## Before a release

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

By hand: the Flatpak, with two windows (Ctrl+N), `--remote next` to the focused one,
`--remote-all next` to both and `--remote-all quit`; the AppImage, with a second window (Ctrl+N)
that stays when the first one closes.

The window's logic (the list, the selection, file operations and what follows them) is tested
without a window: `tests::app_on(dir)` in main.rs makes an `App` on `egui::Context::default()`
with settings of its own, listed, in milliseconds. A fix there gets such a test.

A fix in the window code that a unit test cannot reach (painting, keys, clicks) gets a check in `tests.sh`: the steps
with keys, `--remote` and clicks by text (`menu File "Open With"`, through the `test-hooks`
build's `glimt --remote where LABEL`; a widget the tests need gets a `noted(…)` call), and the
result read from the files or from a colour in a screenshot. Screen coordinates only where a
click's place is the point. Run it against the last release as well, to see that it fails there.

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
- `examples/plugins` and `examples/decoders` are installed as examples, not as active plugins:
  in `/usr/share/doc/glimt/examples/` in the `.deb` and `.rpm` (scripts 755, so they work when
  copied), and as `examples/` in the tar.gz. Glimt reads `/usr/share/glimt/{plugins,decoders}`,
  but nothing is put there: each example needs programs the package cannot count on (Ollama,
  spd-say, exiftool, ImageMagick, Python modules), and would sit in the menu for everyone and
  fail. A user copies the ones wanted to `~/.local/share/glimt/{plugins,decoders}` (Edit →
  Plugins → Open the Plugins Folder). Not in the AppImage or Flatpak, as no README is: their
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
release in a job of its own. Dependabot does not move a container's digest: before a release,
take the tag's current one (the command is in `release.yml`), which brings the image's fixes. To build it locally: its runtime, SDK and Rust extension take about
4 GB, so they can live on `/tmp` through `FLATPAK_USER_DIR`:

```sh
export FLATPAK_USER_DIR=/tmp/fp/flatpak
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    --state-dir=/tmp/fp/state /tmp/fp/build flatpak/io.github.thornbux.Glimt.yml
flatpak run io.github.thornbux.Glimt
```

- In the sandbox, external programs go through `command()` in `src/external.rs`, which runs them on
  the host with `flatpak-spawn --host`; use it for every new external program.
- To run it in a virtual X display, add `--nosocket=wayland --nosocket=fallback-x11
  --socket=x11`, and `--filesystem=/tmp/...` for test files: `/tmp` is private in the sandbox.

## Privacy

Screenshots and examples must never show the maintainer's own folders or files.
`tools/screenshots/run.sh` makes them from generated pictures in a virtual X display; after a
change to the look, run it and compare with `git diff`. The website copies `docs/screenshots/`.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
