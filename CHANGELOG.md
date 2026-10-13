# Changelog

All notable changes to Glimt. The format follows [Keep a Changelog](https://keepachangelog.com/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- A JPEG much larger than the window is decoded at 1/2, 1/4 or 1/8 of its size while decoding,
  instead of whole and then scaled down: a 12 MP photo takes 6 MB instead of 44 MB on its way to
  the window and about a third less time, and the peak memory while browsing photos drops from
  about 286 to 180 MB. Zoom 1:1 still shows every pixel.

## [0.13.0] - 2026-10-13

### Added

- Regular expressions in the search field: re: for file names, exif:re: for the EXIF, keywords
  and comments, ignoring case. A pattern that is not one says why under the field.
- A setting for the search field: fuzzy (the default) or a regular expression always, from the
  first letter and in list order, as Geeqie's file filter.
- Folders can be renamed or moved to the trash with all in them: right-click a folder in the
  list or the folder tree. Renaming the open folder, or one above it, follows it.
- A Types menu beside the sort menu, as the file classes of Geeqie's file filter: the list shows
  images, RAW files and videos, or only some of them. A file opened directly is always listed.

### Fixed

- Menus taller than the window scroll, instead of running off it or over the menu bar.
- With the desktop's screen profile, the first images are no longer shown in sRGB and a moment
  later again in the screen's colours: at start they wait for colord, at most 150 ms.

## [0.12.0] - 2026-10-13

### Added

- A screen profile (Preferences → Colour management → Choose…): photos are converted from their
  own ICC profile straight to the screen's, for calibrated and wide-gamut screens. Without one
  nothing changes: the screen is taken to be sRGB.
- View → Colour Management, as in Geeqie: the profile of images without one (sRGB, Adobe RGB,
  Display P3 or a file), and whether an image's own profile is used. A photo whose EXIF says
  Adobe RGB, as from a camera set to it, is shown as Adobe RGB. The EXIF panel says which
  profile an image is shown from.
- The desktop's screen profile, from colord as GNOME and KDE set it, on X and Wayland; it goes
  before a chosen one, and can be turned off under View → Colour Management.
- The rendering intent under View → Colour Management (perceptual, relative colorimetric,
  saturation, absolute colorimetric), for ICC profiles with tables for each.

### Changed

- The Dark theme has a look of its own: deeper neutral greys and its own blue, instead of the
  desktop's accent, so it no longer looks the same as Desktop on a dark desktop.
- The Light theme likewise: soft neutral greys and its own deeper blue, instead of the
  desktop's accent, so it no longer looks the same as Desktop on a light desktop.
- Themes are easier to find: View → Theme lists them, and Theme comes first in Preferences.

### Fixed

- Pan view said "1 images" for a day or a list with one image.

## [0.11.1] - 2026-10-12

### Added

- A Flatpak bundle, `glimt-X.Y.Z-x86_64.flatpak`, in each release.

### Changed

- Find Duplicates by dimensions reads a JPEG's size from the start of the file, not the whole
  file: much faster in large folders and on network shares.
- Find Duplicates by similarity rules out most pairs before comparing them in full, so large
  folders no longer take hours.
- Printing writes the PDF page by page, to the printer or the file, instead of holding it all in
  memory.

### Fixed

- Find Duplicates could move the shown image to the trash, instead of the checked files, when
  the list had changed since they were checked.
- Keywords added to many files in the background could undo a rating or comment saved meanwhile
  in the same sidecar, or the reverse.
- Adding to a collection that could not be read replaced it with only the new images.
- A file in a subfolder or a collection that could not be deleted stayed hidden, and a collection
  was saved without it.
- The pan view showed the old days after the list changed order (another sort, random order),
  and a click could open another image.
- The sort manager lost folders whose names are not UTF-8.
- In the Flatpak, two windows could write the same temporary collection file.

## [0.11.0] - 2026-10-12

### Added

- Layouts (View → Layout): two images side by side, two above each other, or four, also in
  full screen and in the slideshow, which changes them all together or one at a time; the
  file names in every pane or none.
- Themes (Preferences → Theme): Dark (the default), Light, Desktop, Nord, and High Contrast
  and Contrast for low vision. Own themes are `key=value` files in `~/.config/glimt/themes`;
  Save as Theme… starts one from the look shown.

### Changed

- A newer look: the Inter typeface, Phosphor icons, rounder and roomier widgets, and the
  desktop's accent colour.

### Fixed

- Open Collection (O) failed before the first collection was made, as its folder was not there.
- The release build no longer warns that Zig's linker ignores an optimisation setting.

## [0.10.0] - 2026-10-12

### Added

- Collections, as in Geeqie: lists of images from any folders, made with C or File → Add to
  Collection and opened with O or File → Collections, shown in the file list. They are Geeqie
  `.gqv` files, so Geeqie's collections open too.
- Find Duplicates (D), as in Geeqie: groups of the images in the list that are identical, have
  the same name, size or dimensions, or look alike; checked files go to the trash.
- Keywords and a comment for each image, as in Geeqie, in the EXIF panel (Ctrl+K, Alt+K):
  saved to the XMP sidecar `photo.jpg.xmp` that darktable and digiKam read, and found by
  `exif:` searches. Keywords inside the image are shown too, and can be added to many files.
- Printing (Ctrl+P), as in Geeqie: the selected images one per page, to a printer through CUPS
  or into a PDF file, on A4 or Letter, with the file names below if chosen.
- Pan view (Ctrl+J), as Geeqie's timeline: the images of the list as thumbnails under a
  heading for each day, by file or EXIF date; fast also with 300,000 files.
- Plugins, as in Geeqie: desktop files in `~/.local/share/glimt/plugins` run their command on
  the selected files from Edit → Plugins, and the files are shown again afterwards.
- The sort manager (Shift+S), as in Geeqie: a list of folders on the right; a click on one
  copies or moves the selected files there.

### Changed

- Renaming, moving, copying and deleting take a file's XMP sidecar along.
- The release workflow stops on crates with known vulnerabilities (`cargo audit`), and no
  longer uses actions that run on Node.js 20.

## [0.9.4] - 2026-10-12

### Changed

- A PNG or WebP turned and saved keeps its EXIF block (camera, date, place), also a lossy
  WebP, and its orientation is set upright, as the pixels are saved as the view showed them.
  Files with EXIF were left alone before.
- Sorting by EXIF date and the `exif:` search read only the start of a JPEG, where its EXIF
  block is, not the whole file: much faster in large folders and on network shares.
- With Auto Rotate Using EXIF off, new thumbnails are no longer saved to the shared cache,
  where the file manager showed them on their side.
- A message from a background job keeps the selection; only a failed delete or move reads the
  folder again.
- Faster in large folders: "Sort by number", the status bar and slideshow while searching, and
  the total size in the list with subfolders, which no longer holds a second list of the tree.
- The release workflow uses other projects' actions pinned to a commit.

### Fixed

- Two images with the same name and another extension (`a.png`, `a.webp`) saved turned at the
  same time could replace one with the other's bytes.
- A PNG or WebP turned again, or saved again, while it was still being saved could end up
  broken or turned twice; a decoder crash while saving left the busy pointer on.
- Rename Many failing halfway left files under hidden temporary names; the renames done are
  now undone, and the message names the file instead of a temporary name.
- After an image was saved turned, the status bar and the list still showed its old size.
- Moved files stayed in the list with subfolders, and files moved or copied into a folder the
  list shows only appeared with auto refresh on.
- In the Flatpak, a window opened with Ctrl+N took over the first one's remote control, and
  windows could write the same temporary marks, config or thumbnail file.
- Values from ffprobe that are not numbers no longer reach ffmpeg's filter for HEIC photos.

## [0.9.3] - 2026-10-12

### Added

- A Flatpak manifest, `flatpak/io.github.thornbux.Glimt.yml`, to build a Flatpak from the source.

### Changed

- Saving the rotation of a PNG or WebP runs in the background, so a large one no longer freezes
  the window; files with an EXIF block are left alone, as saving them again would lose it.
- "Open With" reads the applications in the background.

### Fixed

- An SVG claiming a huge size, or a huge JPEG XL or RAW preview, could use gigabytes of memory
  and end Glimt; SVGs are drawn at most 8192 pixels wide, the others refused above 512 MB.
- A decoder crashing on a broken file stopped all later images or thumbnails.
- Marks and stars set in two Glimt windows were lost from the one that saved first.
- A wrong password for a network share was sent again at every prompt.
- Messages from background jobs reopened the folder, which ended random order and the list
  with subfolders, and the message itself disappeared before it was seen.
- Renaming a file in a subfolder, in the list with subfolders, moved it to the top folder.

## [0.9.2] - 2026-10-12

### Added

- An `.rpm` package for Fedora and RHEL in each release.

### Fixed

- The `.deb` asked for glibc 2.34 although Glimt needs only 2.28, so it would not install on
  Debian 10 and 11 or Ubuntu 20.04.

## [0.9.1] - 2026-10-12

### Added

- A manual page, `docs/glimt.1`.
- Release packages: a `.deb` and a `.tar.gz` with every feature, for glibc 2.28 and newer.

### Changed

- New thumbnails are saved to the shared cache (`~/.cache/thumbnails`) by default, as file
  managers do; Preferences can turn it off.

## [0.9.0] - 2026-10-12

The first release. Glimt is not 1.0 yet: it is in daily use, but expect rough edges.

### Viewing

- Fast browsing with the images on each side decoded in advance, scaled to the window; full
  resolution only when zoomed in.
- Folders of any size: the file list draws only what is visible, also with 300,000 files.
- Zoom, fit, 1:1, keep zoom between images, full screen, rotate, mirror and flip the view.
- Formats: JPEG, PNG, WebP, GIF and TIFF, animated GIF and WebP, pages of multi-page TIFFs, and
  camera RAW files (CR2, CR3, NEF, ARW, DNG, ORF, RAF, RW2, PEF and more) by their embedded
  preview.
- Optional features: video previews (`video`), HEIC (`heic`), AVIF (`avif`), JPEG XL (`jxl`),
  SVG (`svg`), and Geeqie's marks and stars (`marks`); `full` has all of them.
- Colour management: embedded ICC profiles converted to sRGB.
- Compare two images side by side, at the same zoom.
- Histogram, image information over the image, EXIF panel and a window with every EXIF field;
  the GPS position opens a map.
- Slideshow with random order, repeat, and optionally the images of all subfolders.

### Finding and organising

- File list as rows or a grid of thumbnails, in three sizes, from the shared freedesktop cache.
- Folder list or tree, folder path field, network shares (`smb://`, `sftp://`) mounted with gio.
- Fuzzy search on file names, search in EXIF (`exif:`), and with marks, by mark and stars.
- Sorting by name, number, date, EXIF date, size and extension.
- Copy, move, rename (several files with a pattern), delete to the trash or permanently.
- Save the rotation to the file: JPEG through its EXIF orientation without touching the pixels,
  PNG and WebP without further loss.
- Stars also to XMP sidecars for darktable and digiKam.
- Copy the image or the files to the clipboard, open with another application, drop files on the
  window.

### Control

- Geeqie's keyboard shortcuts, with every difference listed in the help; own shortcuts in
  Preferences.
- Remote control from the command line: `glimt --remote` and `--remote-all`.
- Remembers the folder, window size and position.

[Unreleased]: https://github.com/thornbux/glimt/compare/v0.13.0...HEAD
[0.13.0]: https://github.com/thornbux/glimt/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/thornbux/glimt/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/thornbux/glimt/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/thornbux/glimt/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/thornbux/glimt/compare/v0.9.4...v0.10.0
[0.9.4]: https://github.com/thornbux/glimt/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/thornbux/glimt/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/thornbux/glimt/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/thornbux/glimt/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/thornbux/glimt/releases/tag/v0.9.0
