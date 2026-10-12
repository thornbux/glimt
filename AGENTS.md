# AGENTS.md

Guidelines for coding agents (and people) adding code to Glimt, a fast, lightweight image viewer
for Linux in Rust/egui, inspired by Geeqie. Read this before changing anything.

## Goals that decide every change

- **Fast and snappy, always.** Nothing heavy on the UI thread: decoding, folder listing, file
  operations, mounting and external programs run in background threads and report back over
  `mpsc` channels, with a repaint request. Show the busy pointer (`busy` counter) while they run.
- **As fast with 300,000 files in a folder as with 10.** No O(n) work per frame, no system call
  per file in the list (use `DirEntry::file_type`), draw lists with `show_rows`, compare file
  names as bytes. Test large folders with hard links to one image, for example
  `mkdir big && for i in $(seq 300000); do ln a.jpg big/$i.jpg; done` on tmpfs.
- **Little memory.** Decode to screen size; full resolution only when zoomed. Preloading has a
  fixed budget (N images on each side). Measure RssAnon before and after changes that touch
  decoding or caches.
- **Simplest thing that works.** No dependency, abstraction, config option or file before it is
  needed. Prefer the standard library, then crates already in `Cargo.toml`, then a few lines of
  code. Heavy formats go through external programs (ffmpeg) rather than linking codecs.

## Behaviour

- Shortcuts and behaviour follow **Geeqie's defaults** (see its keyboard reference). Every
  deliberate deviation goes in the `DIFFERENCES` table in `src/main.rs`, which the Help Manual
  (F1) shows. Remove a row when Glimt starts behaving like Geeqie.
- New user-visible features are described in `src/help.rs` (manual, shortcuts, keyboard map)
  and, where relevant, in `README.md`.

## Language

English for everything in the repository: UI text, identifiers, comments, commit messages and
documentation.

## Code layout

| File | Contents |
|---|---|
| `src/main.rs` | The app: `Action` enum and `SHORTCUTS` table, `Config` (the `config!` macro), decode workers, file list, menus, dialogs, `DIFFERENCES` |
| `src/help.rs` | Help Manual, Keyboard Shortcuts, Keyboard Map, About |
| `src/exif.rs` | Minimal EXIF (TIFF) parser for the panel, overlay and date sorting |
| `src/print.rs` | Printing: a PDF of the images, sent to `lp` or saved |
| `src/raw.rs` | Camera RAW: the largest embedded JPEG preview |
| `src/meta.rs` | Keywords and comment in XMP sidecars; sidecars that follow their files |
| `src/search.rs` | Fuzzy file name search (parallel, byte path for ASCII) |
| `src/thumbs.rs` | Freedesktop thumbnail cache (`~/.cache/thumbnails`) |
| `src/video.rs` | Optional video feature |
| `tools/sbom.py` | Writes `sbom.cdx.json` |

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

Test the release branch by hand, built with `--features full`, with a separate `HOME`,
`XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` under `/tmp`, generated pictures and a virtual X display
(`Xvfb :98`). A script named `zenity` early in `PATH` that prints a folder stands in for the file
dialog. Compare timings with the last release, built the same way.

- **Saving turned:** a PNG, a lossless WebP and a lossy WebP with EXIF orientation 6, turned with
  `]` and saved (Edit → Orientation), with Auto Rotate on and off. `exiftool` must show the EXIF
  kept and orientation 1, and the pixels must be as the view showed them.
- **Saves at once:** `a.png` and `a.webp` of 6000×4000, both turned straight after each other
  with saving on image change. Both must be turned and whole.
- **Subfolders:** with Include Subfolders on and auto refresh off, copy and move files to the
  folder and to a subfolder; the list must follow.
- **Rename Many failing halfway:** names that swap, and one new name too long (over 255 bytes).
  The files must keep their names, with no `.glimt-rename-*` left. A read-only file is no test,
  as Linux renames it.
- **Sorting by EXIF date:** 3,000 JPEGs of about 1 MB with different dates; compare the
  "first image" time on stderr with the last release.
- **Thumbnails:** with Auto Rotate off, nothing new in `$XDG_CACHE_HOME/thumbnails`.
- **Flatpak:** two windows (Ctrl+N), `--remote next` to the focused one, `--remote-all next` to
  both, `--remote-all quit`.
- **Known vulnerabilities:** `cargo audit` (`cargo install cargo-audit`). The release workflow
  runs it too and stops on a vulnerable crate; warnings such as an unmaintained crate are shown
  but do not stop it.
- **Large folder:** 500,000 hard links to one JPEG; the "first image" time and RssAnon. Put them
  in `/dev/shm`: `/tmp` allows about a million inodes, and every link takes one.

## Packages

A tag `vX.Y.Z` pushed to GitHub runs `.github/workflows/release.yml`, which builds against
glibc 2.28 with cargo-zigbuild and publishes a `.tar.gz`, a `.deb` and an `.rpm`. Both package
formats are described in `Cargo.toml` (`[package.metadata.deb]`, `[package.metadata.generate-rpm]`);
new installed files go in both lists and in the tar.gz step. To build and check them locally,
after `cargo build --release --features full`:

```sh
cargo deb --no-build -o /tmp/pkg/          # cargo install cargo-deb
dpkg-deb -I /tmp/pkg/*.deb; dpkg-deb -c /tmp/pkg/*.deb
cargo generate-rpm -o /tmp/pkg/            # cargo install cargo-generate-rpm
```

- Dependencies are written by hand (glibc 2.28): `dpkg-shlibdeps` rounds glibc up to 2.34, and
  cargo-generate-rpm's own list has `[WEAK]` symbol versions that dnf cannot resolve.
- A local build links against the build machine's glibc; only the release binary runs on 2.28.
- Test an `.rpm` without rpm tools in a container, with the storage on `/tmp`:
  `podman --root /tmp/pod --runroot /tmp/podrun --storage-driver vfs run --rm -v /tmp/pkg:/pkg:Z
  registry.fedoraproject.org/fedora:latest bash -c 'dnf -y install /pkg/*.rpm && glimt --help'`
  (also `docker.io/library/almalinux:8` for glibc 2.28). Remove the storage afterwards with
  `podman unshare rm -rf /tmp/pod /tmp/podrun`.

The Flatpak (`flatpak/io.github.thornbux.Glimt.yml`) is built by hand; its runtime, SDK and Rust
extension take about 4 GB, so they can live on `/tmp` through `FLATPAK_USER_DIR`:

```sh
export FLATPAK_USER_DIR=/tmp/fp/flatpak
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    --state-dir=/tmp/fp/state /tmp/fp/build flatpak/io.github.thornbux.Glimt.yml
flatpak run io.github.thornbux.Glimt
```

- In the sandbox, external programs go through `command()` in `src/main.rs`, which runs them on
  the host with `flatpak-spawn --host`; use it for every new external program.
- To run it in a virtual X display, add `--nosocket=wayland --nosocket=fallback-x11
  --socket=x11`, and `--filesystem=/tmp/...` for test files: `/tmp` is private in the sandbox.

## Privacy

Screenshots and examples must never show the maintainer's own folders or files. Use generated
pictures in `demo/` (ignored by git), copied to `/tmp`, in a virtual X display.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
