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

## Language

English for everything in the repository: UI text, identifiers, comments, commit messages and
documentation.

## Code layout

| File | Contents |
|---|---|
| `src/main.rs` | The app: `App`, its decode and thumbnail workers, the file list, menus, panels and dialogs, `run` for every action |
| `src/actions.rs` | The `Action` enum with its menu labels, the `SHORTCUTS` table, the user's own keys |
| `src/config.rs` | `Config` (the `config!` macro), the window state, XDG paths, `write_atomic` |
| `src/decode.rs` | An image file to pixels at the size shown: scaled JPEG, SVG, animations, the histogram |
| `src/colour.rs` | Colour management: ICC profiles, the screen profile (also from colord), rendering intent |
| `src/files.rs` | Image extensions, reading a folder or tree, sorting (`Sort`, `natural_cmp`) |
| `src/save.rs` | Saving a turned image with its EXIF, also lossy WebP |
| `src/external.rs` | External programs: `command()`, ffmpeg (HEIC, AVIF, video), gio mounting, open with |
| `src/help.rs` | Help Manual, Keyboard Shortcuts, Keyboard Map, About, `DIFFERENCES` |
| `src/collections.rs` | Collections: Geeqie `.gqv` files of images from any folders |
| `src/duplicates.rs` | Find Duplicates: groups by checksum, name, size, dimensions or likeness |
| `src/exif.rs` | Minimal EXIF (TIFF) parser for the panel, overlay and date sorting |
| `src/marks.rs` | Optional marks 1–6 and stars, kept in `$XDG_STATE_HOME/glimt/marks`, and stars in XMP |
| `src/meta.rs` | Keywords and comment in XMP sidecars; sidecars that follow their files |
| `src/pan.rs` | Pan view: the list by day, and its rows |
| `src/plugins.rs` | Plugins: desktop files whose command runs on the selected files |
| `src/print.rs` | Printing: a PDF of the images, sent to `lp` or saved |
| `src/raw.rs` | Camera RAW: the largest embedded JPEG preview |
| `src/remote.rs` | `--remote`, `--remote-all` and `--name`: a socket for each Glimt in `$XDG_RUNTIME_DIR/glimt`, and `COMMANDS` |
| `src/search.rs` | Fuzzy file name search (parallel, byte path for ASCII) |
| `src/theme.rs` | The look: Inter and Phosphor fonts, themes (`themes/*.theme` built in, own ones in `~/.config/glimt/themes`), the desktop's dark style and accent |
| `src/thumbs.rs` | Freedesktop thumbnail cache (`~/.cache/thumbnails`) |
| `src/video.rs` | Optional video feature |
| `tools/sbom.py` | Writes `sbom.cdx.json` |
| `tools/release-tests/` | `run.sh` builds this and the last release in Docker; `tests.sh` checks them under Xvfb |

## How to add things

- **An action:** add a variant to `Action`, its menu label in `Action::label`, a key in
  `SHORTCUTS` if Geeqie has one (put Shift/Ctrl+Shift combinations before the plain key: egui
  ignores an extra Shift when matching), handle it in `App::run`, and add it to a menu with
  `menu_item`/`menu_toggle`.
- **A setting:** one line `name: Type = default` in `config!`; values are read with `FromStr`
  and written with `Display`. Add a checkbox in Preferences, and clamp ranges in
  `Config::parse`. Settings stay in `Config` even when their feature is not built in, so saving
  never drops them.
- **An optional format or plugin:** a Cargo feature in `[features]` (also add it to `full`),
  a module or extension list behind `#[cfg(feature = "...")]`, and its settings shown in
  Preferences only when built in. Never break the default build.
- **Background work:** spawn a thread, send the result over a channel stored in `App`, poll it
  in `update`. egui only takes clipboard and other output during a frame, so hand results to the
  UI thread instead of calling the context from a worker.
- **Non-trivial logic** (a parser, a loop with edge cases) gets one small `#[test]` that fails
  if it breaks. Tests must clean up the files they create.
- **A shortcut with a known limit** (work in the UI thread, a whole file rewritten) gets a
  comment `shortcut: <the limit>; <when to do more>`, so the next reader knows it was chosen.

## Before every commit

```sh
cargo fmt
cargo clippy --release -- -D warnings
cargo test --release
cargo clippy --release --all-features -- -D warnings
cargo test --release --all-features
python3 tools/sbom.py        # keeps sbom.cdx.json in step with Cargo.lock
```

- **A branch per feature or fix**, named after it (`flatpak`, `slideshow-transitions`), merged
  into `main` when a release is made, so that a release gathers several changes.
- **One feature or fix per commit.** Formatting-only changes in a commit of their own.
- Use `--release` for tests too; a separate debug build doubles the space in `target/`.
- A release: the version in `Cargo.toml` and in the header of `docs/glimt.1`, a section in
  `CHANGELOG.md`, and a tag `vX.Y.Z`. New command-line options go in the man page too.
- New thumbnails are saved to the shared cache by default (`save_thumbnails`). Test with a
  separate `HOME` and `XDG_CACHE_HOME`, so test runs leave nothing in the real cache.

## Before a release

Run `tools/release-tests/run.sh` on the release branch. It builds the branch and the last
release with every feature in Docker (`rust:1`) and runs `tools/release-tests/tests.sh` there:
a virtual X display, an ordinary user, a separate `HOME` and XDG folders, generated pictures,
and a script named `zenity` early in `PATH` that answers for the file dialog. Each check prints
PASS or FAIL; timings and memory are printed for both versions, to compare. `QUICK=1` leaves out
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
- **Large folder** (500,000 hard links) and **sorting 3,000 JPEGs by EXIF date:** "first image"
  time and RssAnon of both versions.
- **Known vulnerabilities:** `cargo audit`; the release workflow runs it too and stops on a
  vulnerable crate, while warnings such as an unmaintained crate do not stop it. Between
  releases `.github/workflows/audit.yml` runs it every Monday and when `Cargo.lock` changes,
  and Dependabot (`.github/dependabot.yml`) proposes updates of the crates and pinned actions.

By hand: the Flatpak, with two windows (Ctrl+N), `--remote next` to the focused one,
`--remote-all next` to both and `--remote-all quit`; the AppImage, with a second window (Ctrl+N)
that stays when the first one closes.

A fix in the window code that a unit test cannot reach gets a check in `tests.sh`: the steps
with keys, clicks and `--remote`, and the result read from the files or from a colour in a
screenshot. Run it against the last release as well, to see that it fails there.

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
Flathub's `freedesktop-26.08` image (change the image with `runtime-version`), and adds it to the
release in a job of its own. To build it locally: its runtime, SDK and Rust extension take about
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

Screenshots and examples must never show the maintainer's own folders or files. Use generated
pictures in `demo/` (ignored by git), copied to `/tmp`, in a virtual X display.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
