# Changelog

All notable changes to Glimt. The format follows [Keep a Changelog](https://keepachangelog.com/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- A settings file Glimt cannot read, such as one with a byte that is not UTF-8 or the wrong
  permissions, is no longer replaced by the defaults at the first change in a menu: Glimt says it
  cannot save, and reads the rest of a file with a stray byte.
- Marks and stars follow a folder that is renamed, and go with one moved to the trash, instead of
  staying under the old paths, where a new folder of the same name got them.
- Marks and stars follow their files through a Rename Many that swaps or shifts names, instead of
  being lost or given to the wrong file.
- A Rename Many cut short in the middle of one rename, with the file under both names, is put
  right at the next start instead of being asked about at every start.
- Two windows that both found a Rename Many cut short no longer both put it right: the second one,
  which would rename the files again, does nothing.
- Rename Many cut short by a crash is put right by what was done, which the plan now counts: a
  swap of names finished at the next start is no longer done a second time, which undid it, and
  putting the old names back after a crash before the first rename changes nothing.

## [0.34.1] - 2026-10-17

### Changed

- `examples/test-model.py` is in `examples/plugins`, beside the plugin it stands in a model for.
- The marks file's lock is `.marks.lock`, as Glimt's other files' are: a 0.34.0 and a 0.34.1
  window open at once do not wait for each other while writing marks.
- The code is easier to find one's way in: Preferences, file operations and the windows of Find
  Duplicates, pan view, Lightroom import and printing are in their features' modules, and names
  and comments say what things hold. Nothing changes in how Glimt works.

## [0.34.0] - 2026-10-17

### Added

- An AppStream metainfo file in every package, so software centres show Glimt with a description
  and screenshots.
- THIRD-PARTY-LICENSES.txt: the license texts of the crates built into Glimt, in every package
  (deb, rpm, tar.gz, AppImage and Flatpak), as their licenses ask; `tools/sbom.py` writes it.

### Changed

- With many files selected, the Rename Many, New Collection and delete dialogs no longer list the
  whole selection in every frame; Back keeps the last 200 places; Preferences saves when a slider
  is let go, not in every frame it is dragged.
- CI builds Glimt with Rust 1.95 too, the oldest that `rust-version` promises.
- The release workflow runs the tests on the tagged code before it builds the packages.

### Fixed

- A delete or move of hundreds of thousands of files no longer freezes the window when it ends:
  which files were gone was looked up twice for each, in the window's thread.
- Import Lightroom Catalog writes stars and keywords to the sidecars through the same writer as
  stars and keywords set by hand, in the order they were made: a star given during a long import
  could be overwritten by Lightroom's.
- When a Rename Many ends while another image is shown than when it began, the list is read again:
  the shown entry took the renamed image's new name, and with Save Orientation on its turn was
  written into the renamed file.
- Closing Glimt also waits for a Rename Many and for a turned PNG or WebP being saved; closing a
  second time stops the file operations after the file at hand, so no half copy is left behind
  under a hidden name, and only a third close ends at once.
- A write of a sidecar or of Glimt's own files that panics no longer ends its writer thread: every
  later write would have been dropped without a word, and the busy pointer stayed.
- Marks and stars changed just before Glimt closes, or moved with files whose move it waited for,
  are written before it ends: their writer was not waited for.
- A backup into one of the folders it copies (glimt --back-up ~/.local/share/glimt) is refused: it
  copied itself into itself until the paths were too long.
- The keyword tree keeps what other Glimt windows added or removed: each window wrote its own copy
  over the file, so one's new keyword went with the other's next change.
- New Window keeps --lock-settings: the new window was not locked, so Preferences opened there and
  its changes were saved.
- A kiosk (--kiosk) saves no settings: the keys it allows (the slideshow's speed and order, the
  overlay, the histogram, the layout) changed the settings file, and lasted after a restart.
- `--remote` with a command Glimt does not know, such as `nxet`, stops with an error instead of
  sending it; a window that does not answer in time gives exit status 1, and `--name` takes at
  most 64 characters, so its socket path fits.
- glimt --remote select of a linked image selects the link, as the list shows it, not the file it
  points to, which Delete would then have removed; only the links in its folders are followed.
- A --remote select waits only while Delete or Rename asks, as the notes said: it waited for any
  dialog, Preferences too, so a script got no answer and the selection changed later.
- A slideshow's fade draws the image before at the size it was shown, in Fit Width, Fit Height,
  Fill or zoomed, instead of jumping to fit the window as it goes.
- A slideshow in several panes stops at the end of the list when Repeat is off, and shuffles again
  for a new round, also when the number of images is not a multiple of the panes.
- A projector window smaller than the view, or one whose edge is dragged, no longer has every
  image decoded again; only one that grows past the view's size does.
- With Fit Width or Fit Height, the images read ahead are decoded as large as they will be shown,
  not to the shape of the image shown now, so one of another shape is not blurred and decoded
  again when it comes.
- Save This Frame on the first frame of a GIF too large to animate reads it as a still image
  instead of failing.
- Pan view: a file that crashes its date reader no longer gives the files read after it in the
  same thread other files' dates.
- A PNG or WebP saved turned sets the orientation in its XMP upright too, as in its EXIF, so
  programs that read the XMP do not turn it again.
- A JPEG with fill bytes (0xff) before a marker has its EXIF read from the head of the file and
  its orientation saved, instead of being taken for one without segments.
- A lossless WebP with transparency, turned and saved with its EXIF, keeps its transparency in
  other viewers: the extended header now says it has alpha.
- Documentation: when `--check-config` and `--back-up` exit with 1, Stop for the trash (after the
  100 at hand), the JPEG 2000 example with `-protocol_whitelist file` as in examples/decoders, and
  the Flatpak's settings file.
- sbom.cdx.json's serial number comes from the crates' names and versions, so the file is the same
  on every machine.
- README: install one decoder of each pair (FITS, DICOM) instead of every example, as the first by
  name takes the files even without the modules it needs.

## [0.33.0] - 2026-10-17

### Changed

- CI builds with --locked and checks that sbom.cdx.json matches Cargo.lock; the release workflow
  takes the tag through the environment and only as vX.Y.Z, and runs the Flatpak build in an image
  pinned by its digest; the weekly parser run takes other broken inputs each week; Dependabot no
  longer proposes versions of png, tiff, winit and arboard that image and eframe do not use.

### Fixed

- Rename Many in a collection keeps the renamed images in it when the list changed while it ran
  (an image taken out meanwhile): they were looked for by their old names on disk, and the
  collection was saved empty.
- Marks and stars set in another Glimt window are read again when a window gets the focus: each
  window knew only what was there when it started, so marking by mark (Select by Mark, mark:1)
  missed them and pressing a mark's key set it again instead of taking it off.
- A camera RAW file whose sensor data holds a JPEG start and frame marker by chance shows its real
  preview: the chance one claimed a far larger size and was taken, failed, and left a failed-
  thumbnail mark in the shared cache.
- Copy Image to Clipboard, printing and Find Duplicates by likeness survive a decoder that panics
  on a broken file, as showing it does: the busy pointer stayed for good, a PDF was left half
  written, or a share of the files went uncompared.
- Stars written to XMP sidecars go on past a file whose sidecar cannot be written, and are written
  in the background with the keywords, in one order: they stopped at the first failure, and a
  thousand files on a network share held the window.
- Holding Shift+↓ or Shift+Page Down in a selection of hundreds of thousands no longer stutters:
  the size of the selection is worked out once it has stayed the same a moment, not in every step.
- Thumbnails of HEIC and AVIF photos and of videos are made by ffmpeg at thumbnail size: it handed
  over the whole picture first, about 300 MB for a 48 MP phone photo, in each thumbnail thread.
- Printing a contact sheet says how many images went on how many pages (80 images on 4 pages), not
  80 pages.
- With Include Subfolders on, a folder is read once, not twice (500,000 files took both readings);
  a folder given to Open Several Folders inside another one given has its images listed once, not
  twice.
- Settings are merged and written in the background, as every toggle in a menu saves them; a lock
  beside the settings file and beside each collection keeps two windows saving at once from losing
  one's change; Glimt waits for the last write before it ends.
- A thumbnail's source in the shared cache is read with the same URI decoding as lists (a % before
  two hex digits only); the thumbnail and WebP readers get tests with thousands of broken files,
  as every parser of file contents has.
- An image whose own profile is sRGB (as most exports from Lightroom and Photoshop are) is no
  longer run through a colour transform to sRGB, which changed nothing and held a second copy of
  the image while zoomed.
- The EXIF panel reads a photo's EXIF block, its sidecar and its own XMP once each, not six times
  in all: slow on a network share.
- Import Lightroom Catalog leaves out an image whose path in the catalog leads out of its root
  folder (with .. or as an absolute path), which would have put a sidecar outside the folder
  chosen.

## [0.32.0] - 2026-10-17

### Changed

- The decoder examples no longer take .ics and .ids (Bio-Formats; .ics is also a calendar
  invitation, and each started Java for nothing), .dic (DICOM; also a spelling dictionary) or .vsi
  (whole slides too large to read); their files say how to add them back.

### Security

- glimt --remote refuses a command with a line break, as in a file name given to open: the window
  read what followed it as a command of its own, so a downloaded file named "x⏎quit.jpg" opened
  from a script closed Glimt.

### Fixed

- A rectangle being dragged in the grid, or images of a collection being dragged, when the list
  changed under them (auto refresh, a delete finishing) held places of the old list: the selection
  then took other files, which Delete would have acted on.
- Check Images no longer reports phone motion photos (a video after the JPEG) as cut off: it
  looked for the JPEG's end only in the last 64 KB, which a video of megabytes after it may lack.
- A --remote select that comes while Delete or Rename asks waits until the question is answered:
  the answer acted on the files the script selected meanwhile, so Enter on "Permanently delete
  a.jpg?" could delete every file the script named.
- A second Rename Many while one is running is refused: it wrote over the first one's plan for a
  crash, and the first one's renamed files were not followed in the list or the collection.
- A backup to a folder that is not there (a typo, a disk not mounted) is refused instead of made,
  one with nothing to copy says so instead of "0 files", a file that cannot be copied is named,
  and the plan of a Rename Many running meanwhile is left out, which restored later would rename
  files again. The README says where the Flatpak keeps the folders to restore.
- After Tab, a click on the button or menu that had the keyboard focus took the ring away but left
  the focus there, so every shortcut stopped working: a click now puts the focus away.
- The projector shows the frame of an animation stepped to with Ctrl+Page Down, as the main window
  does: it went on playing.
- After a new sort order or a refresh moved the shown file in the list while every decoding thread
  was busy, its image could stay undecoded, with the image before it on screen under its name.
- A projector larger than the main window (full screen on a big screen) shows the image sharp: it
  was decoded for the main window only, and stretched.
- Fit Width, Fit Height and Fill the View decode an image to the size they show it at, not at full
  resolution: a 6000×4000 photo took 96 MB instead of 10, and was slower to show.
- A rectangle dragged in the grid after Ctrl+A in a large folder no longer copies the whole
  selection in every frame: only when it touches other cells.
- A rectangle dragged past the top or bottom of the grid of icons keeps selecting as the grid
  scrolls: it moved with the screen instead of the images, and the rows scrolled out of view were
  dropped from the selection.
- glimt --remote select finds files given through a linked folder (as ~/Pictures may be): the
  window lists the folder with its links followed, so none matched and all were printed as not in
  the list.
- A setting the running Glimt does not know (written by a newer one beside it, as a Flatpak next
  to a .deb) no longer gives a notice at every start; glimt --check-config still lists it.
- Marks and stars that cannot be saved (a full disk, a folder that cannot be written) are told in
  the window, as the settings are: they were lost without a word.
- The NIfTI decoder example reads only the first volume of a series: in FSL's own orientation
  (LAS) it read the whole series before turning it, gigabytes for an fMRI run, and was then
  stopped after 30 seconds.
- The DICOM decoder example shows colour pictures right with pydicom 3, which gives YCbCr as RGB
  already: turned again, a JPEG-compressed ultrasound or dental photo came out green and purple.
  With pydicom 3 it also decodes only the frame shown, not every frame of a clip.
- The HDF5 decoder example shows a file's largest dataset, not the first one listed: of a climate
  file it showed lat_bnds, a strip two pixels wide, instead of the temperatures.
- A frame of a large animation stepped to, then zoomed in, stays shown and counted right: the
  full-resolution decode keeps fewer frames, and the view showed the first while the status bar
  said "frame 11/3".
- Save This Frame of a TIFF's first page follows Auto Rotate, as the view does: with it off, the
  PNG was still turned by the EXIF orientation.
- Save This Frame and Export Frames decode an animation once, not first all of it to learn that it
  is animated.
- A plain list's relative paths with .. name the image as its folder does, so its stars are the
  same; file://localhost/ URIs are read, those of another computer left out, and a % not followed
  by two hex digits is kept as it is.
- The hint after a failed network mount names the server, also when the address has a user and
  password (smb://user:pw@nas/share gave "try user.local") or an IPv6 address.

## [0.31.0] - 2026-10-17

### Added

- glimt --back-up FOLDER copies Glimt's data to a new folder glimt-backup-DATE in FOLDER, as File
  → Back Up Glimt's Data… does, without a window: for cron or a systemd timer.
- The status bar counts the files of a copy, move, delete, keywords set on many files or a
  Lightroom import as they go (Copying 120 of 3,000…), with a Stop button that ends it after the
  file at hand; one that took more than two seconds says when it is done.

### Fixed

- Ctrl+Shift+↑, ↓, Home or End (File → Move in Collection) in an empty collection closed Glimt.
- The ImageMagick decoder examples name the format before the file (psd:, tga:, exr:, xcf:, fits:,
  dcm:), one desktop file each: ImageMagick went by the file's bytes, so a file from the internet
  named .psd could reach Ghostscript or another of its coders. graphics.desktop and
  science.desktop are now photoshop, tga, exr, imagemagick-fits and imagemagick-dicom.
- Check Images stops when another folder or collection opens: it went on, and marked files of the
  new list that had the name of a broken file in the old one, which Select Them then selected.
- Rename Collection and Move Collection to Trash are no longer offered for a plain list (.txt,
  .m3u), which Glimt promises never to write: renaming made show.m3u a .gqv, and trashing sent the
  user's own list away.
- A decoder plugin that takes too long is stopped in the Flatpak too: there it runs on the host,
  which the kill of its process group did not reach, so a hanging one kept a decoding thread busy.
  It now gets TERM, which flatpak-spawn passes on, KILL a second later, and flatpak-spawn's
  --watch-bus.
- Closing Glimt while files were being copied or moved stopped it halfway, without a word, and
  could leave a half-copied file under its real name: closing now waits for them (a second close
  does not), and a copy is written under a hidden name and gets its own when whole.
- A collection played as a random slideshow keeps its own order: taking an image out, or moving or
  renaming one, while it was shuffled saved the random order over the one you set, and stopping
  the slideshow left it shuffled. Moving images in it waits until random order is off.
- Marks and stars follow the files Glimt copies, moves and renames (also with Rename Many), and go
  with those it deletes: they were kept by the old path, so every star was lost by a rename, and a
  new photo with a deleted one's name got its marks.
- Find Duplicates no longer takes one file reached twice, through a linked folder, for its own
  duplicate: checked and moved to the trash, the only copy went.
- Saving a turned PNG or WebP keeps the XMP inside it (keywords, comment, alt text other programs
  wrote there), as it keeps the EXIF: it was dropped.

## [0.30.0] - 2026-10-16

### Added

- File → Open Folder Recursive…: opens a folder with the images of every folder below it, as
  View → Include Subfolders, which turns it off again.
- `--lock-settings`, or a file `/etc/glimt/lock-settings` an administrator puts there, for a
  shared or public computer: Preferences do not open, the sort manager's folders stay as they
  are, View → Theme neither saves a theme nor opens the themes folder or the browser, and the
  settings file is never written, so what is changed in the menus lasts only
  until Glimt closes.
- `glimt --check-config [FILE]` tells what is wrong in the settings file, line by line: lines
  that are not `setting=value`, settings that do not exist or are set twice, values that
  cannot be read or are out of range, with the value used instead. It exits with 1 when
  anything is wrong.
- The Help Manual has a chapter on the command line, with the options and remote commands
  from the same lists as `glimt --help`, and covers what it left out: Back and Forward,
  Refresh, New Window, deleting for good, Select None and Invert, Copy Path, playing videos,
  the thumbnail cache and more.

### Changed

- Glimt tells in its window what is wrong in the settings file when it starts (the first
  problem, and how many more), and when it cannot read or save a file; before, a wrong value
  silently took the default and a failed save was told only on the terminal.

### Fixed

- Thumbnails on a scaled screen (150 % and more, or a larger size of menus and lists) were
  drawn smaller than their box, from the 128-pixel cache; they come from the 256-pixel one
  then, and fill the box. Zoom 1:1 was checked sharp at 100 to 200 %.

## [0.29.0] - 2026-10-16

### Added

- Window → Projector: a second window with only the image, for another screen or a projector,
  while you browse in the main one. It follows the shown image and the slideshow, shows the
  same part of the image when zoomed, and a yellow ring where the pointer is over the image in
  the main window (Window → Show the Pointer on the Projector). F or a double-click makes it
  full screen on the screen it was dragged to; its arrow and slideshow keys work too. Nothing
  is decoded twice: both windows show the same textures.
- Zoom modes as ImageGlass has them: H fits the width and W the height, as Geeqie's keys, and
  View → Zoom → Fill the View fills the window, cut at the sides. They stay for the next images
  until X fits whole images again, and are remembered; a zoomed image is decoded sharp enough.
- Frames of animations: Ctrl+Page Down and Up stop a GIF, WebP or APNG at the next or previous
  frame (the status bar says which), and A plays on; for a TIFF they go through its pages as
  before. For such a file, File → Save This Frame… saves the frame or page shown as a PNG, and Export Frames…
  every one, at full size, into a new folder (never over a file).
- Animated PNG (APNG) plays, as animated GIF and WebP do; it showed only its first frame.
- Selecting with a rectangle in the grid of icons (Ctrl+I), as Geeqie and file managers do: drag
  over the images, which are selected as the rectangle touches them, added to the selection
  with Ctrl or Shift. A click still opens an image; the grid scrolls at its edges; in a
  collection the rectangle starts between the images, as dragging one moves it.

## [0.28.0] - 2026-10-16

### Changed

- The menu bar follows the pointer: with a menu open, pointing at another menu opens it, and the
  open menu closes 0.4 s after the pointer has left the bar and every open menu and submenu. A
  menu opened from the keyboard stays while the mouse is still.

### Fixed

- Browsing fast onto a video showed the photo before it under the video's play button until
  ffmpeg had its frame. While an image is decoded, its own thumbnail now stands in for it when
  there is one; a video without one shows only the play button, a photo the image before.
- Network folders: the message of a mount that failed stayed under the images after another
  address was mounted, or another folder opened, so it looked as if the old address was tried
  again. "Connecting to …" now shows which address is tried, a new folder takes the message
  away, and a long message wraps on a dark band instead of running off both sides.
- A server name the system cannot look up made Samba fail with "Invalid argument" after the
  login; the message now says the name is not found and suggests name.local or the IP address.
- The fields of the login dialog for a network folder were a few letters wide.

## [0.27.0] - 2026-10-16

### Added

- In a collection, images are dragged in the file list to another place, a line showing where they
  go; the list scrolls at its edges.
- View → Filter shows images and thumbnails in greyscale, inverted or with more contrast, for low
  vision; Shift+G goes to the next filter.
- A Read Aloud plugin in examples/plugins: the alt text of the selected images, or their comment,
  or their name, spoken through speech-dispatcher, for those who see poorly but use no screen
  reader.
- A ring in the accent colour shows where the keyboard focus is once Tab has moved it, also in the
  menus and on the flat toolbar buttons, where egui draws none; a click puts it away, as in GNOME.

### Fixed

- The orange mark of Check Images shows in the grid of thumbnails (Ctrl+I) too, not only in the
  list.

## [0.26.0] - 2026-10-16

### Added

- File → Back Up Glimt's Data… copies Glimt's own files (settings, marks and stars, collections,
  keyword tree, plugins and decoders) to a new folder glimt-backup-DATE wherever you choose; the
  README and the manual say how to restore them, and that keywords and comments are in the
  sidecars beside the photos.
- Import Lightroom Catalog writes each image's keywords into its XMP sidecar, with their place in
  the keyword tree, beside those it has: what Lightroom keeps only in its catalog unless it writes
  XMP. A box in the import (kept for the next one) turns it off; the keywords of virtual copies go
  to their file once.
- File → Check Images… opens every image of the list in the background, as the viewer would, and
  lists the ones that cannot be: broken files, JPEGs cut off in copying, and files that are no
  image under an image's name. They are marked in the file list; a click shows one, Select Them
  selects them all, to move or delete.
- Images of a collection put in order with keys: Ctrl+Shift+Up and Down move the selected ones (or
  the shown one) a place earlier or later, Ctrl+Shift+Home and End to the start or end (also in
  File → Move in Collection). The order is saved at once, and the slideshow plays it.
- The size of the menus, lists and text, from 50 to 300 % in Preferences, and larger still when
  GNOME's Large Text or text scaling is on (a box turns that off). egui's own Ctrl+0, which reset
  it, is off.
- Glimt takes its High Contrast theme while the desktop asks for high contrast (GNOME's and KDE's
  accessibility settings, through the XDG desktop portal), whatever theme is chosen; a box in
  Preferences turns that off.

### Fixed

- A window closed while the menus and lists were larger than 100 % opened smaller the next time:
  its size is kept in the desktop's units.

## [0.25.1] - 2026-10-16

### Added

- Sony's pixel-shift .arq, GoPro's .gpr and Panasonic's older .raw files, shown by their preview
  as the other RAW files.

### Changed

- glimt --help and the man page say that --slideshow plays a collection or a plain list (.txt,
  .m3u) in its order, and the man page has more examples: from one image to remote control.

## [0.25.0] - 2026-10-16

### Added

- Plain lists of images open as collections: a .txt or .m3u file with one image on each line,
  relative to the list or absolute, # for comments, also file:// lines as music players write
  them. glimt --slideshow show.txt plays it in its order. Glimt never writes such a list: an image
  taken out or another order stays in the window.

### Changed

- README, man page and manual brought up to date with 0.22–0.25: a Formats chapter with every
  format Glimt reads itself, combined searches, decoder plugins, screen readers, alt text in the
  sidecar, the remote-control sockets and the Rename Many journal among the files, the programs
  decoders and printing need, and all the checks to run before a commit.

## [0.24.2] - 2026-10-16

### Added

- Groups in searches: parentheses as words of their own, sunset ( exif:canon | exif:nikon ), and
  -( … ) to leave a group out; a regular expression keeps its own parentheses and bars
  (re:^(IMG|DSC)).

## [0.24.1] - 2026-10-15

### Added

- Search terms combined: terms one after the other must all match (sunset exif:canon stars:3), a -
  before exif:, re:, mark: or stars: means it must not (sunset -exif:iphone), and | between terms
  means either (exif:canon | exif:nikon). A search of one term works as before.

### Fixed

- A long message where the image should be (a file that cannot be opened, a decoder's error) is
  broken into lines inside the image area, instead of being cut off at both sides.

## [0.24.0] - 2026-10-15

### Added

- Decoder plugins: a desktop file in ~/.local/share/glimt/decoders names a program that turns a
  format Glimt does not read itself into an image, so it is listed and shown with thumbnails.
  examples/decoders has JPEG 2000 (ffmpeg) and FITS, DICOM and OpenEXR (ImageMagick); the README
  says how to write one, and which formats go into Glimt itself.
- Radiance HDR (.hdr), read by the image library Glimt has already, tone-mapped to the screen: the
  average brightness made middle grey and the highlights rolled off instead of cut.
- Decoder plugins for medicine and research in examples/decoders: NIfTI brain scans (the middle
  axial slice), whole pathology slides through OpenSlide (.svs, .ndpi, .mrxs …) and HDF5 and
  NetCDF-4 data (the first dataset of two or more dimensions). A decoder's endings may have two
  parts, as .nii.gz.
- Zeiss .lsm microscopy and .gel gel scans, which are TIFF files under other names: listed and
  shown, their first page.
- A Bio-Formats decoder in examples/decoders: microscope files as Fiji reads them (Zeiss .czi,
  Leica .lif, Nikon .nd2, Olympus .oib/.oir, ICS and more), their first plane scaled to 8 bits,
  through Bio-Formats' bfconvert.
- An MRC decoder in examples/decoders, for cryo-electron microscopy: a micrograph, or the middle
  section of a map or tomogram (.mrc, .mrcs, .map, .rec, .st, .ali), through mrcfile.
- A FITS decoder in examples/decoders, through astropy: the first image of a telescope file, also
  compressed (.fits.fz, .fits.gz), with the zscale stretch astronomers use. Decoders are read in
  the order of their file names, so when two take the same ending the first does; science.desktop
  is left for FITS and DICOM through ImageMagick alone.
- A DICOM decoder in examples/decoders, through pydicom and GDCM: X-ray, CT, MR, ultrasound,
  panoramic and intraoral X-rays, also compressed (JPEG, JPEG 2000, JPEG-LS), in the window the
  file names and with MONOCHROME1 the right way; no patient field is read out or kept.
- NeXus files (.nxs) from neutron and synchrotron sources in the HDF5 decoder, as they are HDF5.
- Decoders for graphics in examples/decoders, through ImageMagick: Photoshop (.psd, .psb) as
  saved, TGA and OpenEXR (graphics.desktop), and GIMP's .xcf with its layers put together
  (xcf.desktop); transparency kept.

### Changed

- The README, man page and manual say that remote commands reach only Glimt on the same computer,
  through a private socket, never over the network, and how to reach another computer (ssh).

## [0.23.1] - 2026-10-15

### Added

- Grey TIFFs of floats or signed numbers, as science writes them (height models, microscopy):
  shown stretched from their lowest value to their highest, on every page. Before, they could not
  be shown.
- BMP, ICO (the largest of its sizes), QOI and PNM (PBM, PGM, PPM, PAM) images, listed and
  shown as the other everyday formats.
- Alt text and Extended description in the EXIF panel: IPTC's fields for accessibility in XMP
  (Iptc4xmpCore:AltTextAccessibility and ExtDescrAccessibility), read from the sidecar or the
  image and saved to the sidecar when edited. A screen reader reads the alt text first, and the
  example AI plugin writes its description there too.

## [0.23.0] - 2026-10-15

### Added

- Screen readers such as Orca (the accessibility feature, in the packages): the menus, buttons,
  lists and dialogs, and the shown image read as its name, place, stars, keywords and comment each
  time you browse. Built without it, Glimt has none of the extra crates it needs.

## [0.22.2] - 2026-10-15

### Added

- examples/plugins: a plugin that writes a description of each selected image as its comment, made
  by a vision model on this computer (Ollama), with the README's notes on using scripts and AI
  safely.
- glimt --remote current, selection and select PATH…: a script, or an AI assistant you run, can
  ask which image is shown and which files are selected, and select files for you. Paths come with
  a NUL byte after each, so names with line breaks stay whole; select only selects, and no remote
  command changes a file.

### Security

- The folder of the remote-control sockets is made private to the user (0700), even where
  $XDG_RUNTIME_DIR is not.

### Fixed

- With both Start in the last used folder and Remember the window size and position turned off,
  the folder and window kept from before are forgotten at exit, instead of coming back when a
  setting is turned on again.
- Saving the orientation of a damaged JPEG, whose JFIF segment claims to be longer than the file,
  says the file is cut off instead of closing Glimt. Found by the new fuzz test of the orientation
  write.
- Building from source needs Rust 1.95 (egui's), not 1.89 as README said; Cargo.toml says so too,
  so an older Rust gets a clear message.
- A collection that lists a file twice shows it once, so Find Duplicates can move its checked
  files to the trash instead of saying they are no longer all in the list.

## [0.22.1] - 2026-10-15

### Changed

- Thumbnails: a file none can be made of gets a marker in the shared cache (fail/glimt-VERSION),
  as the freedesktop specification says, so it is not decoded again at each start until it
  changes. HEIC, AVIF and videos get none, as ffmpeg may come later.

### Fixed

- Rename Collection: a name that is taken only in Glimt's own collection folder is free for a
  collection elsewhere.
- Import Lightroom Catalog without $HOME: the collections are not saved, and the message says why,
  instead of landing in the folder Glimt was started from.
- Keyword tree: Remove from the tree on a level with levels below it (Places, with only
  Places|Norway|Oslo kept) removes it with them, instead of nothing.
- File list with thumbnails and no columns: it can no longer be dragged narrower than a thumbnail
  with a little of its name.
- Two Glimt of the same --name started at once after a crash: the second no longer takes the first
  one's new remote socket away.
- Auto refresh keeps the selection: when another program writes into the folder, Delete and Move
  still act on the selected files, not only the shown one.
- Video player command: a path or argument with spaces works in "quotes", as in a desktop file's
  Exec line.
- A decode asked for before the page, the colours or the turned file changed no longer takes the
  place of the new one: a TIFF page is not decoded twice, the slideshow waits for the next image,
  and the old colours or the unturned image no longer show for a moment.
- Marks and stars set in two windows at nearly the same moment: neither change is lost.
- A PNG or WebP whose EXIF Glimt cannot read can be saved turned: the EXIF is kept as it is,
  instead of the save being refused.
- A PNG or WebP saved turned leaves out its EXIF thumbnail, which other programs showed the old
  way up.

## [0.22.0] - 2026-10-15

### Added

- A Rename Many cut short by a crash or a power cut is found at the next start: Glimt offers to
  put the old names back or to finish. Before, files could be left under hidden temporary names
  and seem to be gone.
- A calendar in the Pan view (Ctrl+J): the months with images, a week to a row, each day with
  its first image and how many it has. A click on a day shows it in the timeline.

### Changed

- Building from source needs Rust 1.89 or newer (file locks).

### Security

- The release is built in a job that can only read; checksums, the signed build provenance and
  publishing happen in a job of their own that builds nothing. Before, every crate's build script
  ran where the signing token could be asked for.

### Fixed

- Saving a turned PNG or lossless WebP on a disk that filled up could replace the photo with a
  cut-off file: images are now encoded in memory and written to disk in full before they replace
  the old file. Glimt's own files are also on disk before they replace the old ones, so a crash
  never leaves an empty one.
- A setting, the keyword tree, a collection or a sidecar reached through a symbolic link (a
  dotfiles repository) had the link replaced by a plain file at the first save; it is now written
  where the link points.
- A single crafted file can no longer stall or freeze Glimt:
  - a fake RAW file made of JPEG start markers took minutes on every decode;
  - a GIF of 100,000 tiny frames froze the window, and a huge canvas took twice the limit;
  - a TIFF of 100,000 pages took minutes to count;
  - ffmpeg had no time limit and its whole output was held, gigabytes for a crafted HEIC;
  - a decoder crash ended the EXIF panel for the session, and could end a sort by EXIF date.
- Only files are images in the list. A folder named like an image (`2019.jpg`) was listed as a
  broken image, so Delete could send the whole folder to the trash; a pipe or a link to a device
  named `a.jpg` could block Glimt or fill its memory as soon as the folder opened. Such entries
  are now a folder or left out, and nothing that is not a file is ever read as an image.
- An SVG could show any local file as a picture, and read a device such as /dev/zero until
  memory ran out; a picture inside it was decoded at any size. Linked pictures are now only
  files under 64 MB, and none may claim more pixels than an image Glimt opens.
- Moving photos to another disk (a USB stick, an SD card) removed the originals as soon as the
  copies were written, before they were on the disk: pulled out right after, both could be lost.
  A copy is now on disk before the original goes, and keeps its modification time, so sorting by
  date still works after copying or moving to another disk.
- Printing to a PDF emptied the file chosen before the images were read: choosing one of the
  images being printed lost that image's page, and a write error halfway left a broken PDF in
  place of the old file. The PDF is now written beside it and takes its place when it is whole.
- With two windows open, adding a folder to the sort manager in one dropped the folders the
  other had added, and a crash while saving could leave half the list. A change now goes to the
  list as it is on disk, written whole or not at all.
- Keywords written by other programs are kept as they are:
  - Lightroom's levels above a keyword (Places, Norway for Places|Norway|Oslo) no longer
    become keyword paths of their own when Glimt saves; one written so by 0.21.0 goes again.
  - Ticking a keyword or adding keywords leaves the comment as it was, in every language.
  - Keywords that differ only in letter case ("Rose", "rose") are both kept; only a keyword
    added in another case than one the image has is left out.
- Copying, moving or renaming a photo replaced a sidecar already at the new name, with the
  keywords of another file; it is now kept, and Rename Many checks the sidecars' new names too.
- Moving on from an image could rewrite its keywords and comment without any edit: a keyword
  with a comma ("Smith, John") was split in two, a comment lost its last line break, and a JPEG
  with keywords inside got a new sidecar just from being shown. Fields left as they were shown
  are now never written.
- Editing a comment while keywords were being set on many files in the background could take
  a keyword off a file again; a tick quickly followed by an untick could leave files ticked; and
  the keyword tree paths inside an exported JPEG were lost when a box was ticked. Saving now
  writes only the fields that were edited, ticks are written in the order they were made, and
  the paths come from where the keywords do.
- Import Lightroom Catalog:
  - it looked for the wrong lock file, so a catalog open in Lightroom was read anyway;
  - a virtual copy counted as a photo of its own, so one file could get two star ratings;
  - a folder typed as `~/Pictures` or `Pictures` gave collections that opened empty: `~/` is the
    home folder now, and a relative path is left out;
  - with stars in XMP, photos deleted since got a sidecar of their own.
- / did nothing in full screen or with the tools hidden (Ctrl+H), but still turned the search
  field on: it now brings the file list back with the cursor in its search.
- Showing several folders together (`glimt /a /b`) emptied the folder tree until a refresh.
- Find Duplicates read each listed file's size on every frame, slow on a network share.
- One edited keyword or comment threw away the index of the `exif:` search: in a large folder the
  next search read every file again. Only the files changed are read again now.
- Changing the thumbnail size, Auto Rotate or colour management while thumbnails or the
  neighbouring images were being made kept some of them in the old size, turn or colours until
  Glimt started again; they are now made again in the new look.
- Scrolling fast through a search, or through the Pan view, made a thumbnail for every image
  passed instead of skipping those out of view; only the ones shown, and some around them, are
  made now.
- Rename Many held the window while it checked and renamed every file, minutes for thousands on
  a network share; it now runs in the background. It also never replaces a file another program
  made under a new name meanwhile, and checks the sidecars' new names too.
- The man page ran two entries of its FILES section into the one before, printed the
  ENVIRONMENT line twice and left out the keyword tree's file. About now credits every crate
  built in, with its licence.
- File names that do not fit in the list are cut in the middle with "…", so their end and
  extension show ("fjord_s…ise.jpg"); they were cut off at the end, also before stars.
- With the size and date columns and thumbnails, the list was too narrow to show any name: it
  is now at least wide enough for the thumbnail, a name and both columns.

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

[Unreleased]: https://github.com/thornbux/glimt/compare/v0.34.1...HEAD
[0.34.1]: https://github.com/thornbux/glimt/compare/v0.34.0...v0.34.1
[0.34.0]: https://github.com/thornbux/glimt/compare/v0.33.0...v0.34.0
[0.33.0]: https://github.com/thornbux/glimt/compare/v0.32.0...v0.33.0
[0.32.0]: https://github.com/thornbux/glimt/compare/v0.31.0...v0.32.0
[0.31.0]: https://github.com/thornbux/glimt/compare/v0.30.0...v0.31.0
[0.30.0]: https://github.com/thornbux/glimt/compare/v0.29.0...v0.30.0
[0.29.0]: https://github.com/thornbux/glimt/compare/v0.28.0...v0.29.0
[0.28.0]: https://github.com/thornbux/glimt/compare/v0.27.0...v0.28.0
[0.27.0]: https://github.com/thornbux/glimt/compare/v0.26.0...v0.27.0
[0.26.0]: https://github.com/thornbux/glimt/compare/v0.25.1...v0.26.0
[0.25.1]: https://github.com/thornbux/glimt/compare/v0.25.0...v0.25.1
[0.25.0]: https://github.com/thornbux/glimt/compare/v0.24.2...v0.25.0
[0.24.2]: https://github.com/thornbux/glimt/compare/v0.24.1...v0.24.2
[0.24.1]: https://github.com/thornbux/glimt/compare/v0.24.0...v0.24.1
[0.24.0]: https://github.com/thornbux/glimt/compare/v0.23.1...v0.24.0
[0.23.1]: https://github.com/thornbux/glimt/compare/v0.23.0...v0.23.1
[0.23.0]: https://github.com/thornbux/glimt/compare/v0.22.2...v0.23.0
[0.22.2]: https://github.com/thornbux/glimt/compare/v0.22.1...v0.22.2
[0.22.1]: https://github.com/thornbux/glimt/compare/v0.22.0...v0.22.1
[0.22.0]: https://github.com/thornbux/glimt/compare/v0.21.0...v0.22.0
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
