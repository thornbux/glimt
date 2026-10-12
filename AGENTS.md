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
| `src/raw.rs` | Camera RAW: the largest embedded JPEG preview |
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

## Privacy

Screenshots and examples must never show the maintainer's own folders or files. Use generated
pictures in `demo/` (ignored by git), copied to `/tmp`, in a virtual X display.

## License

MIT, copyright Hans-Henry Jakobsen. New dependencies must have a license compatible with MIT;
check `sbom.cdx.json` after adding one.
