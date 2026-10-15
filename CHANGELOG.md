# Changelog

All notable changes to Glimt. The format follows [Keep a Changelog](https://keepachangelog.com/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Showing several folders together (`glimt /a /b`) emptied the folder tree until a refresh.
- Find Duplicates read each listed file's size on every frame, slow on a network share.
- One edited keyword or comment threw away the index of the `exif:` search: in a large folder the
  next search read every file again. Only the files changed are read again now.

## [0.21.0] - 2026-10-15

### Added

- / puts the cursor in the file name search (Go → Search File Names), and shows the field if it
  was hidden.

### Changed

- The `.deb` and `.rpm` recommend `sqlite3`, which Import Lightroom Catalog needs.
- New screenshots in the README, made by `tools/screenshots/run.sh` from generated pictures.

### Fixed

- Counts of one are singular: "1 image", "1 collection", "1 page sent", "1 image has other
  stars", where Glimt said "1 images" in the status bar, dialogs and messages.
- In the Flatpak, Import Lightroom Catalog without sqlite3 on the host said the catalog could
  not be read; it now says that sqlite3 is missing.

## [0.20.0] - 2026-10-15

### Added

- File → Import Lightroom Catalog… reads a Lightroom Classic catalog (`.lrcat`) through the
  `sqlite3` program: its collections become Glimt's collections, its keywords go into the
  keyword tree and its stars into the marks, with each of its folders where it is now. Before
  it does, it says which collections are there already and how many images have other stars,
  and asks whose stars to keep. Tested with a catalog made with Lightroom's tables, not yet
  with one from Lightroom itself.
- File → Rename Collection… and Move Collection to Trash, for the open collection. Its images
  stay where they are, and a collection is never renamed over another.

### Changed

- Every keyword is also written as a keyword tree path (lr:hierarchicalSubject), as Lightroom
  writes them: Immich reads only those, so keywords typed in the EXIF panel showed there as no
  tags. A keyword taken away takes its paths along.
- Keywords that differ only in letter case ("Oslo", "oslo") are one keyword, on an image and in
  the keyword tree; the first spelling stays. A sidecar that has one twice shows it once.

### Fixed

- With two keyword tree paths ending in the same keyword (Places|Norway|Oslo and Events|Oslo),
  turning one off took the keyword off the image although the other path was still on, and
  both boxes were ticked when only one was set.

## [0.19.0] - 2026-10-15

### Added

- The image info in full screen (I) follows a template in Preferences, as Geeqie's overlay:
  fields such as %name%, %keywords% and %Camera% (each EXIF panel label), `|` between parts
  that are left out when their fields are empty, and `\n` for a new line.

### Security

- Every action in the workflows that build and sign the releases is pinned to a commit;
  checkout and the artifact actions were pinned to a tag, which can be moved.

## [0.18.0] - 2026-10-15

### Added

- A keyword tree in the EXIF panel (Ctrl+K), as Geeqie's: keywords in levels, typed as
  Places|Norway|Oslo, each with a box that shows whether the image has it and sets it on the
  selected images, or takes it off. The level path goes into the sidecar as
  lr:hierarchicalSubject, which darktable, digiKam and Lightroom read.

### Fixed

- The Help Manual's differences from Geeqie no longer say that printing puts one image on
  each page: contact sheets of 2 to 20 came in 0.17.0.

## [0.17.5] - 2026-10-15

### Security

- ffmpeg and ffprobe open only local files of the formats Glimt lists. A file named like a
  video but holding a playlist (concat, HLS) was followed to the files or addresses it named,
  and its preview, made as soon as the list showed it, showed what it pointed to.
- Each release has SHA256SUMS and a build provenance signed by GitHub for every file, so a
  download can be checked (see Installing in the README).
- The crates are checked for known vulnerabilities every week, and Dependabot proposes updates
  of the crates and of the pinned actions.

### Changed

- Every parser of file contents is tested with thousands of damaged inputs in each test run,
  and five million every week.

## [0.17.4] - 2026-10-14

### Fixed

- A turn not saved yet went into the wrong file when Rename Many gave the shown image's old
  name to another file, and was lost when the open folder was renamed.
- Deleting, moving or renaming a PNG or WebP while its turn was being saved brought it back
  under its old name; these wait until the save is done now.
- Two windows saving the same image's turn at once could leave it corrupt: each has a
  temporary file of its own now.
- Copy and move never replace a file another window put at the destination a moment before.
- Collections keep the entries of files on a drive that is not plugged in, are not written
  over when they cannot be read, keep an entry whose move was refused, are not undone by a
  remove while being read, and Rename Many in a large collection no longer freezes the
  window (nor drops the entries when it fails).
- Find Duplicates leaves symbolic links out, so the real file is not trashed for its link.
- Keywords and a comment stored in the image itself can be cleared.
- A sidecar or the marks file that cannot be read is not written over, and keywords and
  ratings are declared where they are written in sidecars laid out as exiftool does.
- Turning a JPEG whose EXIF block is cut short no longer writes past it, and an animated GIF
  named .png or .webp is not saved as its first frame.
- Settings that a newer Glimt wrote stay when an older one saves, and a theme with a text size
  of NaN is left out.
- Comparing with a large selection no longer copies it every frame, and the colour
  conversions kept for different ICC profiles no longer grow without end.
- Sorting RAW (RAF, CR3 …) and HEIC files by EXIF date reads the first MB of each, not the
  whole file.
- A second Glimt started with the same --name at the same moment can no longer take the first
  one's socket.
- The Flatpak has the license texts, as the other packages have.

### Changed

- The release stops when the man page is not at the tag's version too, and every release test
  that expects nothing to change has one beside it where something must.
- The README has Getting started, Troubleshooting and Privacy, and SECURITY.md says how to
  report a vulnerability.
- Clearer code: tests beside the code they test, and run(), new(), Preferences and the image
  area split by job. Nothing changes for the user.

## [0.17.3] - 2026-10-14

### Fixed

- Searching in a large folder with thumbnails on no longer slows down and gathers memory:
  thumbnails are kept by the rows shown, not by the files between the first and last match.
- Pan view shows thumbnails for every day, also those far from what the file list shows,
  and no longer keeps a core busy asking for them.
- Add to Collection and moving files in a collection are quick with hundreds of thousands
  of files; before, Select All in a large folder froze the window.
- A collection keeps the images another window (or Add to Collection) put in it while it was
  open here, and a move that cannot happen (a file of that name there) keeps its entry.
- Files deleted or moved while the folder was being read no longer come back in the list.
- Two windows no longer undo each other's settings: each writes only what it changed.
- Auto Rotate on or off turns the thumbnails in the list too.
- Two windows changing the keywords of one image no longer write the same temporary file.
- The Flatpak's window has its own app ID, so Wayland shows Glimt's icon for it.
- "Open with" offers Glimt for HEIC, AVIF, JPEG XL, SVG and RAW files, and several selected
  files open in one window, at the first of them.
- The man page shows Ctrl+P, Shift+S and Ctrl+J as entries of their own.
- The .deb has Glimt's license text, and both packages name the OpenGL and xkbcommon
  libraries the window needs.

### Changed

- The release stops when the tag's version is not the one in Cargo.toml or has no section
  here, and the release tests fail when Glimt does not open a window.
- Clearer code for people reading it: names that say what they hold, App's fields and
  methods in groups by feature, one way to replace a file safely, shortcut notes that say
  when to do more. Nothing changes for the user.

## [0.17.2] - 2026-10-14

### Added

- An AppImage in each release, `glimt-X.Y.Z-x86_64.AppImage`: one file that runs on systems
  with glibc 2.28 or newer, as the tar.gz does. New Window starts the AppImage again.

### Fixed

- A file that claims a huge size (a broken or crafted GIF, WebP or progressive JPEG) no longer
  closes Glimt by asking for gigabytes: it is refused with "image too large". The limit, 512 MB
  decoded, which PNG and TIFF had already, now holds for every format: a WebP with alpha larger
  than about 11,500×11,500, or a progressive JPEG over about 180 megapixels, is refused too.
- Turning an animated PNG is refused, as for animated WebP; before, it was saved as its first
  frame and lost the animation.
- A second Glimt started with the same `--name` no longer takes over the first one's remote
  socket, which left the first one unreachable once the second closed. It says so and takes
  no commands.

### Security

- The release workflow pins the actions from others to a commit, and only the jobs that
  publish get a token that can write releases.

## [0.17.1] - 2026-10-14

### Fixed

- With Save orientation on, a turn could be saved into another file: one in the new folder
  when another folder opened, or a neighbour when the list changed by a delete, a refresh or
  random order in a slideshow; deleting the last image of the list could close Glimt. The
  turn is now saved before the folder or the list changes.
- Rename, Rename Many and a delete confirmation act on the files they asked about: a
  slideshow step, `--remote next` or an auto refresh while the dialog was open made them
  rename or trash other files. Now the slideshow waits, and the answer is refused with a
  notice if the list or the shown image changed.
- Saving a turn through a symbolic link turns the file it points to; before, the link was
  replaced by a turned copy and the file stayed as it was.
- Turning a JPEG whose orientation field holds several values (a broken or crafted file) is
  refused; before, two bytes of the picture could be overwritten.
- A JPEG with an empty APP1 segment no longer stops the EXIF panel for the rest of the
  session.

### Changed

- The code is split into modules (colour, config, decode, files, save, external programs,
  actions) and the window's frame into a method per part; nothing changes for the user.

## [0.17.0] - 2026-10-14

### Added

- The slideshow fades from one image to the next, in every pane of a layout: Short (0.4 s,
  the default), Long (1 s) or Off in Preferences, or --fade off|short|long. Browsing by hand
  still shows the next image at once, and CPU use Power saving turns fading off.
- A film strip in full screen: with the pointer at the bottom of the screen, thumbnails of
  the images before and after the shown one, which is outlined; a click shows that image.
  Not in kiosk mode.
- Printing several images on a page, as Geeqie's contact sheet: Images per page 1, 2, 4, 6,
  9, 12 or 20 in the Print dialog, in a grid with each file name below (cut to fit). The
  images are scaled to their cell, so the PDF stays small.

### Changed

- The exif: search index and pan view read the files on two threads at most when Glimt uses
  one malloc arena (the default): more threads waited for each other in the allocator, and
  2,000 files took 551 ms on four threads against 128 ms on two. Find Duplicates, which
  spends its time decoding, still uses every core.

## [0.16.0] - 2026-10-14

### Added

- glimt --kiosk for a picture screen: full screen with only the image; browsing, zoom,
  layouts and the slideshow work, but no menus or panels, nothing that changes files, and
  Escape, Ctrl+Q and closing the window are refused. glimt --remote quit ends it. The man
  page says how to run it as a kiosk session (cage, GNOME Kiosk).
- CPU use in Preferences, below Memory use: Power saving (thumbnails one at a time;
  searches, the EXIF index, Find Duplicates and pan view on two cores), Balanced (the
  default, as before) or Full (thumbnails on every core too). It takes effect at once.
- Several folders as one list, for a slideshow across folders that are not inside each
  other: glimt --slideshow ~/Holiday ~/Family, File → Open Several Folders…, or Show Together
  with These Images in a folder's right-click menu. Names show as folder/file, random order
  mixes all of them, and Include Subfolders takes in the folders below each.

### Changed

- With several panes (a layout, or Compare), the status bar tells about the pane under the
  pointer, which gets a thin outline, else the first: its size, bytes, place in the list and
  its own zoom, with a small map of the panes beside it that shows which pane it is.

## [0.15.0] - 2026-10-13

### Added

- View → Show Hidden Files lists files and folders whose names start with a dot, as Geeqie's
  setting; off by default. Hidden images were listed before, hidden folders not.
- Layouts: L changes to the next layout, U to two above each other and Y back to one image, as
  in Geeqie. A click on a pane makes its image the shown one, and zoom and moving a zoomed
  image apply to every pane, at full resolution when zoomed in.
- The mouse wheel and dragging work over every pane, and over the compared half (E); Alt+arrow
  makes the image in the pane beside, above or below the shown one, as Geeqie's Alt+arrows
  move between panes.
- Case beside the search field when it is a regular expression always (Preferences), as the
  checkbox of Geeqie's file filter: upper and lower case told apart.
- Start options before the file or folder: --slideshow, --random, --fullscreen and
  --layout 1|2x1|1x2|2x2, for example glimt --fullscreen --layout 2x2 --random --slideshow
  ~/Pictures. Random order and the layout are kept, as the View menu keeps them.
- glimt --version (or -V) prints the version.
- Named instances: glimt --name left starts a Glimt called left (shown in its title), and
  glimt --name left --remote slideshow reaches only that one.
- random as a remote command: glimt --remote random turns random order on or off.

## [0.14.0] - 2026-10-13

### Added

- Memory use in Preferences: Low, Medium (the default) or High set preloading, the images decoded
  at once, the limit for animations and the malloc arena together; Details shows them, and
  other values are Custom. Low takes about 40 % less memory at its peak (173 against 286 MB for
  a 12 MP photo); Medium or High keep browsing quick on slow storage such as SMB or NFS.

### Changed

- A JPEG much larger than the window is decoded at 1/2, 1/4 or 1/8 of its size while decoding,
  instead of whole and then scaled down: a 12 MP photo takes 6 MB instead of 44 MB on its way to
  the window and about a third less time, and the peak memory while browsing photos drops from
  about 286 to 180 MB. Zoom 1:1 still shows every pixel.
- Thumbnails of JPEG files are made the same way, at 1/8: under 1 MB each instead of 38 MB for a
  12 MP photo, and a third faster.
- The JPEG preview in a RAW file is decoded the same way, for the window and for thumbnails: the
  peak memory while browsing RAW files drops from about 300 to 180 MB.

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

[Unreleased]: https://github.com/thornbux/glimt/compare/v0.21.0...HEAD
[0.21.0]: https://github.com/thornbux/glimt/compare/v0.20.0...v0.21.0
[0.20.0]: https://github.com/thornbux/glimt/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/thornbux/glimt/compare/v0.18.0...v0.19.0
[0.18.0]: https://github.com/thornbux/glimt/compare/v0.17.5...v0.18.0
[0.17.5]: https://github.com/thornbux/glimt/compare/v0.17.4...v0.17.5
[0.17.4]: https://github.com/thornbux/glimt/compare/v0.17.3...v0.17.4
[0.17.3]: https://github.com/thornbux/glimt/compare/v0.17.2...v0.17.3
[0.17.2]: https://github.com/thornbux/glimt/compare/v0.17.1...v0.17.2
[0.17.1]: https://github.com/thornbux/glimt/compare/v0.17.0...v0.17.1
[0.17.0]: https://github.com/thornbux/glimt/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/thornbux/glimt/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/thornbux/glimt/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/thornbux/glimt/compare/v0.13.0...v0.14.0
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
