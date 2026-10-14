# Glimt

[![CI](https://github.com/thornbux/glimt/actions/workflows/ci.yml/badge.svg)](https://github.com/thornbux/glimt/actions/workflows/ci.yml)

A fast, memory-frugal image viewer for Linux, written in Rust with egui and inspired by
[Geeqie](https://www.geeqie.org/). It has most of Geeqie's features, in simpler forms (the
help lists every difference), and focuses on showing images quickly, in folders of any size,
from a few images to several hundred thousand. The name *Glimt* is Norwegian for "glimpse": a
quick look, which is what an image viewer is for.

> **Early days.** Glimt is young and has not reached version 1.0. It is used every day, but
> rough edges and the odd bug are to be expected. Bug reports, ideas and fixes are very
> welcome: open an issue or send a pull request (see
> [CONTRIBUTING.md](CONTRIBUTING.md)).

## Why Glimt

- **It keeps up with you.** Hold down the arrow key and the images follow: the ones on each side
  are decoded in advance, at the size of the window. The file list draws only what is on screen,
  so it scrolls, searches and sorts as smoothly with 500,000 images as with ten.
- **It stays small.** Images are kept at the size they are shown, not at full resolution, and
  memory goes back to the system when you move on. A broken file cannot take gigabytes or bring
  the viewer down.
- **It feels at home on Linux.** Dark by default, or following the desktop's light or dark style
  and accent colour, with themes for low vision. The thumbnail cache is the one the file
  manager uses, network shares open through GVfs, deleted files go to the trash.
- **Geeqie users know it already.** The keyboard shortcuts are Geeqie's, collections are Geeqie
  `.gqv` files, plugins are Geeqie's desktop files, and the help lists every difference.
- **It respects your files.** Ratings, keywords and comments go to XMP sidecars that darktable and
  digiKam read, and follow the file when it is renamed, moved or copied. Turning a JPEG changes
  its orientation tag, not its pixels. Copy and move never overwrite a file.
- **No setup.** One binary that runs on any distribution with glibc 2.28 or newer, packaged as
  `.deb`, `.rpm`, `.tar.gz` and Flatpak. No library to import, no database to build: open a
  folder and look.

## Glimt and Geeqie

Measured on the same machine and the same files, with Glimt 0.13.0 and Geeqie 2.6.1: a
1280×800 window, the file given on the command line, the time until the image is on screen
and the memory (RSS) 3 seconds later, the median of five runs (three for the large folder).
Geeqie decodes in helper processes; its memory is the sum over them, which counts the shared
libraries more than once.

| | Glimt | Geeqie |
|---|---|---|
| First image, 12 MP JPEG photos | **0.46 s** | 0.83 s |
| First image, 20 screenshots (PNG) | **0.38 s** | 0.85 s |
| First image, folder of 500,000 files | **0.47 s** | 27 s |
| Memory, folder of 500,000 files | **159 MB** | 1,165 MB |
| Memory, 12 MP photos | **142 MB** | 213 MB |

What each has. ✓ is there, – is not; Glimt follows Geeqie's keys, so most of it works the
same way in both.

| | Glimt | Geeqie |
|---|---|---|
| Keyboard shortcuts | Geeqie's (differences listed in the help) | ✓ |
| Thumbnails in the file list, folder tree, icon grid | ✓ | ✓ |
| Fuzzy file name search (like fzf) | ✓ | – |
| Regular expression filter | ✓ | ✓ |
| Search EXIF, keywords and comments from the search field | ✓ | Search window |
| Memory use as one setting (Low, Medium, High) | ✓ | Cache size |
| Layouts with two or four images; Compare at the same zoom | ✓ | ✓ (also three) |
| Slideshow: random order, subfolders, one pane at a time | ✓ | ✓ |
| Start options (`--slideshow --layout 2x2 …`) and named windows for remote control | ✓ | Remote control |
| Kiosk mode for a picture screen | ✓ | – |
| Camera RAW | Embedded preview | Embedded preview or LibRaw |
| HEIC, AVIF, JPEG XL, SVG | ✓ | ✓ |
| Video | Still frame; plays in your player | ✓ |
| Colour management (ICC, screen profile from colord) | ✓ | ✓ |
| Ratings, keywords and comments in XMP sidecars | ✓ | ✓ |
| Keyword tree | – | ✓ |
| Find Duplicates, also by similarity | ✓ | ✓ |
| Pan view | Timeline | Timeline, calendar, folders |
| Collections (`.gqv`) | ✓ | ✓ |
| Plugins (desktop files) | ✓ | ✓ |
| Lua scripts, external editors | – | ✓ |
| Several windows in one process | One per process | ✓ |
| Printing, contact sheets | ✓ (CUPS or PDF) | ✓ |
| Install | One binary: `.deb`, `.rpm`, `.tar.gz`, Flatpak | Distribution packages, AppImage |
| Written in | Rust (egui) | C/C++ (GTK 3) |

## Screenshots

The images in the screenshots are generated for the purpose.

![Glimt with thumbnails in the file list](docs/screenshots/main.png)

| | |
|---|---|
| ![Four images at once, with their names](docs/screenshots/layout.png) | ![Find Duplicates by similarity](docs/screenshots/duplicates.png) |
| Layout 2×2 (View → Layout), with the file names shown | Find Duplicates (D) by similarity, also in subfolders |
| ![File size and date columns, and the EXIF panel](docs/screenshots/exif-and-columns.png) | ![Fuzzy file name search](docs/screenshots/fuzzy-search.png) |
| File size and date columns, and the EXIF panel (Ctrl+K) | Fuzzy search: "sun" finds sunflower and sunrise, and also de**s**ert_d**un**es |
| ![Pan view in the Light theme](docs/screenshots/pan.png) | ![Keyboard map](docs/screenshots/keyboard-map.png) |
| Pan view (Ctrl+J), here in the Light theme | Help → Keyboard Map |

## Features

**Viewing**
- Zoom, fit, 1:1, zoom kept between images, full screen, rotate, mirror and flip.
- Layouts with two or four images at once, and Compare (E) side by side at the same zoom.
- Slideshow with random order, repeat, all subfolders, several folders at once
  (`glimt --slideshow ~/Holiday ~/Family`), and every pane of a layout.
- Histogram, information over the image, an EXIF panel and a window with every EXIF field.
- Colour-managed: photos with Adobe RGB or Display P3 profiles are shown correctly.

**Finding and organising**
- Fuzzy search on file names, and search in EXIF, keywords and comments (`exif:canon 2024:05`).
- Sort by name, number, date, EXIF date, size or extension.
- Marks 1–6 and 0–5 stars, keywords and a comment for each image.
- Collections: lists of images from any folders, saved and opened again.
- Find Duplicates: identical files, the same name, size or dimensions, or similar pictures.
- The sort manager: move or copy images to favourite folders with one click.
- Pan view: every image in the folder as a timeline of thumbnails by day.

**Files**
- Copy, move, rename (many at once with a pattern), delete to the trash.
- Save the rotation to the file. Copy images or paths to the clipboard, open them with another
  application, and print them to any CUPS printer or a PDF, one per page or as a contact sheet.
- Plugins: run any command on the selected images from the Edit menu.
- Remote control from scripts and desktop shortcuts: `glimt --remote next`.

## Supported formats

- **Everyday:** JPEG, PNG, WebP, GIF and TIFF, with animated GIF and WebP playing in a loop and
  every page of a multi-page TIFF.
- **Camera RAW:** CR2, CR3, NEF, ARW, DNG, ORF, RAF, RW2, PEF and more, shown at once through
  the JPEG preview the camera stores in them.
- **Modern formats:** HEIC/HEIF from phones, AVIF, JPEG XL and SVG.
- **Video:** a still frame in the list and the viewer; opening it plays it in your video player.
- **Colour:** photos with an ICC profile (Adobe RGB, Display P3 …) are converted to sRGB, so
  they do not look dull, or to your screen's own profile for a calibrated or wide-gamut screen,
  taken from the desktop (colord) when it has one.
  Adobe RGB photos without a profile are found from their EXIF, as in Geeqie.

The release packages have every format built in; HEIC, AVIF and video also need `ffmpeg`. When
building from source, HEIC, AVIF, JPEG XL, SVG and video are optional features (`heic`, `avif`,
`jxl`, `svg`, `video`).

## Installing

Each [release](https://github.com/thornbux/glimt/releases) has a `.deb` for Debian and Ubuntu, an
`.rpm` for Fedora and RHEL, a `.tar.gz` for other distributions, and a `.flatpak`. All have every
optional feature built in; the first three run on systems with glibc 2.28 or newer (RHEL 8,
Debian 10, Ubuntu 20.04 and later).

```sh
sudo apt install ./glimt_0.16.0_amd64.deb     # Debian, Ubuntu: also menu entry, icon, man page
sudo dnf install ./glimt-0.16.0-1.x86_64.rpm  # Fedora, RHEL: the same

tar xzf glimt-0.16.0-x86_64-linux.tar.gz      # elsewhere: the binary and the files to install
./glimt-0.16.0-x86_64-linux/glimt

flatpak install --user ./glimt-0.16.0-x86_64.flatpak  # any distribution with Flatpak
```

For video, HEIC and AVIF, install `ffmpeg` too; `zenity` gives the file dialogs.

## Building

### Requirements

- Linux with glibc. It is tested on x86_64, and aarch64 should work but is untested.
- Rust 1.88 or newer (edition 2024, let chains), installed through [rustup](https://rustup.rs):
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- A C toolchain for linking: `build-essential` (Debian/Ubuntu), `gcc` (Fedora), or `base-devel` (Arch).

No development packages for graphics are needed. OpenGL, Wayland and X11 are loaded at runtime.

### Build and run

```sh
cargo build --release
./target/release/glimt [FILE_OR_FOLDER]
```

The binary is `target/release/glimt`. It is self-contained, so you can copy it anywhere:

```sh
install -Dm755 target/release/glimt ~/.local/bin/glimt
```

### Optional features

Extra file types are optional features, so the default build stays small:

| Feature | Adds | Needs at runtime |
|---|---|---|
| `video` | Video files in the list with a preview frame; Enter plays them | `ffmpeg` (previews), a player or the default application |
| `heic` | HEIC/HEIF photos (iPhone), with EXIF | `ffmpeg` and `ffprobe` 8.0 or newer |
| `avif` | AVIF photos, with EXIF | `ffmpeg` and `ffprobe` with dav1d |
| `jxl` | JPEG XL photos, decoded in Rust (jxl-oxide) | nothing |
| `svg` | SVG images, drawn in Rust (resvg) | nothing |
| `marks` | Geeqie's marks 1–6 and 0–5 stars, shown in the list and searchable | nothing |

```sh
cargo build --release --features video,heic,marks
cargo build --release --features full   # all of them, same as --all-features
```

### Before committing

```sh
cargo fmt
cargo clippy --release --all-features -- -D warnings
cargo test --release --all-features
```

### Portable build (older distributions)

A normal build only runs on systems with the same or a newer glibc as the build machine.
To run on every supported distribution down to glibc 2.28 (RHEL/Rocky/Alma 8, SLES 15,
Ubuntu 22.04, Debian 12), link against an older glibc with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), as the release workflow does:

```sh
pip install ziglang            # or: sudo apt install zig
cargo install cargo-zigbuild
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
```

The binary ends up in `target/x86_64-unknown-linux-gnu/release/glimt`. This command
checks that it needs no glibc newer than 2.28:

```sh
objdump -T target/x86_64-unknown-linux-gnu/release/glimt | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1
```

musl-based distributions (Alpine) are not supported, because the viewer calls glibc's
`malloc_trim` to return decoder memory to the system.

### Flatpak

The `.flatpak` in each release takes its runtime from Flathub when installed.
`flatpak/io.github.thornbux.Glimt.yml` builds one from the checkout, with every feature. The
runtime, SDK and Rust extension take about 4 GB:

```sh
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    build-flatpak flatpak/io.github.thornbux.Glimt.yml
flatpak run io.github.thornbux.Glimt
```

The sandbox is loose: Glimt sees every file, and runs ffmpeg, zenity and gio on the host
through `flatpak-spawn --host`, so those must be installed there. The thumbnail cache is the
host's. Settings live in `~/.var/app/io.github.thornbux.Glimt`. Remote control works between
Flatpak instances: `flatpak run io.github.thornbux.Glimt --remote next`.

## Remote control

As with `geeqie --remote`, a running Glimt takes commands from the command line, for example
from a script or a keyboard shortcut of the desktop. `--remote` goes to the Glimt window used
last, `--remote-all` to every one:

```sh
glimt --remote next          # also prev, first, last, slideshow, random, fullscreen, quit
glimt --remote open ~/Pictures/holiday   # starts Glimt if it is not running
glimt --remote-all quit      # close every Glimt
glimt --help                 # all commands
```

## Desktop integration

To get Glimt with its icon in the application overview and dock (for the current user):

```sh
install -Dm644 assets/glimt.svg ~/.local/share/icons/hicolor/scalable/apps/glimt.svg
install -Dm644 assets/glimt-256.png ~/.local/share/icons/hicolor/256x256/apps/glimt.png
sed "s|^Exec=glimt|Exec=$PWD/target/release/glimt|" assets/glimt.desktop \
  > ~/.local/share/applications/glimt.desktop
update-desktop-database ~/.local/share/applications
```

Leave out the `sed` path rewrite if `glimt` is on your `PATH`.

The manual page (`man glimt`) describes the command line and remote control:

```sh
install -Dm644 docs/glimt.1 ~/.local/share/man/man1/glimt.1
```

## Tested on

| Distribution | Desktop | Session | How |
|---|---|---|---|
| Ubuntu 26.04.1 LTS (glibc 2.43, Mesa) | GNOME 50.1 | Wayland | Daily use |
| Ubuntu 26.04.1 LTS | none (Xvfb) | X11 | Automated checks and the screenshots |

CI builds and tests on GitHub's current Ubuntu. Other distributions with a recent Rust should
work when Glimt is built from source; a binary built on a newer glibc does not start on an
older one (see the portable build above). Reports from other distributions and desktops (KDE,
Sway, X11 with a window manager) are very welcome.

## Runtime dependencies

| Needed for | Tool | Fallback |
|---|---|---|
| Showing images | OpenGL 3 (Mesa), Wayland or X11 | none |
| Open / Copy / Move dialogs | `zenity` | `kdialog` |
| Move to trash | `gio` (glib) | none: Delete reports that gio is missing |
| Open With | `gio open`, `gio mime` and `gio launch` | `xdg-open` (default application only) |
| smb://, sftp:// … in the folder field or on the command line | `gio mount` (GVfs) | none: mount the share yourself |

## Files

| What | Where |
|---|---|
| Preferences | `$XDG_CONFIG_HOME/glimt/config` (`~/.config/glimt/config`) |
| Last folder (when enabled) | `$XDG_STATE_HOME/glimt/state` (`~/.local/state/glimt/state`) |
| Collections | `$XDG_DATA_HOME/glimt/collections` (`~/.local/share/glimt/collections`), as Geeqie `.gqv` files |
| Thumbnails (read; written only with "Save new thumbnails" on) | `$XDG_CACHE_HOME/thumbnails/normal` (`~/.cache/thumbnails/normal`), shared with other programs |

The first two are plain `key=value` text files written atomically.

## SBOM

[`sbom.cdx.json`](sbom.cdx.json) is a CycloneDX 1.5 software bill of materials: every crate the
Linux build uses (all features), with version, license, SHA-256 from `Cargo.lock` and the
dependency graph. External programs Glimt runs (ffmpeg, gio, zenity) are not in it. Regenerate
it after changing dependencies:

```sh
python3 tools/sbom.py
```

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 Hans-Henry Jakobsen.
