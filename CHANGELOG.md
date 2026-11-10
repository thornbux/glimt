# Changelog

All notable changes to Glimt. The format follows [Keep a Changelog](https://keepachangelog.com/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- A write that fails while Glimt waits to close, such as keywords typed for a photo on a
  read-only folder and then Ctrl+Q or a logout, keeps the window open with the failure shown,
  and prints it in the terminal. Close again to close. Before, the window closed as the failure
  came, and the keywords were lost without a word. Failed moves, copies and deletes are printed
  in the terminal too.
- A failed write stays on screen when another notice comes at the same moment. Before, a
  Lightroom import's "imported" could hide "Could not save the keyword tree".
- A window closed twice while files are written still waits, when it closes, for the last
  folder and window size and for the marks to be saved. Only a third close ends at once.

## [0.80.0] - 2026-11-10

### Added

- Preferences → Memory and CPU sets the most memory an added format's program may take, 4096 MB
  by default (the setting `added_format_memory_mb`), for Java with a large heap. A program that
  runs out of it is told so, with the limit, and finds it in `GLIMT_FORMAT_MEMORY_MB`; the
  Bio-Formats example keeps Java's heap under it.

### Changed

- Copy Image to Clipboard moves from the Select menu to Edit, beside Crop and Orientation. It
  copies the shown image, while Copy Path and Copy Files in the Select menu copy the selection.
- The Help Manual's differences from Geeqie name each key that Geeqie 3.2 changed, such as
  Ctrl+C, Ctrl+M, Ctrl+R and Ctrl+T, and what "/" does. Glimt keeps its keys.
- Before 1.0, a few names that misled their reader are renamed: the settings `low_memory`,
  `fade` and `icons` are now `one_malloc_arena`, `slideshow_fade` and `icon_grid`, and the
  actions `CopyFiles`, `ExifRotate`, `Escape`, `Up` and `Home` are now `CopyFilesToClipboard`,
  `AutoRotate`, `LeaveFullScreen`, `ParentFolder` and `HomeFolder`. The old names keep working
  for good, in the settings file, in `keys=` and with `--remote run`, and Glimt writes the new
  ones. The option `--fade` keeps its name.

### Fixed

- `--remote-all` exits with 3, not 1, when one window does not answer within 10 seconds while
  another answered, so a script that retries on 1 no longer does the command twice in the
  windows that did it. It exits with 1 only when no window took the command or none answered.
  The man page names every case of exit 2.
- A window that listens for `--remote` with remote control off, as a named window does, becomes
  the Glimt used last when it gets the focus, so plain `--remote` goes to it.
- `glimt --remote open smb://nas/photos` mounts the share, as `glimt smb://nas/photos` does,
  and the commands sent after it wait for the mount. Before, it opened `smb:/nas/photos` in the
  working folder, which does not exist.
- A tool's `%f` or `%u` inside a word, as in `--input=%f`, is filled in with each file, as an
  added format's is. Before, the tool ran once with the text as it was, and no file.
- A tool's or added format's `%f` inside a "quoted" word, such as `sh -c "exiftool %f"`, stays
  as it is, as the desktop entry specification says. Before, the file's name was put into the
  shell script's text, and a downloaded file named `x$(command).jpg` ran the command. An added
  format whose `%f` is only inside quotes, such as `sh -c "convert %f ppm:-"`, is therefore no
  longer loaded, and Glimt says so at start and in `glimt --check-config`. Rewrite it with `%f`
  as a word of its own, read by the script as `"$1"`: `sh -c "convert \"$1\" ppm:-" sh %f`.
- Printing on both sides with orientation Automatic turns every page as the first one, so the
  back of a landscape page is no longer upside down.
- A CMYK JPEG or TIFF with its CMYK profile in the file, as print and stock photos often are,
  shows in its colours. Before, the view, the thumbnails (also in the shared cache that file
  managers read), prints, Copy Image and Export Frames showed it nearly black.
- Export Frames writes a 16-bit page with a colour profile in 16 bits, as one without a profile.
  Before, it came out in 8 bits.
- A file with `;` in its path has its thumbnail in the shared cache under the name GNOME and KDE
  give it. Before, each program made a thumbnail of its own.
- Remember Last Folder keeps only a place that opened. Closing the window while several folders
  were shown together kept `/`, and the next start listed the whole disk; closing it before a
  slow folder had opened lost the folder remembered.
- A path that is not there, among several opened, is left out. Before, it stood for its parent
  folder, whose images were shown with the others.
- After Rename Many, the new names and the removal of its plan are on disk before the marks and
  collections follow them, also when no plan could be kept. A power cut right after could
  otherwise put the marks on the wrong photos at the next start.
- When Rename Many was cut short with its plan on another disk than the photos, putting it right
  at the next start could leave a photo under a hidden `.glimt-rename-*` name with no plan left
  to find it. A repair that cannot finish now puts back the steps it did.
- When Rename Many swapped names on a card or stick that was pulled out before its first renames
  reached it, the next start took the swap as finished. Finishing it moved the marks, stars and
  collection entries of each photo to the other, and putting back the old names swapped the
  photos themselves. Such a swap is now known as not begun. A swap whose first renames the disk
  says it could not keep is undone at once, with the error told.
- When Rename Many failed and could not give every file its old name back, a keyword, comment,
  alt text or star set meanwhile is no longer written to the old name, which may hold another
  photo by then. It is left out, and the user is told with the rename's error.
- A collection that came with a download, with a pipe or a link planted as its hidden lock file
  `.NAME.gqv.lock`, no longer hangs the window at Rename Collection or stops every later
  collection edit, and a link there no longer makes an empty file where it points. Rename
  Collection now renames in the background, so another window's edit on a share that stopped
  answering no longer holds the window either.
- `--remote select` and `info` with more than 100,000 files are refused by the window too, with
  "not done", not only by Glimt's own client. Another program on the socket could freeze the
  window with millions of paths.
- The programs of added formats get a memory limit of 4 GB. Before, a small downloaded file
  whose header claimed a huge picture could make an example script take gigabytes.
- A setting of a few values with a value it does not take, such as `layout=3x3` or
  `print_paper=letter`, is put back to its default, and `glimt --check-config` tells the line.
  Before, the check said the file was fine, and the window fell back to another value without a
  word. The language setting takes a language's own code too, so `language=nb`, `nn` or `pt`
  gives that language, as `LANG=nb_NO` does, and no longer English. A language Glimt does not
  have still shows English, and a layout it does not know in `slideshow_one_image_at_a_time`
  is kept, so that a value of a later Glimt is not lost; the check tells both.
- Every action can be given a key of its own in Preferences → Keyboard and in `keys=`, also one
  without a default key, such as Toolbar or Open With. Before, such a key was kept and never
  used, without a word. `glimt --check-config` tells an own key that names no action or a key
  it cannot read.
- A change of the settings whose save failed, as on a full disk, is written by the next save
  that works. Before, that save put the file's old value back, and the change was lost at the
  next start.
- Taking out one of two keywords that differ only in letter case, such as "rose" beside
  "Rose", keeps the other. Before, both went from the sidecar.
- Turning a PNG keeps its XMP (keywords, title, description) and its EXIF when they come after
  the pixels, where ExifTool adds XMP. Before, the turned PNG that replaced the file had lost
  them. A copy (Save As, Crop) keeps them too.
- An empty `<dc:subject/>` or `<dc:description/>` in a sidecar is read as empty. Before, the
  keywords and comment were read from the lists after it, such as the keyword tree, and a
  keyword edit wrote them into the sidecar.
- Copy for Sharing leaves out the EXIF of a crafted PNG or WebP whose tags all name one large
  value, and copies a tag named twice once. Before, such a file could make Glimt fill the
  computer's memory.
- Find Duplicates shows the busy pointer while it compares, until its groups are listed, as
  other work in the background does.
- The window no longer waits for the disk where a home folder or a share that stopped answering
  could hold it. The theme menu and Preferences read the theme files in the background, and a
  colour profile chosen is read there. Save As no longer reads a WebP file in every frame of its
  dialog. New Collection, Set Window Name, Rename Many, Print to a file, opening a file, Import
  Lightroom Catalog and Edit → Tools look up what they need in the background too, and so do
  the file dialog for a colour profile and a tool's check of the file types it takes. Open
  Recent, the Sort Manager's folders, the keyword tree and the window's state are read before
  the window opens, the window's state once instead of twice.

- Zoomed in, Alt+arrow changes the pane and Shift+arrow extends the selection again. Before,
  both moved the image, as only an arrow pressed alone should.
- The screen saver stays off while the projector is full screen, also when the main window is
  not, as during a slideshow on a projector.
- The keyword tree in the info panel lays out only the keywords on screen, and is built only
  when it changes, so a tree of thousands of keywords from a Lightroom import no longer slows
  every frame. A keyword's children open with the caret before it.
- Check Images stopped before the end says how far it got, not "None broken.".
- Esc in a text field or an open list of Preferences only leaves the field or closes the list.
  Before, it closed Preferences as well.
- The Help Manual names Refresh under View and New Window under Window, and the keyboard map
  and shortcut list leave the Delete key out while Enable Delete key is off.
- The Help Manual says that a change on a network share shows by itself within seconds, and
  that a caching mount such as rclone or s3fs can hold it back until its cache expires.
- A keyword, comment, alt text or star written while Rename Many runs, also by Import Lightroom
  Catalog, is saved to its photo under the photo's new name. Before, when the new names swapped
  with old ones, it landed on the photo that took the old name. An edit in the info panel not saved yet follows its file through
  a move or rename too.
- Stop in the status bar, or closing the window a second time, no longer drops the info panel's
  save of keywords, comment or alt text that waits behind a long job, such as a Lightroom
  import. Before, the edit was lost without a word while the panel showed it as saved.
- Going back to an image whose edit in the info panel was not saved yet shows the edit. Before,
  while the next image was still being read, the panel could show the file as it was, and a
  later edit wrote the old alt text or comment back.

## [0.79.0] - 2026-11-10

### Changed

- A folder that takes more than three seconds to open, as on a share that stopped answering,
  shows a notice that it is still opening and that opening another folder stops the wait.
  Before, only the busy pointer showed it.
- With Include Subfolders on, Auto Refresh now sees an image added to, removed from or renamed in
  a subfolder, not only in the folder opened. The watcher still looks once a second, which works
  on network shares too: at the open folders every second, and at up to 1,000 subfolders a
  second, so a tree of 10,000 subfolders is looked through in ten seconds. After a listing,
  a change waits four times as long as the listing took before the folder is read again, so a
  sync client writing into a large tree has it read at most a fifth of the time; a small folder
  is read again at once.
- Rename, Rename Many, New Folder and Rename Folder refuse a new name that starts with a dot, which
  the list hides, unless hidden files are shown. Before, Rename Many with `.#` gave `.1.jpg`,
  `.2.jpg` …, and the files vanished from the list. Collections refused such names already, by
  the same rule now.
- Copy for Sharing knows the namespace that digiKam 4 wrote its keyword list in, so Place alone
  keeps that list. Before, any ticked box left it out, as a tag Glimt did not know.

### Fixed

- Find Duplicates offers "Check all but the first of each group" only when comparing by
  checksum, the one way that finds copies. By name, size, dimensions or likeness a group may
  hold different photos, and Move to Trash now warns so and asks before it goes on, also with
  confirming turned off.
- A file whose name has a line break is refused, with a notice, when it is added to a
  collection or given a mark or stars. Glimt's files keep a name on a line, so such a file
  seemed added or marked and was left out at the next save without a word (#92). The same
  notice comes when such a name is reached otherwise: a file moved, copied or renamed into a
  folder whose name has a line break, a folder renamed to one, or stars from a Lightroom
  catalog. A moved file's marks are then forgotten under its old name too.
- Delete to the trash works without `gio` (a `.deb` installed without its recommended packages,
  a tar.gz or AppImage on a small system): Glimt moves the files into the trash itself, as the
  trash spec says. A file on another disk, such as a USB stick, goes into `.Trash/UID` or
  `.Trash-UID` at the top of that disk, also when it is reached through a link, and is named
  there from that top, so the stick restores to its own folders wherever it is mounted; a file
  on a network share is refused, as gio refuses it. A large delete syncs the trash once for
  each hundred files, not twice for each file.
- A file moved into the trash by Glimt itself has its `.trashinfo` on disk before it moves, so
  a power cut cannot leave it in the trash without one.
- Undo of a delete waits for the marks as long as their writing takes, and shows in the status
  bar with Stop meanwhile. Before, it waited 5 seconds and could bring the files back without
  their marks and stars. A stopped Undo puts nothing back and can be done again.
- A move on one disk and a folder rename are on disk before the marks and collections follow
  them, so a USB stick pulled out right after cannot leave a file and its marks under
  different names.
- Undo of a move keeps the files that could not go back (a name taken, a share that did not
  answer) to be undone again, as Undo of a delete does.
- Stop during a copy or move acts inside a large file, such as a video copied to a slow stick,
  and leaves no part of it behind.
- A window started with a `--name` that another window took by a rename at the same moment
  shares the name under its own ID, as when the name is taken already. Before, it was left
  without remote control.
- Renaming a window to the name of a Glimt that hangs no longer hangs the renamed window too:
  a socket that has not taken the connection after half a second counts as taken.
- A third SIGTERM or SIGHUP ends Glimt at once also while it waits at its end for a write to a
  share that stopped answering, as the man page says, with exit status 143. Before, only
  SIGKILL ended it there.
- Go → Back and Forward change their history when the place has been looked up, not when they
  are asked for. Back pressed twice before a slow share answers goes back one step, and a
  folder clicked meanwhile no longer leaves Forward holding the place the user is on.
- File → Import Lightroom Catalog looks at the catalog's root folders, and at the folders typed
  for them, in the background. Before, the window's thread looked at each of them in every
  frame while the dialog was open, so a share that stopped answering held the window.
- Move, Copy and Copy for Sharing hold their files back from other work from the moment they
  are asked for, while the folder's real path is looked up. Before, a delete and a rename in
  that moment could hand a Move to a slow share another file of the same name.
- A Copy or Move looks up the file system of the folder it goes to in the background, with the
  folder's real path, to refuse a name that a camera card does not allow. Before, the window's
  thread looked it up, which a share that stalled in that moment held.
- A Move of two RAW files of one name stem, such as `r.cr2` and `r.nef`, where the second could
  not go or was not reached after Stop, leaves Lightroom's `r.xmp` where it is, as it belongs to
  neither. Before, it went with the first when that one went first. The Undo of such a move
  does the same when one of them cannot go back.
- Marks and stars set one after another while the last one is still being saved go to the
  marks file in one write, not one write each. With 500,000 files marked, a write takes about
  0.4 s, so 500 mark keys took over three minutes to save, longer than a close waits for them.
- A turned PNG or WebP, a sidecar, a file saved over by name and a file moved to another disk
  keep their extended attributes, as `mv` does: the user's own (`user.*`, where KDE's Dolphin
  and Gwenview keep tags, ratings and comments) and the file's ACL. Before, they were lost. A
  copy keeps the user's own but not the ACL, as `cp` does. The owner is not kept, as only root
  may give a file to another user.
- A turned PNG keeps its Photoshop resources ("Raw profile type 8bim"), where ImageMagick keeps
  the IPTC caption, copyright and keywords, without the thumbnail and the copies of the EXIF
  and XMP among them. Before, a turn dropped them, as a crop or copy still does.
- The manual says that a turned PNG with a palette, or of 1, 2 or 4-bit grey, keeps every pixel
  but is saved in full colour or 8-bit grey, so its file grows. It said a PNG lost nothing.
- Cleaning the thumbnail cache removes a thumbnail only when its file is surely gone. Before, an
  error from a share that answered badly for a moment counted as gone, and the thumbnail went.
- An edit of a collection that waits for a share that stopped answering no longer holds the
  settings, marks and Open Recent saved after it: collections are written in a thread of their
  own (#103).
- Rename Many tells when it cannot write its plan (a full disk, a read-only state folder) and runs
  without it, as a crash halfway is then not put right at the next start. Before, the user was
  not told.
- When Glimt closes while a write still runs after its five seconds' wait (a share that stopped
  answering), it says so on the terminal. Before, it ended without a word.
- A saved collection that cannot be written after its images were moved or renamed is told in
  the window. Before, it kept the old names without a word. The open collection, when none of
  the moved images was in its list yet, is told so too.
- A write of the settings, marks, Open Recent or a collection that Glimt broke off with an error
  of its own is told in the window, as every failed save is. Before, the change was lost
  without a word. A sidecar write broken off so is told too.
- README no longer says that Lightroom reads Glimt's sidecars: it reads only its own photo.xmp
  beside a RAW file. Immich reads their ratings, comments and keyword tree.
- Copy for Sharing keeps only a property's own value: a tag written inside a property that the
  copy keeps, such as a GPS position inside a gain map's value or a keyword list, is left out
  unless that tag is kept too.
- Preferences in Norwegian names the Delete key as the menus do, in "Use no trash at all".

## [0.78.0] - 2026-11-10

### Added

- `glimt --action-list` prints the actions that `glimt --remote run` takes, one to a line: the
  action's name, a tab and its menu label in English, as Geeqie's `--action-list` does. Its form
  is frozen in 1.x, so scripts can read it. `glimt --help` now points to it instead of listing
  the names itself.
- `glimt --remote info` prints what Glimt knows about the image shown, and `info FILE…` about
  the files named: the size, date, dimensions, format, orientation, stars, marks, keywords,
  comment, alt text and the info panel's EXIF fields, as a key, a tab and a value on each line,
  with a blank line after each file. A value's backslash, tab and line breaks are written as
  `\\`, `\t`, `\n` and `\r`, a `;` inside a keyword as `\;`, and the path keeps its bytes, so a
  script can undo them. Each file is sent as soon as it is read, so 100,000 files answer too,
  and an answer cut short by a window that closes exits with 3. Its form is frozen in 1.x, so
  scripts and AI assistants can read it.
- Window → Set Window Name… and `glimt --remote name NAME` give a running window a new
  `--name`, which `--remote` reaches it by from then on; without a name, its process ID does.
  With remote control off in Preferences, a window listens while it has a name, as with
  `--name` at start.
- `glimt --remote-all identify` shows each window's ID large over the image for a few seconds,
  and `%id%` puts it in the window title: its name, or its process ID when it has none.
- Several windows may share a `--name`, and `glimt --name NAME --remote` reaches each of them.
  A window that shares its name with one started before it shows its own ID beside the name,
  as `left (4242)`, and `--name 4242` reaches it alone, as it reaches a window without a name.

### Changed

- Saving a file that has other names (hard links), such as a turned image, a copy saved over
  it or its sidecar, tells that the other names keep the old version, once the save went
  through. A job over many such files tells it once, with their count. The save gives the
  name a new file, so the other names are not changed with it. Before, they were left behind
  without a word.
- SIGTERM and SIGHUP, as a logout, a session manager or `kill` sends them, close Glimt as
  Ctrl+Q does: it waits while files are copied, moved or saved, so the marks follow the moved
  files, and writes its state. Each further signal counts as closing again (the second stops
  the files after the one at hand, the third ends at once), and SIGKILL always ends it at once.
  A signal that was ignored when Glimt started, as `nohup glimt` ignores SIGHUP, stays
  ignored. Before, either signal ended Glimt halfway through a job.

### Fixed

- The IMG_1.xmp that Lightroom, Camera Raw, Bridge and Capture One write beside a RAW file IMG_1.CR2
  now goes with it when Glimt moves, copies, renames, deletes or trashes it, and comes back with an
  Undo. Before, it stayed behind, and the next IMG_1.CR2 in that folder got Lightroom's edits and
  keywords. IMG_1.XMP, as Capture One and Photo Mechanic name it, goes too. An IMG_1.JPG or
  IMG_1.DNG beside the RAW file leaves it alone, as Lightroom keeps their metadata inside them, and
  two RAW files of one name (IMG_1.CR2 and IMG_1.NEF) leave it where it is, unless both are moved
  together, in any order, and it goes along. The manual no longer says that Lightroom reads Glimt's
  own IMG_1.CR2.xmp.
- A network share that stops answering (a NAS gone to sleep, Wi-Fi lost) no longer hangs the
  window. Opening a folder, a file or a collection, Open Recent, the file list's size and date
  columns, the status bar's file size, editing a collection, turning random order off, and
  Copy and Move to a folder now look at the disk in the background, and the window stays
  responsive while the share is silent. A `glimt --remote` command sent after `open` waits
  until the place to open has been looked up, so it acts on that place, also in a minimized
  window. `--remote quit` does not wait, and a further `--remote open` replaces the look-up,
  as a click in the folder tree does, so a script can leave a share that stopped answering;
  the commands that waited for the old place are answered that they were not done. A command
  that waits longer than a client does lets its place go, so waiting ones never keep the
  next clients out.
- An Undo of a move, and other work in the background, now lets go of its files before the
  busy pointer goes. On a busy computer, an operation started on the same files at that moment
  was refused with "Still working on these files".
- A Copy to or Move to a folder that is closed at once now waits for the copy or move, as
  closing does for every file operation. While the folder's real path was looked up, a close
  went through, and the files were not copied. The busy pointer now also stays until the window
  has taken what work in the background sent back, such as a report or a turn saved.
- With Remember Last Folder on, Glimt opens its window at once when the last folder is on a
  share that stopped answering, and opens the current folder when the last one is gone.
  Before, it hung before it showed a window.
- Find Duplicates no longer looks at each file's size as its row is drawn, which a share that
  stopped answering held for ever.
- A file renamed and then at once deleted is taken out of the open collection. Before, the
  collection could keep the new name of a file in the trash.
- A turn saved after another program removed the file is told as not saved. Before, it was
  taken as saved, and a JPEG with Auto Rotate off was told "Saved".
- The file list's size and date columns answer the rows on screen first. On a slow share,
  after scrolling through a large folder, they waited behind every row scrolled past.

## [0.77.0] - 2026-11-09

### Changed

- Copy for Sharing keeps an iPhone's HDR gain map as it keeps an Ultra HDR one, so the copy
  still shows brighter highlights on an HDR screen. The gain map keeps its image data and
  Apple's few words that mark it, without its own EXIF or other tags, and the main image keeps
  the two numbers of its maker note that say how bright it may get (the HDR headroom and gain),
  without the rest of the maker note. Before, the gain map went with the extra images.
- Inside, Cargo.toml's lints deny `unwrap`, `expect` and undocumented `unsafe`, a mutex is
  locked in one way that survives a panic, a write's result is never dropped, and the rules in
  AGENTS.md start with the ones never broken, with the release runbook in `docs/RELEASING.md`.
  The release workflow smoke-tests the AppImage and the Flatpak before anything is published,
  and the release tests copy real photos for sharing (Ultra HDR, an iPhone's, a drone's, a
  Lightroom export and one with content credentials) and check what the copies keep.

### Fixed

- The Help Manual names every order of the sort menu (file creation date and the two EXIF
  dates were missing), and says that a tool still running is stopped from Edit → Tools.
- A power cut, or a USB stick pulled out without unmounting it, no longer loses a file moved
  to another disk, its copy and its source both. A saved sidecar, a turned image and the
  settings are on disk with their names when Glimt says they are saved, so an older
  version cannot come back, and Rename Many's plan is on disk before its first rename. File →
  Back Up waits until the backup is on the disk, so a stick pulled out right after holds all
  of it.
- An Undo of a move to the trash puts the files' names on disk before it removes their
  .trashinfo files, so a power cut right after cannot leave a file hidden in the trash's
  folder where no trash view lists it.
- When the disk reports that Rename Many's renames did not reach it, Glimt says so, and the
  plan goes as after any finished Rename Many. A plan kept there made the next start move the
  marks and stars again, erasing them or swapping a swap's back onto the wrong files.
- Copy for Sharing: unticking a tag named like a group's word in the saved choice, such as a
  PNG keyword `group:place`, no longer unticks that group.
- A close of a minimized or covered window no longer loses what a file job reported a moment
  before, so the marks and saved collections follow the files it moved.
- An Undo of a delete puts back the marks and stars that another window set on those files a
  moment before, also when this window had not read them yet, and not a mark or stars cleared
  just before the delete. The marks file is read for it in the background, not in the window.
- A collection open in one window follows the images that another window moves or renames,
  as an open folder does, instead of keeping their old names until it is opened again.
- Folders shown together are read again when an image is added, removed or renamed in one of
  them, as an open folder is, instead of never.
- A screen reader reads out a notice given twice within one frame, such as two refusals of
  `--lock-settings` close together, also the second time.

## [0.76.0] - 2026-11-08

### Changed

- File → Open With Default App opens the selected files, as the Open With submenu beside it
  does, rather than only the shown image. It is in the file list's right-click menu too. It
  asks before it opens more than five, as Nautilus does, because each file starts a program of
  its own, and in the Flatpak a Cancel in the desktop's chooser ends the rest.
- The settings file names the full screen info's setting `full_screen_info`, as the menu and
  the action do, instead of `osd`, and its template `full_screen_info_template` instead of
  `osd_template`. The old keys are still read.
- The layouts whose slideshow changes one image at a time, as the menu says, are kept as
  `slideshow_one_image_at_a_time` instead of `layout_turns`. The old key is still read.
- Preferences has no checkbox for the window title "Glimt - image name" any more: the custom
  window title says the same with `%app% - %name%`. A settings file with `app_name_first=true`
  gets that title once, unless it has a title of its own.
- The light theme for low vision is called High Contrast Light instead of Contrast, so its name
  says what it is beside High Contrast. A settings file with `theme=Contrast` still gets it,
  without a problem told, and the next save writes the new name. A theme of your own named
  Contrast stays yours, and Save as Theme takes the name no more.
- The action Previous Page is named `PreviousPage` in `keys=` and `glimt --remote run`, beside
  `NextPage`, instead of `PrevPage`. The old name is still read.
- `glimt --remote` waits for the window's answer to every command, and exits with 3 when the
  command was not done: a `run` refused while a dialog is open or a text field has the focus,
  by a kiosk, or with no image to act on, or a command the window does not know, with the reason on standard error. Before, it exited
  with 0, as if it had been done. `--remote-all` exits with 3 when any window did not do it.
  A minimized or hidden window answers at once too, and shows what the command changed once it
  is shown again.
  A command line longer than 64 KB is answered as not done, "too long".
- The example formats and the manual write `[Glimt Format]` as the desktop file's header,
  after 0.75.0 renamed decoders to formats. Files with `[Glimt Decoder]` are read as before, and
  the manual says that the other `%` codes in `Exec` are kept for later.
- In the templates of the full screen info, the print caption, the page header and footer and
  the window title, `%%` is a `%`. Before, it was an empty field, which could take its part of
  the line along. In an added format's `Exec`, `%%f` is the text `%f`, not a `%` and the file.
  A single `%` in the page header or footer still leaves the page numbers after it filled in,
  as in `Scale 100% · page %page% of %pages%`.
- The marks file keeps what this Glimt does not understand: marks above 6, and lines of two
  fields and a path that it cannot read, stay as they are when a mark is changed, so a later
  Glimt can add to the file without an older one deleting it. Other damaged lines still go.

### Fixed

- A minimized window that is asked to close, from the taskbar or with `glimt --remote quit`,
  waits for the files being copied, moved or saved, as a shown window does, and closes once
  they are done. Before, it ended at once, halfway through the job, and the marks of the files
  already moved stayed on their old names.
- Import Lightroom Catalog drops a plan made for root folders that were changed while it was
  being made, so Import never writes onto the files of the folders given first. Next makes a
  plan for the folders as they are now.
- A file renamed or moved in Glimt, or a folder of them, now keeps its place in every saved
  collection, as in Geeqie, not only in the one open. Before, an entry stayed on the old name
  and could later show another file that took that name.
- Import Lightroom Catalog takes only the images that are there now. The stars of a photo
  deleted long ago no longer wait on its name for the next photo of that name, and a catalog
  from elsewhere can no longer have a sidecar written beside a file that is no image. It looks
  for them in the background, so the window does not stand still for a catalog on a share,
  and says how many it did not find, so that their drive can be mounted and the import done
  again.
- A JPEG whose EXIF orientation is not one of the eight values is turned from upright when its
  orientation is saved, as the view shows it. Before, a value of 9 or more was taken as 8, and
  the saved file showed another turn than the view.
- Marks and stars that another window set a moment before a move or delete in this one now
  follow the file or go with it too, also while this window's own marks are still being saved.
- Edit → Undo (Ctrl+Z) pressed while a delete to the trash or a move is still running says to
  try again in a moment, instead of undoing the operation before it.
- Copy Path puts a backslash before `$`, `` ` ``, `"` and `\` in the quoted path, so a file
  name such as `$(command).jpg` pasted into a terminal runs nothing. A `!` goes in single
  quotes, `"a"'!'"b.jpg"`, so bash and zsh do not put an earlier command line in its place.
- Rename and Rename Many refuse a new name that is another file's sidecar, such as `a.jpg.xmp`
  beside `a.jpg`, which would have made the renamed image that file's sidecar.
- A last folder whose name holds a line break is not remembered, rather than written into the
  window state as two lines. A keyword with a line break, which a sidecar can hold, is not put
  into the keyword tree, whose file would read it back as two.
- Two backups made at the same moment each get a folder of their own.
- A program Glimt runs for its answer is no longer signalled by its time limit after it has
  ended, when a program it left behind keeps its errors open.
- Pan view keeps the dates it has read when the list changes: after a rename, a refresh, a new
  sort or a shuffle it reads only the files that are new, rather than every file again, which
  took minutes in a large folder with EXIF date. A folder that a sync client keeps adding to
  now gets its whole timeline. A name that a rename, move, copy, delete or Undo in Glimt gave
  another file, and a file whose turn was saved, has its date read again, and F5 reads every
  date again.
- A network share that answers late is no longer opened over the folder you opened while
  waiting for it.
- The projector's keys wait while a dialog is open in the main window, as the main window's
  do: Space there no longer moves to the next image behind the Delete question.
- A start option after the file or folder is read as an option, as Geeqie reads its own:
  `glimt a.jpg --fullscreen` starts in full screen, rather than showing the current folder
  together with a.jpg's. Any other argument there is still a file or folder, and after `--`
  every argument is.
- An edit of the keywords field in the info panel changes only the keywords added or taken out
  there. Keywords that another window, a Lightroom import or a background job wrote to the
  shown image after the panel read it are kept, and so is a keyword that holds a comma. Such a
  keyword ("Smith, John") is taken out or changed whole when it is edited in the field.
- A keyword hierarchy that only the image holds, as a JPEG exported from Lightroom, goes whole
  into the sidecar that adding a keyword makes, so its box in the keyword tree stays ticked and
  other programs still read it as a hierarchy. Taking Oslo out of Places|Norway|Oslo keeps
  Places|Norway, instead of writing Norway as a keyword at the top of the tree.
- A keyword taken out or added in Glimt shows so in digiKam. digiKam reads its own keyword
  list (digiKam:TagsList) and Microsoft's before dc:subject, so Glimt now writes them again
  from the keyword tree it writes, also over the image's own. A hierarchy that only one of
  these lists holds is kept and written into each. An edit of the comment alone leaves every
  keyword list as it is, and MediaPro's catalog sets and ACDSee's categories stay. A keyword
  with a "/" in its name, which Lightroom allows, stays out of these two lists, where the "/"
  would be read as one more level. When one of them already holds it, as Music/AC/DC beside
  Lightroom's Music|AC/DC, it is read as that keyword, not as a new level DC under AC.
- A comment, alt text or extended description in several languages keeps them all when it is
  edited. The panel shows the default language (x-default), or else the first, and an edit
  replaces only that one; before, it showed the first and wrote it alone as the default.
- A control character in a comment or keyword, such as a form feed pasted from a PDF, or the
  noncharacter U+FFFE or U+FFFF, is left out of the sidecar. Written there, it made the sidecar unreadable for darktable and digiKam.
- A sidecar whose namespace is declared with spaces around the equals sign, or whose first
  description has a '>' in a value, keeps being XML when keywords or stars are written. An old
  packet's xap:Rating is changed instead of getting a second rating beside it.
- A line of the keyword tree file (`~/.config/glimt/keywords`) that is not UTF-8 keeps its
  bytes when the tree changes, and the rest of the tree shows at the start instead of none.
- An SVG made to take gigabytes of memory, or to keep drawing for hours, no longer ends Glimt
  or keeps a decoder busy for good. SVGs are drawn in a process of their own, which may take
  four times the picture it draws and 512 MB besides, and 30 seconds, and one that needs more
  is told as not shown. So that a photo linked or embedded in an SVG fits there, an SVG shows
  linked pictures of at most 32 megapixels each and 96 MB of files together, where it took
  256 MB of them before. The process also ends with Glimt, when Glimt quits or is killed while
  it draws, and at 30 seconds of processor time of its own. The system's fonts are read once
  for all of them, not again for each SVG with text.
- The images decoded at the same time (the view's, the thumbnails, Print and Check Images) take
  at most 2 GB together. A folder of images that each claim nearly the limit of one decode no
  longer takes gigabytes at once; such an image waits until there is room. The images take
  their turns, so a large one is not passed by smaller ones for good, and the shown image goes
  before the thumbnails and the images read ahead. Glimt's own logo in About takes nothing
  from them, so About no longer waits for a large image being decoded.
- An SVG shows only the pictures in its own folder or below it, as a browser shows an SVG used
  as an image, not a picture named by its whole path, through `..` or through a link, as an
  archive may unpack one. A link is not followed even to see where it leads, so a link to a
  network share does not mount it.
- A thumbnail in the shared cache that is larger than its size (128 or 256 pixels), as another
  program may write one, is made again rather than used at its size.
- A file whose thumbnail runs out of time twice (a video or SVG made to stall) is marked as
  having none, as a broken file is, instead of costing 30 seconds at every visit.
- Export Frames saves at most the first 1,000 frames of an animation, as many as the view keeps,
  so an animation of a million tiny frames is not hours of work and gigabytes of files. An ICO
  has at most as many sizes too.
- `glimt --check-config` on an account with no settings file yet says that the defaults are
  used and exits with 0, as Glimt starts without a problem then. It said that the file cannot be
  read and exited with 1. A FILE named on the command line that is not there still exits with 1.
- Glimt's remote control serves at most 16 clients at once, and reads a command other than
  `select` up to 64 KB, so a program that connects to its socket again and again, or sends
  without end, can no longer take a thread for each and 400 MB of memory for each line.

## [0.75.0] - 2026-11-08

### Added

- File → Copy for Sharing… copies the selected JPEG, PNG and WebP images into a folder you
  choose, without the metadata you leave out, to publish them. The dialog lists the tags the
  images have, in groups that can each be left out whole: place, camera and lens with their
  serial numbers, date and time, photographer and copyright, software, keywords, comment and
  alt text, and the preview inside the file, and More tags for the rest; Remove All leaves out
  every one. The pixels are copied byte for byte, so nothing is lost, and the orientation, the
  colour profile and the gain map of an Ultra HDR photo (or one marked as ISO 21496-1 says) are
  always kept, so a copy looks as the original.
  What Glimt's sidecar says, such as alt text, goes into the copy's own XMP. The dialog starts
  with the last choice. The originals are never changed, and a file already in the folder is
  never written over.

### Fixed

- `glimt --remote run ACTION` does nothing while a text field has the focus, as the keys do
  nothing then, so a script no longer takes the focus from the search field or a name being
  typed. The named commands, such as `next` and `open`, still act, as a remote control's
  buttons.
- A screen reader reads out a notice again when it is given again with the same text, such as
  a second refused action; before, it heard only the first.

### Changed

- Plugins are called tools now: Edit → Plugins is Edit → Tools, and their folder is
  ~/.local/share/glimt/tools (/usr/share/glimt/tools for the system). Decoder plugins, which
  let Glimt open more formats, are called formats, in ~/.local/share/glimt/formats. The old
  folders, plugins and decoders, still work and are always read; a desktop file of the same
  name in the new folder comes first. The desktop files keep their format. Edit → Tools →
  Open the Tools Folder opens the old folder while there is no new one.
- Added formats that take the same file ending are tried in the order of their names without
  ".desktop", so raw.desktop now comes before raw-dcraw.desktop, which came first before.
- The examples are in examples/tools and examples/formats (/usr/share/doc/glimt/examples/ in
  the packages), each desktop file named for the program it runs: describe.desktop runs
  glimt-describe, and the ImageMagick ones are dicom-imagemagick.desktop and
  fits-imagemagick.desktop. A copy of a renamed example next to the old copy shows twice in
  Edit → Tools: remove the old one.
- Inside, the check that the file list is drawn only by its known views also watches the
  functions that draw one row or cell of it, which a new view could call directly.
- Inside, the check that every modal dialog is in the one list of what is open compares the
  modal's name with whole names in that list, so a new modal named "confirm" no longer passes
  on `confirm_delete`, and it reads a call that is split over several lines.
- Inside, the check that keys are read in one place also sees the keys held down
  (`keys_down`).
- Inside, the check that every file operation is in the contract sees work that counts itself
  as writing however the counter is named, also after a `use`.
- Inside, the check that every action on the selection reaches its targets follows the files
  `targets()` gives back to where they are used, so an action that asks for them and then acts
  on the shown image fails it, and `asked_targets()` no longer counts as `targets()`.

## [0.74.0] - 2026-11-08

### Changed

- Names that say what they hold or do, before they are frozen in 1.0. Settings: `trash` is
  `delete_to_trash`, `delay` `slideshow_delay`, `random` `slideshow_random`, `own_profile`
  `use_image_profile`, `videos` `list_videos`, `sort_case` and `search_case`
  `sort_case_sensitive` and `search_case_sensitive`, and `exif_panel` `info_panel`. Actions in
  `keys=` and `--remote run`: `HideTools` is `HideFileList`, `Overlay` `FullScreenInfo`, `Play`
  `PlayVideo`, `Faster` and `Slower` `SlideshowFaster` and `SlideshowSlower`, `Keywords`
  `FocusKeywords`, `Copy` and `Move` `CopyTo` and `MoveTo`, `ViewFilter` `NextFilter`,
  `ExifPanel` `InfoPanel` and `Prev` `Previous`. `glimt --remote prev` is `glimt --remote
  previous`. The old names are still read, always; Glimt writes the new ones, and the first
  save takes the old keys out of the settings file. An older Glimt that shares the file then
  uses its defaults for those eight settings, and `--remote-all previous` does not reach a
  window of 0.73 or older, which still takes `prev`.
- The EXIF panel is the info panel (View → Show Info Panel, Ctrl+K), as it also holds the
  keywords, comment, alt text and marks.
- `--check-config` says when a settings file sets a setting under both its old and new name.
- Inside, about 450 names of the code were renamed so that a reader new to it knows what each
  is from its name, and a check keeps the old ones from coming back.

## [0.73.0] - 2026-11-07

### Fixed

- Print to File saved the PDF over an image when the name chosen was an image's, such as
  `a.jpg`; it now asks for a name ending in .pdf.
- A double-click in the side panel's file list with Ctrl or Shift held did not open the file
  clicked.

### Changed

- Checks that keep Glimt's rules in one place and fail when new code goes around them: the
  names frozen in 1.x (settings and their values, `--remote` commands, options and exit codes)
  are pinned in `src/frozen-1.0.txt`; clippy refuses the calls that delete or change a file
  outside Glimt's own ways; every file operation, also the trash, its Undo and a folder renamed,
  is in the file contract; every key read, modal and view of the file list has its one place;
  and a dialog about files opens only with the files it asks about.

## [0.72.0] - 2026-11-07

### Added

- `glimt --remote run ACTION` does an action by its name, as in `keys=`: zooming, browsing,
  selecting, the panels and windows, the settings. Only the actions that change no file,
  sidecar, mark or collection are taken, not even through a window the user then clicks in, and
  none while a dialog asks the user; `glimt --help` lists them, and another name exits with 2.
- A plugin whose desktop file has `MimeType=` (such as `image/x-canon-cr2;` or `image/*`) is
  given only the selected files of those types, and Glimt says how many it leaves out.

### Fixed

- With `XDG_DATA_DIRS` set but empty, the system's plugins and decoders were not found.

## [0.71.0] - 2026-11-07

### Added

- A screen reader such as Orca reads out every notice as it comes ("Copied 3 files", an error,
  an action the search stops), without moving the focus.

### Fixed

- The Lightroom import's screenshot showed only the main window.
- A test of the pointer in full screen that failed now and then.

### Changed

- The screenshots are the same in every run, so a changed one means a changed look.

## [0.70.0] - 2026-11-07

### Added

- When the search hides some of the selected files, the status bar counts them ("2 selected but
  hidden by the search"), and Delete's question says how many it leaves out. An action on the
  selection acts on the files the search shows, as before (#80).

### Fixed

- A test of `--remote-all` that failed now and then while another test started a process.

### Changed

- A new screenshot for the website: the thumbnails in the image area (G).

## [0.69.0] - 2026-11-07

The findings of a full audit of 0.68.0, fixed by class, and the hardening of #79.

### Fixed

- When the search hides every selected file, Delete, Move, Copy, Rename, Print, plugins, Open
  With, keywords, marks and collections no longer act on the shown image instead; they say the
  search hides the files. Find Duplicates' Move to Trash, Check Images' Select, New Collection,
  Rename Many and `--remote select` leave the selection as it was and say why when the search
  hides some of their files, where Find Duplicates wrongly said the files were gone.
  `--remote selection` prints nothing then.
- Undo of a rename leaves alone a file that has taken the new name since, and Undo of a move or
  rename no longer moves back another file at the name, also one the file system gave the same
  inode. A turn saved after a rename keeps it undoable, and an Undo that fails stays to be done
  again.
- Keywords, comment, alt text, stars and the keyword path written again into XMP replace the old
  value in every form it has, including a rating in single quotes, instead of writing it twice,
  which darktable and digiKam refuse as broken XML.
- A collection or list entry named through a link to a folder (such as /home on Fedora
  Silverblue) shows its marks and stars, is found by `--remote select`, and Remove and reordering
  work on it; the file keeps the user's spelling. Opening such a collection no longer jumps back
  to the first image, and renaming a folder moves its entries.
- With Include Subfolders, an image under a link to a folder inside the tree is listed by its
  real path, so `--remote select` finds it.
- A copy of a HEIC, AVIF, JPEG XL or RAW photo keeps the EXIF the info panel shows, and a copy of
  a WebP whose EXIF starts with "Exif" gets EXIF other programs read. A copy's EXIF no longer
  carries the source's pixel fields. A PNG or WebP turned keeps its whole EXIF block.
- The info panel, the date sort and copies read Exif and GPS directories whose pointers have the
  TIFF type IFD, as libtiff writes them.
- HEIC and AVIF photos (an iPhone's Display P3) are shown, thumbnailed and saved in their own
  colours. Thumbnails cached before keep their old colours until `~/.cache/thumbnails` is
  cleared.
- Every page of a multi-page TIFF keeps its own colour profile, pages after the first of a 1-bit
  TIFF such as a fax are shown, and Export Frames writes float pages as 16-bit PNG.
- Save This Frame and Export Frames save the frames turned as the view shows them.
- Photos whose EXIF date is written with dashes sort among the others and share their day in Pan
  view.
- A cached thumbnail is used only when its size matches the file, so a burst renumbered in the
  same second gets its own thumbnails.
- Two names count as one file only when their size and time agree too; on a share without inodes
  that ignores case, Save As to the same name in other case is refused instead of writing over the
  image.
- A search with thousands of nested `(` no longer crashes Glimt; groups nest up to 32 deep.
- `--remote-all` no longer removes the socket of a Glimt that is just starting.
- Keywords and Comment (Ctrl+K) turns the EXIF panel on as the menu does.

### Changed

- Every action name, also one without a default key, is pinned for `keys=`.
- The release tests clean old build folders first, stop when Docker has little space left, and
  remove their container and image however they end.
- New screenshots for the website and the README, with the View menu's new items.

## [0.68.0] - 2026-11-06

### Added

- View → Thumbnail Size chooses Small, Medium or Large thumbnails from the window, not only in
  Preferences.
- View → Show File Names Under Thumbnails (also in Preferences) turns off the names in the grids
  of thumbnails, so more rows fit; a name still shows when the pointer rests on it and is still
  read by a screen reader.

### Fixed

- On a share that gives every file inode 0 (sshfs with `-o use_ino`), Find Duplicates no longer
  keeps one file of the folder, the backup no longer takes every folder for one it copies, Save
  As and Rename Many no longer take two files for one, and Undo of a move no longer moves back
  another file put at the moved name since.
- With Include Subfolders, a folder link whose real path cannot be worked out is not followed.
- An animated GIF, WebP or APNG that claims a huge size gets no frames made.
- A file operation refused after a folder's listing broke says the listing stopped, instead of
  "try again in a moment".
- A turn saved while another window renames the photo keeps the photo and its sidecar in step.
- A collection row dragged from outside the selection lets go of the images a search held back.
- Pan view lays its rows out again when the thumbnail size or the file names under them change.
- Changing the file list's settings in Preferences keeps the shown image in view, as the menus
  do.

## [0.67.0] - 2026-11-06

The findings of a full audit of 0.66.0, fixed by class, each in one place with a check that
fails when new code opens the class again.

### Added

- `--` on the command line ends the options, so a file named like an option (`-h`, `--x.jpg`)
  opens: `glimt -- -h`.
- Diagrams of how Glimt is built in `docs/architecture/`: the code's layout, a frame and its
  threads, from a folder to pixels, a change on disk, and the checks.

### Fixed

- A search no longer loses the selection while a query is typed (`re:`, `exif:` while its index
  is read, an unfinished pattern, a typo). The files a search hides are held back, never acted
  on, and selected again when the search shows them.
- Moving or removing images in a collection right after it opens is no longer undone when its
  listing finishes, and an image taken out of a plain `.txt` or `.m3u` list stays out.
- With Include Subfolders, a tree on sshfs mounted with `-o use_ino` (every inode 0) is listed
  whole, not empty.
- Several folders opened at once list their files by their real paths, so marks, stars and
  `--remote select` work as when one folder is opened, and a folder named twice is listed once.
- Undo of a delete finds a file named through a link to another disk in that disk's trash.
- Copy To, Move To and sort folders follow the destination's links before they start, so a
  move refused by a FAT card behind a link keeps the files in the list, and a destination
  written with `..` is no longer refused wrongly (#77).
- Esc that leaves a text field, such as the folder path or the search, no longer also leaves
  full screen, stops the slideshow or goes back to the grid, and the first key after closing a
  name prompt with Esc is no longer lost.
- The file list scrolls to the image shown after full screen or Hide File List.
- A PNG cropped or saved as a copy after a quarter turn keeps its DPI across and down the right
  way round, so a 204×98 dpi scan no longer prints stretched.
- Crop and Save As of a 32-bit float TIFF or JPEG XL as PNG works; it failed before.
- A copy keeps the image's colour profile only when it fits the pixels written: a grey JPEG
  stays grey with its profile, and a CMYK JPEG's profile is no longer put on RGB pixels.
- An image is shown turned by the orientation the info panel reads, in every format: a PNG with
  it only in its EXIF text chunk, and a WebP whose EXIF chunk starts with "Exif\0\0". A PNG with
  Adobe RGB only in that text is shown as Adobe RGB. Thumbnails cached before stay as they were
  until `~/.cache/thumbnails` is cleared.
- Leaving thumbnail and pixel-size properties out of a derived file's XMP is linear on hostile
  packets and never cuts through an XML comment (#78).
- A size just under a megabyte or gigabyte reads "1.0 MB" or "1.0 GB", not "1000.0 kB".

### Changed

- With Include Subfolders, a link to a folder is followed only when it leads inside the folder
  opened, so a downloaded folder cannot list your other folders for Select All and Delete. A
  link from the tree to another drive or a NAS is no longer listed; open that folder itself.
  The Help Manual and Differences from Geeqie say so.
- A part chosen for cropping is never saved while the grid of thumbnails covers the image.
- Sorting by EXIF date and searching read each PNG once.
- The README keeps building from source and what the website does not have; the describe
  plugin and the Help Manual point to glimt.pario.no.

## [0.66.1] - 2026-11-06

### Fixed

- Enter on a focused Cancel in the questions before Delete, Delete Permanently, a folder or a
  collection to the trash no longer deletes. Enter and Esc in dialogs, the crop, the pan view and
  Check Images answer only when no button or text field has the keyboard focus, never as a key
  held down repeats, and never with Ctrl or Alt.
- Esc or Q back to the grid in the image area ends a crop of the image it leaves; the next
  Enter in the grid saved the part as a copy.
- A PNG whose EXIF is kept in a "Raw profile type exif" text chunk, as digiKam, GIMP and older
  ImageMagick write it, keeps its date taken, camera and GPS when turned, cropped or saved as a
  copy; the info panel and sorting by date read it too.

## [0.66.0] - 2026-11-06

A release of fixes from a full audit of 0.65.0, each class of finding fixed in one place, with a
test or a check of the source that fails when new code opens the class again.

### Fixed

- Saving a turn, a sidecar, a collection, Save As or Print to File no longer replaces a
  read-only file; it says "the file is read-only", as Geeqie refuses it. A turn refused that
  way, or on a full disk, leaves the photo's sidecar as it was, and a sidecar that cannot be
  written is named in the error.
- Saving a rotation also sets the orientation in the photo's `.xmp` sidecar, so digiKam shows
  it turned the same way.
- A crop or Save As copy no longer keeps a thumbnail of the whole photo in its XMP, its sidecar
  or a PNG's Photoshop resources, nor Lightroom's crop or darktable's history, which made those
  programs crop it again. Its sidecar's orientation and pixel sizes match the copy. A photo
  turned in place keeps its own edits.
- Thumbnails in the shared cache, prints, Copy Image and exported frames are in the image's own
  colours whatever the colour settings are. Colour management off, or a profile chosen for
  images without one, changed what other programs got.
- A very large JPEG zoomed in on a large screen is refused instead of being decoded at half
  size into gigabytes of memory, as the other formats already were, and so is a TIFF page that
  claims more than the limit.
- Thumbnails and the print of a turned photo shrink it before turning it, so a large TIFF with
  an EXIF orientation no longer holds two full copies at once.
- Copy Image in the file list's right-click menu copied the shown image, not the file
  right-clicked; it is no longer in that menu (Edit → Copy Image copies the shown image). Open
  With there lists the applications for the selected files, which it opens, not for the shown
  image.
- A Ctrl+ or Shift+double-click in the grid in the image area opens the image clicked, not the
  one shown before.
- With Thumbnails and File Details on, the grid in the image area no longer draws thumbnails
  over the side panel's names.
- View → Thumbnails in Image Area is no longer ticked on an image opened from the grid, where
  choosing it goes back to the grid.
- A screen reader hears the grid in the image area as it hears the image, and the names of the
  grid's cells, the filmstrip, the file list's rows with thumbnails or details, the pan view's
  days, the panes of a layout, the projector's image and a video's play button.
- Glimt no longer crashes when the projector is open on an empty folder, or when the last image
  is deleted while it is open.
- Rename, Rename Folder and Rename Collection answered with the name unchanged no longer rename
  a file whose name is not UTF-8 or starts with a space.
- A slideshow that changes one pane at a time goes on from its last image after the list is
  read again, instead of starting from the top.
- Undo from the trash puts back every file it can, also after one that cannot come back.
- Find Duplicates no longer counts one file reached through a bind mount as its own duplicate,
  and Include Subfolders does not read a folder mounted inside itself again.
- The backup copies a settings, themes or collections folder that is a link (such as one kept
  in a dotfiles repository), and never a folder into itself.
- A folder opened while a collection was still being checked allows file operations at once,
  and a list whose listing broke allows none until it is read whole.

### Changed

- Find Duplicates takes hard links of one file as one file: deleting one frees no space. The
  Help Manual and Differences from Geeqie say so.
- Thumbnails show each image in its own colours, not with the profile chosen for images without
  one; the Help Manual says so.
- AGENTS.md: findings are fixed by their class, at the root, right the first time; an action
  names what it acts on; a new way to show the list has a checklist.

## [0.65.1] - 2026-11-06

### Fixed

- A lossy WebP turned and saved keeps its ICC profile and its colours. Its pixels went to sRGB
  through the view's colour settings, so with colour management off a Display P3 or Adobe RGB
  photo lost its profile and its colours for good, and with an input profile for images
  without one each save shifted them further.
- A list or collection keeps only files. A folder named in a downloaded `.gqv`, `.m3u` or
  `.txt` was an entry, shown as a broken image, and deleting it moved the whole folder to the
  trash. Until a list just opened has been checked, no file operation starts.

## [0.65.0] - 2026-11-06

### Added

- View → Thumbnails in Image Area (G) shows the files as a grid of thumbnails where the image
  is. The arrow keys move in it, ↑ and ↓ a row; Enter or a double-click shows the image there,
  and Esc or G goes back to the grid. A slideshow started from it shows the images. While it is
  on, the side panel lists the names. Geeqie has no such view.
- The interface in Brazilian Portuguese, Czech and Norwegian Nynorsk: English and fourteen more
  languages now.
- `po/glimt.pot`, a template for a new translation, written by `tools/pot.py` and kept in step
  by a test; CONTRIBUTING.md says how to send one.

### Changed

- Help → About's licence heading in British English, as the rest of the interface.
- A locale given as a translation's own code, such as `LANGUAGE=no_nn`, picks that translation
  before one of the same language.

## [0.64.0] - 2026-11-06

### Fixed

- Copy To, Move To and the sort folders' buttons onto a camera card reached through a link
  (`~/card` → `/media/u/CARD`) say which characters the card refuses and copy or move nothing,
  as they do for the card's own folder, also with a sort folder saved before. The link is
  followed in the job's background thread before the first file, and the folder is kept and
  shown as it was picked.
- A Copy To or Move To of many files with a name a camera card refuses stops at the first such
  name, instead of first making a path of every one in the window's thread.

### Changed

- `tools/check.sh --clean` removes the build folders of checkouts that are gone, which had
  filled the disk of the release tests' Docker.
- Dependabot leaves zbus at 5 until AccessKit takes zbus 6, so the full build has one copy.

## [0.63.0] - 2026-11-06

### Fixed

- Copy To, Move To and the sort folders' buttons onto a camera card (FAT or exFAT) say which
  characters it refuses (`: ? * " < > | \`) before copying or moving anything, instead of
  copying the other files and failing on that one with "Invalid argument".
- Renaming a folder on a camera card to a name with one of those characters says so before
  trying, instead of giving only "Invalid argument".
- Rename Many's preview of the first new name warns when a camera card would refuse it.
- A folder mounted at a name that is not UTF-8 is known by its own file system, so a camera
  card or share there gets the same plain messages and questions as any other.
- A folder mounted over another mount that was there before it, such as a share mounted at
  `/mnt` after a card at `/mnt/card`, is known by the file system that is on top.
- Rename and Rename Many on an exFAT card mounted through exfat-fuse (`fuseblk`) say which
  characters it refuses, as on a card the kernel mounts, when udev knows the card's file system.
- An Enter held down no longer answers the question to delete for good what the trash refused:
  only a new press of Enter does.
- Files the trash refused on a network or cloud folder, when another folder was opened before
  the question to delete them for good, are named in a notice that says they are still there,
  instead of being dropped without a word.
- The question to delete for good what the trash refused opens also in a folder that keeps
  changing, such as a sync folder taking in uploads with auto refresh on. It waited for the
  folder to be still before.
- A file the trash refused twice is counted once in the question to delete it for good.

## [0.62.0] - 2026-11-06

### Fixed

- The Help Manual's table of differences from Geeqie says English and eleven more languages,
  not ten.
- The example describe plugin makes its change again when Glimt writes the sidecar at the moment
  the plugin saves it, instead of writing over what Glimt wrote.

### Changed

- Delete to the trash on a network or cloud folder (an rclone or s3fs mount, smb:// and sftp://
  folders, cifs, NFS), where gio has no trash, asks whether to delete the files for good
  instead of only showing gio's error.
- A rename that a share, cloud folder or phone refuses names the type of its mount
  (`fuse.rclone`, `fuse.gvfsd-fuse`, `cifs` …).
- Rename and Rename Many on a camera card (FAT or exFAT) say which characters it refuses
  (`: ? * " < > | \`) before renaming anything, instead of failing halfway with "Invalid
  argument".
- The Help Manual and README tell what to expect of network and cloud folders: slow the first
  time, F5 for changes made from another computer, no trash, and the limits above.
- The release tests rename, move, tag, delete and Rename Many on an rclone mount and in an
  sftp:// folder that Glimt mounts with gio.

## [0.61.0] - 2026-11-06

### Fixed

- A copy saved with Save As, Crop or Save This Frame to a slow share no longer holds up the
  keywords, comments and stars written to other files meanwhile.
- A change in Preferences no longer waits for an image with a large colour profile being decoded,
  nor makes every other image being decoded wait behind it.
- Printing and Open With in the Flatpak end with a message when the desktop portal restarts
  while its dialog is open, instead of waiting 30 minutes for an answer that never comes.

### Changed

- The example plugins open an image's XMP sidecar without following a link and give exiftool what
  they read from the open file on its standard input, so a link put in its place after a check is
  never read or written through. The describe plugin reads the sidecar again after the model has
  answered, so a keyword or stars Glimt wrote meanwhile are kept.
- Every crate is at its newest compatible version, and a weekly pull request keeps them there:
  `cargo update` with the files that follow `Cargo.lock`, checked by CI before it is merged.
- `tools/check.sh` builds its image again when `rust:1` moves on, and its turns are kernel locks
  that a killed check lets go of at once.
- The release tests rename on a real FAT file system: a change of case and a swap.

## [0.60.0] - 2026-11-06

### Fixed

- Undo of a move no longer moves back another image saved over the moved file with Save As or
  Save This Frame, and still moves back a file whose turn was saved, also through a folder opened
  by a link.
- Undo of a move works on FAT, exFAT and FUSE file systems, whose files can get a new inode number
  between the move and the Undo.
- Undo from the trash puts a photo back beside its own sidecar when the delete could not trash the
  sidecar, instead of refusing.
- A collection whose file is a link is no longer written through: the link is replaced by the
  collection, and the file it pointed to stays as it was, with none of its text copied in.
- Save Every Frame always saves into a folder made new for it, never into one another export just
  made.
- A copy saved with Save As, Crop or Save This Frame waits while another write holds its sidecar.
- A sidecar that is swapped for a link at the moment it is read is not read through.
- Trash by hand in the Flatpak takes a file whose name is 246 bytes or longer.
- Rename keeps a hard link whose name differs only in case from the file's on ext4, instead of
  taking it for the same name.
- A portal request (printing, Open With in the Flatpak) takes its answer only from the portal
  itself, and one that gets no answer no longer leaves a thread waiting.
- Find Duplicates: Stop leaves at once, also while a large file is read on a slow share, and
  Similarity finds every match in a block and finds left-out matches again on all cores.
- A slow or crafted colour profile no longer holds up the other images being decoded, and the
  info panel's colour profile row no longer holds up Preferences.
- A thumbnail of a file changed while its thumbnail was being made shows the new contents,
  whichever worker answers first.
- Open With asks again after an empty answer or a failed lookup, instead of showing no
  applications for that type until Glimt is closed.
- A collection is opened with one system call per entry instead of two.
- Clean Thumbnails also removes the failure markers older versions of Glimt left.
- A rename that a share, cloud folder or phone cannot do is told in plain words.

### Changed

- The translations are in `po/` instead of `src/`.
- The checks before a commit build with `--profile quick`, and `tools/check.sh` runs them in
  Docker with mold and sccache: a small change is checked in seconds instead of minutes. CI runs
  on a push to any branch.
- Every checkbox is drawn through a helper that names it for the release tests, and clippy refuses
  egui's own checkbox and the other widgets that would draw an unnamed clickable text.
- The release tests' search-panel check fails when a screenshot is missing, and the list-width
  checks first see the image where it must be.

## [0.59.0] - 2026-11-05

### Fixed

- Undo of a move leaves a file alone that another program put where the moved file was, and tells
  so, instead of moving that file back under the old name.
- Undo from the trash no longer puts a photo back where a sidecar of its name has appeared since
  (written by another program, or synced in), which gave the photo that sidecar's keywords and
  left its own in the trash.
- Putting Rename Many right after a crash keeps its plan when a file cannot be looked at (a share
  that answers with an I/O error), instead of deleting the plan while files still have hidden
  names.
- Printing and Open With in the Flatpak stop waiting for a desktop portal that never answers after
  30 minutes, so the window can close.
- An empty or relative `$XDG_RUNTIME_DIR` is ignored, so the remote-control sockets and sidecar
  locks no longer go into `./glimt` of the current folder.
- Renaming a file, moving one file, Save As and Crop no longer look at the disk on the window's
  thread to tell whether a folder was renamed, which could hang the window on a share that stopped
  answering.
- The info panel waits only for the sidecar writes of the shown file before reading it again: a
  long job on other files no longer holds back a plugin's new description, and an image shown
  again while its own save is still queued is read once that save is written.

### Changed

- The release tests run a commit from before 0.58.0 again, and each section starts in the same
  working folder and without the variables an earlier section set.

## [0.58.0] - 2026-11-05

### Fixed

- A video or plugin-decoded file whose thumbnail timed out on a slow share, or failed with an I/O
  error, is tried again later instead of keeping the no-thumbnail icon for good.
- With a screen reader, an image a search hides is read as not among the matches, not as a dash of
  the number of matches. Its place is read in the chosen language.
- A Preferences search for an action that is not built in no longer lists every keyboard shortcut.
- Preferences → Keyboard shortcuts can give an action Ctrl+C or Ctrl+X, which before left the row
  waiting for a key. A key given to Ctrl+X runs, and the keyboard's own Copy key runs Copy.
- With the Pan view open, a delete or a move out of the list no longer makes it read the date of
  every file again.
- Deleting images while the Pan view still reads the days of a large folder no longer starts the
  reading over.
- Deleting or moving other files no longer resets the shown image's zoom and turn, nor saves its
  turn early with Save Orientation on.
- Next on the last image, Previous on the first, a click on the shown image's own row or pane, a
  slideshow of one image, opening the file already shown and turning Random order on or off no
  longer reset the image's zoom, page and turn, or write the turn into the file at once.
- With Compare on, deleting or moving other files no longer throws away the shown image's decode,
  which made it be decoded again.
- With Compare on and a search, a delete or a refresh no longer blanks the compared image until it
  is decoded again.
- A Lightroom catalog sqlite3 cannot read is told by its reason alone, without part of the query.
- Keywords added to many selected files show in the info panel at once. Before, the panel could read
  the shown file before its keywords were written, and a later edit there took them off again.
- After a plugin changes the shown photo (describing it, say), the info panel shows its new
  keywords, comment and alt text, and the screen reader reads them, not only after another photo and
  back. Before, an edit in the panel could save the old ones over the plugin's.
- A plugin that starts a program in the background (an editor, xdg-open) no longer counts as
  running, holding its files, until that program closes.
- A delete, move or rename of files a plugin is still working on waits until the plugin ends, as for
  every other file operation.
- The delete, Rename Many and New Collection dialogs keep naming the files they asked about when the
  list changes while they are open, and New Collection adds those files, not the selection of the
  moment.
- New Folder and Rename Collection act on the folder or collection they asked about, also when a
  script opened another one while the dialog was open.
- A window opened just as another one finished a Rename Many no longer offers to put right renames
  that were already done.
- Printing a contact sheet of slow images (videos, HEIC, RAWs on a share) to a printer is no longer
  cut off as "No answer from the print server".
- Finishing a Rename Many of many files cut short by a crash no longer takes minutes.
- clippy's message for Command::new names host_command for the user's own programs, which
  external::command cannot reach in a Flatpak.
- The release tests fail when NEW does not show a text they click (a renamed label or a build
  without the test hooks), instead of carrying on with a note, and NEW is never built without the
  test hooks.
- Three release checks (a collection moved to the trash, a copy closed twice, a long error) passed
  when their action never ran; each now needs to see the action happen.
- run.sh and the screenshots' run.sh take tests.sh, samples.txt and shots.sh from the commit they
  test, not from the working tree.
- The release tests' kiosk, --lock-settings, Crop and printing checks that expect nothing to happen
  each show in their own section that the keys reach the window.
- The release tests' summary counted no failure of cargo audit, so an audit failing alone ended in
  "0 checks failed".
- The release workflow built the Flatpak in a folder inside the source the manifest copies.
- The weekly run with five million broken inputs skipped two tests that fed them, as their names
  lacked survives_broken; a source test now keeps every such test named so.
- A source test checks that each name in a row of AGENTS.md's code layout is defined in that row's
  file, so a function moved to another module takes its name to that module's row.
- Enter in a text field, such as the toolbar's folder path, no longer saves a crop being chosen.
- Open With and the video player no longer keep a thread and everything the opened application
  prints on its errors for as long as it stays open.
- A video player or Open With program that fails without printing why now shows its exit status,
  where it showed an empty notice.
- A damaged video or HEIC shows ffmpeg's last error line, not up to 64 KB of its errors over the
  image and in Check Images.
- Find Duplicates by dimensions groups camera RAW files by the size of the photo, not of the small
  thumbnail in their header, which put a whole folder of NEFs in one group, and it reads only the
  first part of a RAW where it can; %dimensions% in a print caption shows the photo's size too.
- Save This Frame and Save as PDF suggest a file beside the image, also in a collection, several
  folders or with subfolders, where they suggested one in / or in the top folder.
- Choosing a key in Preferences ends when the Keyboard shortcuts header or Preferences is closed.
  Before, Esc stopped closing Preferences, and a key pressed later in another field became the
  action's key.
- The notice after a keyword is set on or taken off several files is one sentence each language can
  word in its own order, and the EXIF window's title is translated.
- Alt+Right in Two Above Each Other (and Alt+Down in Two Side by Side) no longer goes to the other
  pane as the other arrow does.
- In a build without video, Enter no longer runs Play Video, which did nothing there.
- A kiosk whose quit is held back no longer lets every other action through.
- The info panel names a JPEG XL's own colour profile instead of saying it is taken as sRGB.
- Open With in the Flatpak no longer hangs on a pipe named in a collection or on the command line.
- An animated GIF cut short in a download shows the frames that came, not only its first frame as a
  still.
- An SVG that claims a huge size no longer takes hundreds of megabytes: it is drawn at the size it
  is shown.
- Add to Collection no longer holds up the window while it writes a large collection.
- Esc in a dialog shown over Preferences (a share's login, an interrupted Rename Many, a folder
  delete) closes only that dialog, not Preferences with it.
- In the Flatpak, Undo of a move to the trash finds the host's trash also where the host has its own
  XDG_DATA_HOME.
- A message from work in the background (a wallpaper set, Open With, the video player, a missing
  file dialog) shows at once, not only after the mouse moves.
- A file renamed or moved in a collection, or renamed in place, leaves the search's matches it no
  longer fits and joins those it now fits.
- The EXIF panel, the sort menu, the Types menu, theme and filter names, the CPU and memory levels,
  several Preferences texts, dialogs and notices now show in the chosen language; a test fails on a
  new text left untranslated.

### Changed

- A test checks that the man page has an entry for every start option and remote command.
- A test checks that `full` holds every optional feature and that README's feature table lists each.
- A test checks that the .deb, .rpm, tar.gz, AppImage and Flatpak install the same files.
- CONTRIBUTING.md and the pull request template point at AGENTS.md's checks instead of an incomplete
  copy.
- CI, the release workflow and the release tests no longer install graphics development packages,
  which the build does not need.
- Moving files to the trash has one function, trash::trash, so no caller can skip the Flatpak's
  fallback for a file the portal refuses.
- Opening a file of the folder already shown (File → Open File, a drop, --remote open, Back) goes to
  it at once, without reading the folder, its thumbnails and its EXIF index again.
- The toolbar's Refresh and Show Thumbnails buttons show the same icons as their menu items.
- Temporary files left by a crash are hidden (.glimt-tmp-*), so a file manager no longer shows them.
- Counts and sizes are written as each language writes numbers (1.200 Dateien, 2,3 MB, 1 200 filer),
  no longer in the English form in every translation.
- Move Collection to Trash asks before the collection's file goes to the trash, as Move Folder to
  Trash does.
- The release tests click file rows, the print dialog's page text and the toolbar by their text
  instead of at fixed places.
- Every button, selectable row and radio button is drawn through a menus helper (button,
  icon_button, button_enabled, small_button, selectable, choice, radio, radio_choice) that names it
  for the release tests, and clippy refuses egui's own elsewhere, so the folder list's rows, Check
  Images' broken files and the choices in Preferences, Print and Save As can be clicked by their
  text too.
- A whole selection is set through set_selection, which also tells what follows it: the title, the
  range's end and the size in the status bar.
- Whether Glimt runs in a Flatpak is asked one way, external::in_flatpak, also in a build without
  the portals, and the host's folders one way, config::host_xdg_dir.
- An operation edits the open collection only through collections::edit_told, which keeps the
  plain-list rule and the message for a collection gone.
- Every sidecar is read through xmp::own_sidecar, which skips a link.
- The user's own keys and the animation limit have one setter each, and a source test keeps every
  process-wide copy of a setting written only by its setter.
- Actions that need a file are refused once, at the top of App::run, and a test runs every action in
  an empty folder.
- A decoder made directly is decoded only through decode::from_decoder, after the size limit, and
  clippy refuses the image crate's own from_decoder.
- A panic in a worker is caught in one place, threads::caught, and a source test refuses a second
  way.
- SVG has a module of its own (svg.rs), with its limits and their tests.
- The release tests start each section clean (section()): no stubs or homes left by a section
  before, whichever part run.sh starts it in.

## [0.57.0] - 2026-11-05

### Fixed

These come from the review on the unreleased branch `readability-0.53`, each ported to the
layout of 0.56: the fixes where a file, a sidecar, a mark or a setting could be lost or end up
on the wrong file.

- Save As over a file no longer loses that file's sidecar when the image cannot be written, for
  example on a full disk.
- Save As or Save This Frame over a file whose sidecar cannot be removed leaves that file as it
  was, no longer with the copy's pixels under its old keywords and stars.
- Save As and Crop give the copy the marks and stars of the original, matching the sidecar it
  gets, as Copy does.
- Save As refuses to save over another name (a hard link) of the image itself, as it refuses the
  image's own name.
- Save As to JPEG of an image with more than 64 KB of embedded XMP (a long Photoshop or darktable
  history) makes the copy without that XMP instead of failing.
- Undo of a delete puts back every file it can when one of them is no longer in the trash or its
  name was taken, instead of none, and says how many came back.
- An Undo of a move to the trash that was refused (a new file had taken a name) or stopped partway
  no longer throws away the stars, marks and collection places of the files not put back; Ctrl+Z
  tries them again. Those never replace the Undo of a delete made while it ran.
- Undo of a move to the trash no longer leaves a restored photo's sidecar in the trash when a
  later file cannot be put back.
- Undo of a move to the trash finds a file whose name is not UTF-8, such as one from an old
  Latin-1 archive.
- Undo of a rename no longer crashes Glimt when an empty folder was opened after the rename.
- With Save Orientation on, a turn of the image shown when Glimt closes is saved, also one made
  after the first close while Glimt waits for file operations.
- An own key that this build cannot read, written by a newer Glimt, is no longer lost when keys
  are changed in Preferences, nor one for an action this build does not have.
- Renaming a file to the same name in other letter case (IMG_0001.JPG to img_0001.jpg) works on a
  camera card (FAT, exFAT), where Glimt said the name existed already. A rename onto another name
  of the same file (a hard link) that differs by more than letter case is refused, so that name
  stays.
- A delete tells of a sidecar it could not remove with its photo, instead of leaving it behind
  without a word.
- A sidecar that is a link pointing nowhere goes with its photo when it is renamed, as it does when
  the photo is moved or deleted, and such a link at a new name counts as taken.
- A file renamed, moved or deleted in Glimt takes its thumbnail, size, date, Check Images' mark and
  exif: index entry with it: a file given its name afterwards (Rename Many swapping two names, a
  rename to the name of one deleted) showed the old file's thumbnail and matched searches by the
  old file's EXIF.
- A copy is held back while Rename Many, a move or a delete is still working on its files, and
  says to try again: it could copy one photo with another photo's sidecar while Rename Many
  swapped their names. A copy now holds its own files too while it runs, so a turn of one of them
  is not saved, and a rename or a delete of them waits, until the copy is done.
- A collection opened while another one's sort was still being read keeps its own order: the sort
  reached it and rewrote its file in that order.
- The describe example plugin keeps the keywords and description an image holds in its own XMP: a
  new sidecar starts with them.

## [0.56.0] - 2026-11-04

### Security

These come from a review on the branch `readability-0.53`, which was never released. Each is
ported to the layout of 0.55.

- A sidecar that is a link is no longer read: a downloaded photo.jpg.xmp pointing at another XMP
  file of yours showed that file's keywords, comment and alt text for the photo in the info
  panel and found the photo by them in the search.
- Save As and Save This Frame over a file that is a link replace the link, no longer the file it
  points to.
- Print to File over a link, such as one in a downloaded folder, replaces the link and no longer
  writes the PDF over the file it points to.
- A JPEG larger than 512 MB (a sparse file in a download), whatever its name says, is refused,
  not read whole into memory by the decoder until Glimt is stopped. A large TIFF or PNG, which is
  read by seeking, still opens (issue #48).
- A JPEG XL file is decoded within a memory limit of 2 GB, enough for a camera's photo, where
  jxl-oxide had none.
- A crafted image whose EXIF names one large part from many text fields can no longer make the
  info panel or a print caption take gigabytes and crash Glimt: each field's text is read up to 4
  KB.
- Plugins and decoders are no longer read from the current folder when an XDG data variable is
  empty or holds a relative path.
- An SVG's linked picture is decoded from the bytes Glimt's limits checked; usvg read the file a
  second time, so a file swapped in between got past the limits (issue #38).
- The example plugins (Describe and Read Aloud) leave a sidecar that is a link alone, as Glimt
  does (issue #46).

### Fixed

- A delete no longer takes a link whose target is gone for a file it deleted, which removed the
  link's own sidecar and its entry in the collection when the delete had failed (issue #35).
- A permanent delete takes the photo's sidecar along also when the sidecar is a link that points
  nowhere, instead of leaving it behind under the name.
- The release tests no longer stop for hours when a window cannot start: every `--remote` call
  ends after 30 seconds, and Xvfb runs with `-noreset`, so a window started just as the last one
  closed is not refused. A check shows a lossy 24 MP JPEG XL within its new memory limit.

## [0.55.0] - 2026-11-04

### Changed

- Every part of the window keeps its state to itself now; Glimt itself works as before. What
  is open (`Folder`: the folder or collection, its listing, the way back, the login to a
  share) and the list with everything that holds a place in it (`List`: the selection, the
  decoded images, the panes, a drag, the search matches) have private fields, as do the
  desktop's look, Open With, the remote control listener, Check Images and the Lightroom
  import. The windows of pan view, Find Duplicates, Save As and Print, whose fields were
  private already, are now held to it by the test too. The rest of the program changes
  them through methods named for what happens, such as `list.select_one(i)`,
  `list.places_changed()` or `folder.relist_later()`. The test that keeps a part's fields
  private covers all of them.

## [0.54.0] - 2026-11-04

### Changed

- The window's parts keep their state to themselves; Glimt itself works as before. Each of
  the 14 parts of `App` (the slideshow, the zoom, the file operations, the search, the
  dialogs …) has private fields, and the rest of the program calls methods named for what
  happens, such as `slideshow.toggle_pause()` or `zoom.fit()`. A rule that belongs to a part,
  such as when the selection's size is worked out or what the kiosk allows, is now in that
  part. What belongs to the image shown (`Viewing`) moved to `viewer/view.rs`, beside the code
  that uses it. A new test fails when a part's field is made public again.
- The comments of the fourteen files with the most in the older clipped style are rewritten
  in plain sentences, with "because", "when" and "so" written out. Six doc comments that stood
  above the wrong item are back above their own. AGENTS.md lists where the older style is
  left.
- The layout test of imports no longer counts a commented-out `#[macro_export]`.

## [0.53.0] - 2026-11-03

### Changed

- The source is in folders by what it does; Glimt itself works as before. `src/` holds only
  `main.rs`, `cli.rs` and eight folders, in the order a reader meets the program: `model/`
  (what is open), `viewer/` (the image area), `ui/` (the window around it), `features/`,
  `images/` (from file to pixels and back), `metadata/`, `files/` (how files change on disk)
  and `support/`. AGENTS.md lists the modules folder by folder.
- `main.rs` is down from about 1,200 lines to 560. The command line is in `cli.rs`, the list of
  files in `model/list.rs`, the trash in `files/trash.rs`, the safe writes and renames in
  `files/file_io.rs`, `--back-up` in `files/backup.rs`, and the test helpers in modules of
  their own.
- A new test keeps the layout: a module file directly in `src/`, or a folder's file with more
  than its `mod` lines, fails it.

## [0.52.0] - 2026-11-02

### Changed

- The source reads more like a reader expects, and stays that way; Glimt itself works as
  before. The window's state, 170 fields side by side in `App`, is in structs of the modules
  that use it (`self.slideshow`, `self.folder`, `self.work`, `self.thumbs` …), each with its
  own constructor. Every file has its items in one order (imports, constants, types, the
  `App` block, functions, tests), tests sit beside the code they test, and functions over 120
  lines are split by what their parts do. Imports name the module that defines them, and three
  modules have names that say what they hold: `listing.rs`, `xmp.rs` and `decoder_plugins.rs`.
- Tests in `src/source_rules.rs` keep the layout AGENTS.md describes: a file out of order, a
  test in main.rs that belongs elsewhere, a field more in `App`, a function too long, an
  import through main.rs, a module missing from AGENTS.md or a comment too wide fails them,
  so the layout cannot drift back unnoticed. They read the code with comments and strings
  blanked, so neither is taken for code.

## [0.51.0] - 2026-11-02

### Changed

- The source is organised for whoever reads it next; Glimt itself works as before. `main.rs`,
  a third of the code at 11,600 lines, is down to about 3,100: the window's parts are each in
  a module of their own (the image area, the file list, menus, dialogs, the info panel,
  folders, the selection, the slideshow, Check Images and more), and helpers for text and
  threads are in `format.rs` and `threads.rs`. Every module names what it uses instead of
  `use crate::*`, two tuples of five and six fields became structs with named fields, and long
  comments are wrapped. AGENTS.md lists the modules.

## [0.50.0] - 2026-11-01

### Changed

- The Flatpak is fit for Flathub: Glimt's own programs (the file dialog, the trash, ffmpeg,
  sqlite3) are the runtime's, in the sandbox, and go through the desktop portals there, so the
  host needs none of them installed. **Plugins, decoder plugins, a video player of your own, the
  applications listed in Open With, and network shares (smb://, sftp://) run on the host, which
  the Flatpak now reaches only after**
  `flatpak override --user --talk-name=org.freedesktop.Flatpak io.github.thornbux.Glimt`;
  until then they say so.
- In the Flatpak, File → Print offers the System print dialog, the desktop's own, which picks
  the printer; full screen keeps the screen saver off through the inhibit portal; and without
  the host permission, Open With → Choose Application… shows the desktop's application chooser.
- In the Flatpak a read-only file, which the trash portal refuses, goes into the trash by hand,
  as the trash spec says, and Undo puts it back.
- GNOME's Large Text is read from the desktop portal, as the dark style already was, instead of
  from gsettings.

### Added

- `tools/flathub-manifest.py` writes the Flathub repository's manifest and crates for a release.
- An optional feature `portals` (in `full`) for the portals that pass a file or wait for a
  dialog.

## [0.49.0] - 2026-11-01

### Added

- A `.*` button beside the search field switches between fuzzy search and regular expressions,
  as the setting in Preferences → File List does. It is lit while regular expressions are on,
  and the Case checkbox shows beside it then.

### Fixed

- The hint text in the empty search field is shown in the chosen language; it was English in all.
- An ICO with more than 256 sizes shows its best size first, and each size as a page; one past
  the 256th was left out.
- A background job split over several cores can no longer put results on the wrong images if
  one of its parts fails: it stops instead.

## [0.48.0] - 2026-11-01

### Added

- Every size of an ICO file is shown, as pages: the largest first, as before, and Ctrl+Page
  Down and Ctrl+Page Up go to the next and previous size. Save This Frame, Export Frames, Crop,
  Save As and Copy Image take the size shown.
- A Code of Conduct (Contributor Covenant) for the project.

### Changed

- Find Duplicates by similarity groups the images on all cores, and compares fewer pairs:
  100,000 images in 4–10 s instead of 25–86 s, with the same groups. The progress bar now also
  counts the grouping, where it stood still before.
- Deleting files for good goes in chunks of 100, as moving them to the trash does, instead of
  taking the sidecar locks for each file on its own.
- The Flatpak builds without the network, from the crates listed in
  `flatpak/cargo-sources.json`, as Flathub requires.

### Fixed

- In full screen, Glimt no longer redraws the window every half second while only the shown
  image is being decoded.

## [0.47.0] - 2026-10-31

### Fixed

- In a full-screen slideshow the busy pointer no longer flashes up at each image not preloaded:
  the shown image being decoded no longer keeps a still pointer from hiding.
- In full screen the pointer hides when work that kept it ends, even if nothing else redraws
  the window; it could stay on the screen until the mouse moved.
- The Preferences search finds the preload estimate under Memory and CPU ("Up to ≈ … MB, each
  image at most …").

## [0.46.0] - 2026-10-31

### Changed

- New Folder makes the folder in the background: on a share that stopped answering the window
  no longer freezes until it is made. Opening a path is the last of these left in the window.
- A delete holds the sidecars of one chunk of files at a time, not of every file for the whole
  job, so a keyword saved in another window meanwhile waits for one chunk, not thousands.

### Fixed

- Undo of a move to the trash takes the move's own file, not another file of the same name
  trashed in the two seconds before the move.
- Rename Many keeps its plan on the disk once a swap's files are all under hidden names: after a
  power cut that lost every count of a finished swap, the next start took it for not begun, and
  finishing it would have swapped the files back.
- A `--remote` command that connected just as Glimt closed is answered too; it could get an
  empty answer, which `select` reads as "all found".
- Two threads of one Glimt never hold one sidecar at once, also when the lock file in the
  runtime folder could be made for one of them and not the other.

## [0.45.0] - 2026-10-31

### Added

- In full screen the pointer hides when the mouse has been still for two seconds, and comes
  back when it moves; while work goes on (the folder read or sorted again, as after a
  slideshow, or a file job) it stays, as the busy pointer, and so it does while a window or
  menu is open. Preferences → Image turns it off.
- A search field at the top of Preferences: only the sections with the words typed show, in
  English or in the language shown, and Keyboard shortcuts only the actions it finds.

## [0.44.0] - 2026-10-31

### Security

- A Lightroom catalog whose tables Glimt reads include a virtual table or a view is refused
  before anything is read from it: sqlite3's safe mode lets a virtual table through, and one
  such as zipfile reads any file it is given into the import.
- Undo of a move to the trash passes over an entry dated in the future: a trash folder on a
  memory stick from someone else could hold one of the same name, which came back instead of
  your file (yours stayed in the trash).

### Fixed

- A keyword or star saved while its photo is moved, renamed or deleted (or moved back by Undo)
  no longer leaves a sidecar under the old name, which a later file of that name would take
  over: the operation waits for the write, and a write that waited finds the photo gone.
- Putting back a Rename Many cut short by a power cut puts every file back: the count of the last
  steps done can be lost then, and a photo was left under a hidden temporary name. The steps
  done are now found by the names too.
- glimt --remote select sent just as Glimt closes is answered (none of its paths selected), not
  left with an empty answer that reads as all found: closing waits for each client still
  connected, up to a second, and answers what it sends.
- Rename Folder runs in the background: a folder on a network share that stopped answering no
  longer holds the window until the share comes back.
- Importing a Lightroom catalog works for a user whose sqliterc changes sqlite3's output (such
  as .mode json): sqlite3 is started without it.
- A very long strip (10000×5) is sharp again after the window grows: since 0.43.2 it was only
  decoded again when its short side grew by more than a pixel too.
- The Help Manual can be made narrow: the tables' long texts wrap to the window's width, where
  before the widest row of Differences from Geeqie set the window's least width.

## [0.43.2] - 2026-10-31

### Fixed

- A photo in a window of some sizes is no longer decoded again and again, using every core and
  showing its thumbnail with no size or zoom in the status bar: on a HiDPI screen a 4000×2657
  JPEG came out two pixels narrower than the scale shown, which counted as too small. Full
  screen and back stopped it.

## [0.43.1] - 2026-10-30

### Security

- A sidecar that is a link (photo.jpg.xmp pointing at another of your files, such as a private
  key, in a downloaded folder) is no longer copied through by Crop, Save As, Save This Frame, Copy
  or a move to another disk: the copy gets no sidecar, and a move takes the link as itself.
- An SVG whose entities would grow to gigabytes (100 KB of text can) is refused instead of using
  up the memory, which ended Glimt each time its folder was opened; also inside another SVG.

### Fixed

- A rotation is not saved while Rename Many or a move is still working on the photo, where it
  could be saved over another photo that took its name; Glimt says so instead.
- Sort by name puts a collection in name order with Case on, the default: it kept the collection's
  own order.
- Refresh (F5) in a folder that has not changed shows the image again, also one an editor saved
  under its own name: it asked for nothing, and the busy pointer turned until the next image.

## [0.43.0] - 2026-10-30

### Added

- Edit → Plugins has Stop for a plugin that is still running, such as a script that hangs: it ends
  the plugin and what it started, and its runs on further files are not made.

### Changed

- The setting that saves thumbnails, the manual and the README say that saved thumbnails are small
  copies of the pictures, which stay readable after the photos are on an encrypted volume or a
  stick taken away.

### Fixed

- A GPS position stored as degrees alone, one value where the standard has three, is shown in the
  EXIF panel instead of no GPS at all.
- Two Glimts with the same process ID on two computers sharing a home folder, or in two
  containers, never use the same temporary file when they save at the same moment.
- A glimt --remote select that waits for a folder to be read is answered for that folder: another
  script's command sent after it, such as open, waits its turn. At most 64 commands wait; one more
  is told to try again.

### Security

- Remote control is used only in the user's own runtime folder: where XDG_RUNTIME_DIR is set to a
  folder others can write, another user could otherwise have answered glimt --remote. A command is
  read no further than the largest select.
- A network address that begins with - is refused before it reaches gio, which would read it as an
  option such as unmount or eject.

## [0.42.2] - 2026-10-30

### Security

- A password written into a network address (smb://user:secret@nas/share) is no longer put on
  gio's command line, where every user of the computer could read it in the process list, nor
  shown in Connecting to…: it is given to gio when gio asks for it.

## [0.42.1] - 2026-10-30

### Security

- A sidecar (photo.jpg.xmp) that is a link is no longer written through: a downloaded folder could
  point it at another XMP file of yours, which adding a keyword, a comment or stars then
  rewrote. Glimt now says the sidecar is a link and leaves both alone.
- File → Import Lightroom Catalog reads a catalog with sqlite3's safe mode, which refuses the
  functions that write or read other files: on sqlite3 older than 3.30 (RHEL 8 has 3.26), a
  catalog with a view in the place of a table could write files when it was read. Importing now
  needs sqlite3 3.37 or newer, and says so on an older one.
- Undo after a move to the trash reads a trash folder at the top of a disk (.Trash-1000,
  .Trash/1000) only when it is your own folder, as the trash specification asks: another user who
  can write on that disk could otherwise have Undo put their file in the place of yours.
- A copied file no longer keeps a set-user-ID or set-group-ID bit, as cp does: a program another
  user had made so could be copied into one that runs as you.

## [0.42.0] - 2026-10-29

### Added

- A window title of your own in Preferences → Window: a template of %name%, %path%, %folder% and
  %app%, as the full screen information has. Left empty, the title is as before.
- Window → Set Window Title… gives this window a title of its own, as a template, until it closes;
  left empty, the one in Preferences again.
- glimt --remote title TEXT gives a window a title of its own from a script, and title alone the
  one in Preferences again.

### Changed

- Every file operation is held to one contract in the tests: run on a folder with sidecars, stars
  and a collection, it must bring them along and wait while other work changes its files; a new
  operation fails the tests until it joins.

### Fixed

- A random slideshow with the images in subfolders no longer ends at once when started on the last
  image without Repeat: it now waits for its shuffled list.
- A mark or star that could not be saved (a full disk) no longer stays shown in the window as if
  it were saved.
- A random slideshow with the images in subfolders and without Repeat shows every image: it
  started its round where the shown image had landed in the shuffled list and ended after the
  images behind it.
- A random slideshow of a collection with "Include the images in subfolders" on for the
  slideshow plays in random order: it played the collection in its own order.
- The size of the selected files in the status bar no longer stays on calculating until the
  pointer moves.
- With hundreds of thousands of files marked, each mark or star key after the first no longer
  reads the whole marks file again: the window takes what it wrote itself.
- In a large folder sorted by EXIF date, giving an image keywords, stars or alt text no longer
  reads every file's EXIF again: the folder read again keeps its order when no name is new.
- Full screen no longer waits for the screen saver inhibitor to be looked for: in the Flatpak that
  asked the host on every F.
- A collection shown while one of its images is renamed or moved keeps the image, with auto
  refresh on: a refresh that started just before the rename was reported brought the old name
  back, which dropped the image from the list, and a later move of it no longer changed the
  collection's file.
- Large folders stay quick in four more places: a search sums the size of its matches once typing
  pauses, not at each letter, and an exif: search looks through the index in parallel; Shift+Up or
  Down changes only the rows between the old and new end of the selection; a delete no longer
  looks at every name in the list and every mark for each file; the folder tree draws only the
  rows on screen, opened at the folder shown.
- Two glimt --remote select sent while a folder is still being read both get their answer, in the
  order sent: the second took the first one's place, and the first script got no answer, which it
  read as every file found.
- Editing a collection that Geeqie wrote keeps Geeqie's own lines (its #geometry and comments):
  they were dropped the first time Glimt took an image out or put them in order.
- Crop, Save As, Save Frame and Export Frames keep a file name that is not UTF-8 (a Latin-1
  café.jpg) as it is in the new file's name, instead of a replacement character.
- A file with a long name (over about 230 bytes) can be saved turned and given keywords, stars and
  comments: the temporary file was named after it with more added, past the limit of 255 bytes.
- An EXIF field that says it has no values, or fewer than are read, shows nothing instead of a
  number made of the bytes beside it (an aperture of count 0, a GPS position with one value).
- The status bar's Renaming and the message when the keyword tree cannot be saved are in every
  language; three texts no longer shown anywhere are gone from the translations.
- Stars and marks set in another Glimt window follow the files this window moves or renames, and
  go with those it deletes, also while a move started in this window is still running: this window
  decided from its own copy of them, which did not have them yet.

## [0.41.1] - 2026-10-28

### Fixed

- A playlist opened by its name in its own folder (glimt show.m3u) shows its images: its entries
  were joined to an empty folder name and every one was left out. A collection or list opened by a
  relative name is kept in Open Recent and as the last folder by its full path, so another window
  opens that file and not one of the same name where it was started.

## [0.41.0] - 2026-10-26

### Fixed

- Save This Frame refuses to save over the animation it comes from: a still took its place, and
  since this version its sidecar, marks and stars would have gone with it.
- Save As or Save This Frame over a file in the list shows the new image, not the old one's
  thumbnail and pixels, and the old one's marks and stars go, as its sidecar did.
- An exif: search finds files renamed, moved or copied into the folder, new ones a refresh
  brought, and those of subfolders once Include Subfolders is on: the index never read their
  names, so no search found them.
- Undo, Save As, Crop and moving a collection to the trash wait while other work changes the same
  files, as Copy, Move and Delete do: an Undo could run in the middle of a Rename Many, and a
  delete in the middle of an Undo. An Undo held back stays to be done.
- Asking the desktop for its colour profile, dark style and text size, the Open With list's names
  and whether a program is installed all end after 30 seconds: each could wait for good.
- A --remote command sent as another window takes the focus goes to that window: the file naming
  it was emptied before it was written, and the command went to any window.
- Save as Theme writes the theme whole or not at all: a full disk left half a theme, whose name
  was then taken.
- Open Recent keeps the folders two windows open at the same moment, and never writes over a list
  it cannot read.
- Save This Frame over another file leaves that file's sidecar behind: the frame showed its
  keywords.
- Select → Copy Image copies the image as it is shown: turned by hand, and the page of a multi-
  page TIFF shown. It copied it unturned and its first page.
- Deleting or moving to the trash on a share that stopped answering ends with an error after two
  minutes, instead of running for good with Stop unable to end it: gio trash had no time limit.

### Changed

- What belongs to the image shown (its page, frame, turn not saved yet and crop) is kept together
  and made anew when another image is shown, so nothing of one image can stay for the next.
- Every file the windows share (the settings, marks and stars, the keyword tree, the sort
  manager's folders, the recent list) is read, changed and written in one function, under its
  lock, never over a file it cannot read; clippy refuses the lock anywhere else.
- What happened to files on disk is told in one report, and one place brings along everything that
  follows a file (marks, the collection, a turn not saved, decoded images, thumbnails, details,
  the exif: index); a separate channel for files a plugin edited is gone.
- Every file operation names the files it changes when it starts, and the check for other work on
  them is made there, by the job or the background work itself, so no operation can leave it out.
- The window's state can be made in a unit test without a window (App::with, tests::app_on), so
  the logic behind the list, the selection and file operations is tested in milliseconds instead
  of only through the release tests' virtual display.
- Reading a file whole, writing, renaming, copying and starting or waiting for a program each go
  through one function, and clippy refuses the raw calls everywhere else (clippy.toml), so the
  next feature cannot leave them out; the few places that must do otherwise say why. Every program
  Glimt waits for is stopped the same way.

## [0.40.1] - 2026-10-25

### Fixed

- Answering the repair of a cut-short Rename Many in a second window as well no longer moves marks
  and collection entries a second time (a swap's marks swapped back, a renamed file's lost), nor
  can it do the steps again on a plan the first window had just put right.
- Crop and Save As on page 2 or later of a multi-page TIFF save that page, not the first.
- Crop ends when its image is no longer the one shown (deleted or renamed meanwhile, the folder
  read again, another turn or page): Enter saved the part chosen from the next image.
- Save As over another file gives the copy its own sidecar, or none when saved without metadata:
  the replaced file's keywords, comment and stars stayed on it. A crop never takes a sidecar left
  at its name.
- A crop saved shares only what was kept: the camera's EXIF thumbnail of the whole frame stayed
  inside the copy, unlinked but readable by any tool that looks for a JPEG in it. The same holds
  for an image saved turned.
- Undo of a large move to the trash, and deleting many images from a large collection, no longer
  take minutes (each file was looked for through the whole trash or the whole collection), with
  closing waiting for them.
- Undo of a move to the trash gives the files their marks and stars back, and their places in the
  collection shown; both were gone for good.
- Undo of a move to the trash works on USB disks and shares without one-step renames (FAT, NTFS
  and exFAT through FUSE): it failed there every time.
- Undo of a move to the trash puts back what that move put there and nothing more: not an older
  sidecar of the same name from the trash, which belonged to another file and brought its keywords
  and stars along.
- A folder that came with a download cannot take Glimt down any more: a sidecar linked to
  /dev/zero or a pipe in its place, a large HEIC or AVIF without EXIF, or a keyword of a million
  '&' filled memory, blocked a thread for good or took hours. Every file is now read whole only if
  it is a file and of 512 MB at most, and a test holds every read in the code to that.
- A print to a printer longer than two minutes is printed: lp was ended two minutes after it
  started, while the PDF was still being made for it; now only after two minutes in which it takes
  nothing.
- HEIC, AVIF and video frames show at once when Glimt was started from a terminal: ffmpeg was
  handed the terminal for its input, stopped itself there, and each file waited 30 seconds for
  nothing.

### Changed

- Glimt renames in one way: one step where the file system can, else a link and an unlink, else a
  rename after looking; moves, copies, new files, folders, collections and Undo all use it.
- Every program Glimt may stop starts with the same safe defaults (a process group of its own, no
  terminal for its input, its output and errors read), and is stopped the same way, TERM and then
  KILL, also by the watchdog of lp and gio mount, which in a Flatpak could not reach the program
  on the host.

## [0.40.0] - 2026-10-22

### Added

- The .deb and .rpm install the example plugins and decoders in
  /usr/share/doc/glimt/examples, and the tar.gz has them in examples/, ready to copy to
  ~/.local/share/glimt/plugins or decoders.
- Edit → Undo (Ctrl+Z): the last files or folder moved to the trash come back from it, the last
  files moved go back, and the last renamed get their old names. Never over a file that has
  taken the name since; one step, in the window that did it.
- File → Set as Wallpaper: the shown image as the desktop's background, through the desktop
  portal (GNOME and KDE, also from the Flatpak), which keeps a copy of its own.
- File → Open Recent: the last ten folders and collections opened, shared by the windows, with
  Clear Recent. Kept in ~/.local/state/glimt/recent; Preferences → Keep a list of recent
  folders, when off, keeps none and deletes the one there.
- A checkerboard behind transparent images (Preferences → Checkerboard behind transparent
  images, off by default), so black on transparent is seen on the black background; opaque
  images look as before.
- Edit → Crop… (Shift+C): drag over the image to choose a part, and Enter saves it as a new
  file beside the image (NAME-cropped), as the view shows it; the image itself is never
  changed. JPEG at the quality set in Preferences (92 by default), PNG and WebP as they were,
  other formats as PNG, with the EXIF, XMP and sidecar.
- File → Save As… (Ctrl+Shift+S): the shown image saved again as JPEG, PNG or WebP (lossless
  or lossy), at its size or smaller (longest side or percent), at a quality of your choice,
  and if asked without EXIF and GPS, XMP and sidecar; never over the image itself.
- The interface in Danish, Dutch, Finnish, French, German, Icelandic, Italian, Polish, Spanish
  and Swedish too, by the system's language or chosen in Preferences; nouns after a number take
  each language's form (Polish's three, Icelandic's 21 as one, French's 0 as one).

## [0.39.0] - 2026-10-21

### Fixed

- A program installed while Glimt runs (zenity, sqlite3, ffmpeg) is found without a restart: only
  a program that is there is remembered, one that is not is asked for again.
- glimt --remote open of a folder or file whose name ends in a space opens it as named, instead of
  a trimmed name that is not there.
- Escape closes the Check Images window, as it closes the pan view; a check still running stops.

### Changed

- egui and eframe 0.37 (from 0.36.2), with accesskit 0.25: the same look and keys; the names in
  keys= are unchanged, as the test pins them. The arrow after Sort by and All types in the status
  bar and the arrow keys of the Keyboard Map are from the fonts Glimt ships, as egui no longer
  bundles the emoji fonts that drew them; a test holds every sign of the UI text to those fonts.
- The release tests click menu items, buttons and choices by their text (a test-hooks build's
  glimt --remote where LABEL), not by screen coordinates, so a change to the look no longer breaks
  them.
- The file list and everything indexed into it (the shown image, selection, panes, decoded slots,
  search matches, drags) are one List, made anew when the list is read again: nothing indexed can
  outlive its list, whatever is added later; what follows a file is carried over by name in one
  place.
- Every receiver of a background thread's answer is a Receiving, whose only ways to read tell once
  when the thread is gone and forget it, so no new receiver can leave the window waiting for a
  thread that broke.
- The manual (Help → Help Manual, and the user guide on the website) is in short paragraphs and
  lists instead of one block of text per chapter; every fact and key as before.
- The rule that a file which is there but cannot be read is never written over (the settings,
  marks, the keyword tree, the sort manager's folders, a sidecar) is one function, where each had
  its own copy.

## [0.38.5] - 2026-10-20

### Changed

- Preferences is in ten sections, each with a heading and an icon: Look, Keyboard, Image,
  Slideshow, File list, Deleting, Colour management, Memory and CPU, Video, Start and system; the
  settings are grouped by what they are about instead of one long list.
- A plugin's edits and a repair's renames ask for the folder to be read again by name (relist) in
  the one report every file operation sends, instead of posing as a failed delete.
- What holds places in the file list is let go in one place (list_changed) when the list is read
  again or files are taken out, the selection is restored by name in one place, the thumbnails'
  look change in one, and the folder watcher ends with the old list in one, instead of the same
  lines written by hand in two or three places each.

### Fixed

- The rating set in a sidecar, the keywords set in one, the info and print template, ffprobe's
  sizes, the desktop's theme answers, colord's device list and plugin desktop files are fed broken
  input in every test run, as the other parsers are.
- A plugin, Open With or Play Video program's output is not kept in memory while it runs (a
  converter writing a 2 GB file to its output would be), only its errors.
- A Rename Many that could not put every name back and could keep no plan for the next start (no
  state folder, a full disk) says where the files are, instead of promising a repair that cannot
  come.
- The orientations that turn an image a quarter, the test for a JPEG by name and which files
  another program reads are each written once, where they were in three, five and two places that
  had to change together.
- A PDF printed to a file and a new thumbnail are written through the one write-beside-and-rename
  every file Glimt writes goes through, instead of two copies of it; a later change to that one
  reaches them too.
- Open Several Folders after a file of a type the Types menu leaves out (a RAW with RAW off, a
  video with videos off) lists by the menu again, instead of keeping that type listed for the
  folders too.
- With --lock-settings (a kiosk, a shared machine) a change to the keyword tree lasts for the
  session only, as the other settings do, instead of being written to the tree file.
- Renaming a folder or a collection, and a new collection's file, never land over one of that name
  made by another window or program between the look and the rename (RENAME_NOREPLACE, as a moved
  file); one rule for all three.
- A file that breaks the EXIF reader costs only itself in the exif: index, instead of its whole
  share of the folder (1/N of the files) silently left out of every exif: search.
- Check Images and sorting RAW files by EXIF date read a file through the usual size limit, so a
  file that claims gigabytes (a sparse or damaged one) is refused instead of read whole.
- Asking gio for a share's local path before mounting it, the Open With list (gio info and gio
  mime) and lpstat have the usual time limit, and lp is ended after two minutes without an answer,
  instead of Connecting…, an Open With list at … or the busy pointer for ever while a server does
  not answer; the mount's own watchdog is the one every such program uses.
- A folder listing, the selection's size or the subfolders whose thread broke no longer leave the
  busy pointer, Reading folder…, auto refresh or a waiting --remote select stuck for the window's
  life: told once, and nothing waits; and the Pan view stops reading an old list's days when the
  folder or its order changes, instead of starting the new ones only after every old file was
  read.
- A layout's other panes keep their files when the folder is read again (auto refresh, another
  sort order, a file added), as the shown image does, instead of keeping their places in the list,
  which are other files' then: a slideshow one pane at a time no longer shows another image in a
  pane after a refresh.
- A Rename Many cut short by a crash and finished at the next start tells marks, stars and
  collections about the renames, so they follow the files as after a finished Rename Many; and the
  list is read again when the steps are done, not before.
- Keywords, a comment or the alt text typed in the panel are written as a sidecar job like every
  other sidecar write, so Move, Rename and Delete of that image wait until they have landed,
  instead of the new keywords being left under the old name while the photo moves without them.
- Rename and Rename Many run as a job like every other writer: closing waits for them (since
  0.38.2 the window could close in the middle of a batch, and the next start said it was a crash),
  the status bar counts them and Stop puts the names done so far back; and Copy, Move, Delete,
  Rename Folder, Move Folder to Trash and plugins wait while a Rename Many runs, as a step would
  find its file gone and the undo could not put the others back.
- HEIC, AVIF and video frames are decoded no larger than the view (ffmpeg scales them), with the
  real size from ffprobe for the zoom and the status bar, instead of a whole 48 MP frame for each
  image shown or preloaded; the print and thumbnails report their real size too.
- A settings file with a byte that is not UTF-8 (a path in another encoding) is saved again like
  any other, instead of every later save being refused with "cannot be read".


## [0.38.4] - 2026-10-20

### Added

- An icon before every item of the menus and submenus, from the Phosphor set the toolbar uses.
- glimt --remote slideshow, random and fullscreen take on or off, so a script sets a state instead
  of flipping whatever the window was in; without the word they go to the other, as before.

### Changed

- The settings that are one of a few are written by name (fit_mode=width, fade=long,
  histogram=rgb, thumb_size=large, toolbar_icons=large, view_filter=inverted, cpu_use=full), so
  that a longer list in a later version cannot change what a saved number means; the numbers of
  before are still read.
- The size an image is decoded to goes with its decode job (the view's box, turned with the shown
  image when that is turned by hand, at least the projector's), instead of one shared box set in
  one frame and read in the next; neighbours of a turned image are decoded at their own size at
  once.
- A move or copy reports its moved files and the gone ones in one message, so their marks follow
  before the gone ones are forgotten whatever the timing; nothing depends on two messages' order
  any more.
- A rename step is done, undone and put right after a crash by one and the same code
  (journal::step), so a case fixed for one of them holds for the others.
- Closing waits for everything being written through one count held by every writer (file jobs, a
  turned image, a print, keywords, the settings), instead of a list of counters a new writer had
  to be added to; a third close ends at once without waiting for anything.

### Fixed

- The Pan view shows the days read so far while a large folder is still being read, every half
  second, instead of nothing until every date is known; Escape closes it.
- Include Subfolders is kept across starts, as the other View toggles are, now that a large tree
  shows its top folders at once; since 0.38.0 the saved setting was turned off at every start.
- Release tests of daily use: the zoom and fit keys, the wheel and a drag, Open With (the default
  application and the list), the clipboard read back, and the Norwegian interface in a real
  window; closing is waited for instead of a fixed second, and the first image before the keys.
- A plugin that is an editor no longer keeps the busy pointer on for as long as it stays open: a
  plugin runs without it, as Open With does; its failure is still told.
- A kiosk always listens for --remote quit, also with remote control turned off in the settings;
  where it cannot (no $XDG_RUNTIME_DIR), it says so and Ctrl+Q closes it, instead of a kiosk
  nothing but kill ends.
- glimt --remote open takes one path: two words are refused as two paths (exit 2), instead of
  being joined into the name of a file that is not there.
- A PDF printed to a file and a new thumbnail are written like every other file Glimt writes:
  never through a link planted at the temporary file's name, and a PDF that was a link stays one.
- Mounting a share that never answers (a NAS asleep, a VPN down) gives up after a minute and says
  so, and reading a Lightroom catalog has the usual time limit, instead of Connecting… and the
  busy pointer for ever.
- A damaged GIF, animated WebP or APNG whose frame header claims gigabytes is refused, instead of
  a failed allocation ending Glimt at once; damaged JPEG, TIFF, PNG, WebP, HEIC, colour profile
  and marks files are fed to their readers in every test run too.
- Renaming a collection waits for an edit of it under way in another window, so the edit lands in
  the renamed file instead of in a new file of the old name.
- glimt --remote select or current sent while the window is still reading a large folder, or has a
  dialog open, comes back as no answer (exit 1) once the time is up, never as an empty answer with
  exit 0 that a script takes for every file selected; and one client waiting holds up no other.
- The print dialog's preview shows a photo with its own colour profile in the print's colours, as
  the page will have them. One small decode for thumbnails, the preview and the print, where there
  were three.
- A file whose decoder crashes gets its failure mark in the thumbnail cache like any other,
  instead of being decoded again on every visit.
- A background thread that ends without its answer (it broke on a file) is told once, and nothing
  waits for it any more: Rename Many is not refused for the rest of the session, closing does not
  wait for ever, the Pan view, Find Duplicates and the exif: search do not stay at Comparing or
  Calculating. Find Duplicates leaves out only the file that broke its reader, not the whole share
  of the list that worker had.
- The exif: index and the Pan view's days stop being read when the folder is left or the window
  closed, instead of reading every file to the end for no one and keeping the busy pointer on.
- A move within a file system is one step that never replaces a file (renameat2 with
  RENAME_NOREPLACE), so a crash or a forced close in the middle no longer leaves the photo under
  two names; file systems without it keep the way of before.
- Renaming one file runs in the background, as Rename Many does, so a share that stalls no longer
  holds the window; one path for both.
- A collection's file loses an image only once it is really deleted, in one place for Delete and
  Move Folder to Trash alike: a delete that fails or is stopped leaves the collection as it was,
  in its order, instead of taking the images out first and putting them back at the end.
- An image that could not be read (a share away for a moment) is read again when it is shown
  again, instead of the failure being kept for the session; the keywords edited mark their own
  file for the exif: search, not the one shown; panes are filled again after a delete; the
  selection's size follows a refresh; Open Folders includes the subfolders as the menu says, also
  after a collection.
- Copy, Move, Rename, Delete and the folder operations wait while keywords are being written to
  the files, as they wait for a turned image being saved, so a batch of keywords no longer misses
  the files moved before their turn; a file moved meanwhile counts as failed, not done.
- Save Orientation with Auto Rotate off writes the turn on the pixels as the view showed them,
  instead of on the file's EXIF orientation, which turned the photo wrong for every viewer that
  reads it.
- A slideshow of the subfolders in random order stopped before its listing arrived no longer
  leaves the folder in random order, shuffled again at every refresh.
- In the Flatpak, whether a program is on the host is asked of the shell once (host_has) and
  remembered: the file dialogs, Open, the screen saver inhibitor and the Lightroom import decide
  their fallbacks the same way, and the release tests run them as a Flatpak.
- A slideshow asks for its next frame itself, whatever it did in this one, so no layout, order or
  search can leave it standing still; its moves are one decision, tested over every combination.
- Rename, Rename Many, Copy and Move refuse a new name whose sidecar is another file's also when
  the file has no sidecar of its own, so a photo never shows a stranger's keywords; one rule for
  all four. A sidecar that cannot go with its copied or moved photo is told, instead of left
  behind in silence.
- A turned image whose save lands after the file was deleted, moved or renamed is not made again
  under its old name, whatever operation moved it: the save is refused and told. A plugin run on
  an image being saved turned waits, as Delete does, and its own report no longer ends that save's
  guard.
- In the Flatpak, Open With lists the programs installed as Flatpaks and the user's own, not only
  the distribution's: the host's programs get the host's data folders, not the sandbox's.

## [0.38.3] - 2026-10-19

### Fixed

- Glimt is offered under Open With for Panasonic's older .raw files too, which it reads.
- The Pan view's calendar shows the weekdays in Norwegian too in the Norwegian interface.
- glimt --remote open with a file name that is not UTF-8 opens it when no Glimt is running, and
  else says it cannot send it, instead of opening a name with a replacement character that is
  no file.
- %dimensions% in printed captions is the size shown, upright, also for a portrait turned by EXIF
  and for RAW files, instead of the stored size or a RAW's thumbnail.
- A folder is not renamed or moved to the trash while a turned image in it is being saved, which
  left a hidden temporary file as large as the image behind.
- A move whose source cannot be removed (someone else's file in a shared folder) leaves no second
  name behind in the folder it went to.
- Open With finds the applications installed on the system also in the Flatpak, where it said none
  were found, and no longer lists the Flatpak Glimt itself.
- A RAW photo whose preview is less than twice the view's size (a 24 MP camera on a 4K screen) is
  read only as far as its preview, not whole.
- The images after one turned a quarter by hand are decoded at their size at once, not too small
  first, and a turned photo on the projector is sharp.
- Keywords, comments and alt text are written through the same thread as stars and tags, in the
  order they were made, and closing waits for them; a share that stopped answering no longer holds
  up the saving of settings and stars.
- Move and Copy refuse a photo whose sidecar name in the folder is taken by another file's, as
  Rename does, instead of giving the photo that file's keywords and stars and leaving its own
  behind.
- A Rename Many whose plan could not be written (a full disk) leaves no half plan behind, which
  the next start would offer to finish and so undo the renames.
- With Auto Rotate off, files no thumbnail could be made of are marked and not tried again on
  every visit, as with it on (since 0.38.2).
- Ctrl+Shift+C copies the image to the clipboard, as the menu says, instead of opening Copy to
  Folder.
- A slideshow in a layout that changes one image at a time goes on with Fade off, or in power
  saving, instead of standing still once its images are loaded.
- A slideshow in random order in a layout that changes one image at a time goes on into a new
  round, instead of shuffling the whole list on every frame and standing still.
- Full screen in the Flatpak, on a host where no screen saver inhibitor runs, stays smooth,
  instead of trying again on every frame and drawing about five frames a second (since 0.38.2).
- New Collection works on a new install again, where the collections folder is not there yet,
  instead of saying the collection could not be saved (since 0.38.2).

## [0.38.2] - 2026-10-19

### Fixed

- In the Pan view, an image whose camera left its EXIF date blank is under No date, instead of a
  day of its own with a heading of dashes.
- Find Duplicates stopped halfway shows no result, instead of saying it found no duplicates among
  all the images.
- Exposure times from 0.3 to 0.8 s show as such in the EXIF panel and overlay, instead of 1/3 s,
  1/2 s or 1/1 s.
- A copy of a private photo is not readable by others while it is written; it gets the photo's
  permissions once whole.
- In the Flatpak, the projector window has Glimt's icon and stays with Glimt in the dock, instead
  of an entry of its own with a generic icon.
- Plugins and decoders whose desktop file starts with a comment are found, instead of silently
  left out.
- Putting a cut-short Rename Many right no longer now and then does nothing, when Glimt started
  another program at that moment.
- With Auto Rotate off, thumbnails are made as the image is stored, also in a folder seen before,
  instead of taken upright from the shared cache.
- In the Flatpak, a host without zenity falls back to kdialog for the file dialogs, opening with
  the default program falls back to xdg-open, and the screen saver is kept off by systemd-inhibit
  where gnome-session-inhibit is missing; a host with neither file dialog says so instead of doing
  nothing.
- Rename refuses a new name whose sidecar is left over from another file, as Rename Many does,
  instead of giving the renamed photo that file's keywords and stars.
- A sidecar moved along with its image to another drive is on disk there before the original goes,
  as the image is, so a stick pulled out or a power cut no longer loses its keywords.
- Rename Many that cannot put every name back after a failure (a share that stopped answering)
  keeps its plan and says so, so the next start offers to put the files right, instead of saying
  nothing was renamed.
- Keywords, comments and stars still waiting to be written when their folder is renamed no longer
  make a folder of the old name again, full of sidecars of nothing.
- A link planted beside a collection or a photo in a shared folder, where Glimt puts its lock or
  temporary file, no longer empties the file it points to.
- Marks and stars of files moved to another folder go along with them also when the move's two
  reports arrive between two frames, instead of now and then being forgotten.
- A slideshow in random order goes on when a search hides the shown image or shows only one,
  instead of shuffling the whole list on every frame and standing still.
- An image turned a quarter by hand in Fit, Fit Width or Fit Height is decoded once at the size
  shown, instead of too small first and then again.
- Zooming in a layout of several panes decodes each pane's image at full size once, instead of
  again and again on every frame with the panes never sharp.

## [0.38.1] - 2026-10-19

### Fixed

- Sentences of the Norwegian interface that were half English ("3 filer copied to …", the status
  bar's "selected" and "bytes", the file dialogs' titles) are Norwegian throughout.
- Images printed on small photo paper with a wide margin are decoded for the room they get, not
  for less (since 0.38.0).
- Landscape pages printed on both sides turn on the short edge, so the backs are the right way up
  (since 0.38.0).
- glimt --remote subfolders takes only on or off, and tells anything else as an unknown command
  instead of doing nothing (since 0.38.0).
- Enter in a text field of the Print dialog (the page text, the template) ends the typing instead
  of printing at once (since 0.38.0).
- Turning Include Subfolders on during a slideshow in random order keeps it random: the images
  found so far came in name order and took the shuffled list's place (since 0.38.0).
- The images of a tree found so far wait one at a time for the window: a window not drawn while a
  large tree was read could gather a copy of the list for every quarter second.
- With Include Subfolders on, a tree is read in name order and opens on its first image, also in
  another sort order, instead of on an image of its last subfolder (since 0.38.0); its first
  images show as soon as they are found, also inside one large subfolder.

### Changed

- The images of a large tree found so far come less often as the list grows, never taking more
  than a fifth of the reading's time to copy.

## [0.38.0] - 2026-10-19

### Added

- The interface in Norwegian bokmål (src/no_nb.po), chosen by the system's language (LANG) or in
  Preferences → Language; a text not translated shows in English, and the Help Manual, the command
  line and --remote stay English.
- The Print dialog shows the first page as it will print: the images, their text and the page
  text, laid out as the PDF is.
- Printing on both sides of the paper, on a printer that can.
- Text at the top and the bottom of every printed page, with %page% and %pages% for the page
  numbers, as Geeqie's page text.
- The text under each printed image comes from a template, as the full screen info's: the file
  name by default, or its date, camera, keywords, comment and other EXIF fields, as Geeqie's image
  text.
- Printing can fill each box with its image, cut to the box, for even contact sheets and
  borderless photo prints.
- One image a page prints at a size of one's own, 10 to 100 % of the room it has, centred, as
  Geeqie's Image size.
- A margin of one's own around the printed page, 0 to 50 mm, instead of always 10 mm.
- Printing turns the paper by the image as before, or always portrait or landscape; a contact
  sheet on landscape paper turns its grid with it.
- Printing on A3, A5, Legal and photo paper of 10 × 15 and 13 × 18 cm, beside A4 and Letter.
- glimt --remote subfolders on|off turns Include Subfolders on or off.

### Changed

- The About page links to glimt.pario.no and to Glimt's MIT License.
- Include Subfolders is off at every start: kept on, a start in a folder with many subfolders read
  the whole tree before any folder showed.

### Fixed

- With Include Subfolders on, the images of a folder's tree show as they are found, a subfolder at
  a time, in name order; the sort asked for comes when the whole tree is read.
- With Include Subfolders on, a folder's own subfolders show at once, before its whole tree is
  read; and opening another folder meanwhile stops reading the tree, which went on in the
  background until its end.

## [0.37.2] - 2026-10-18

### Fixed

- A thumbnail is written whole or not at all: on a full disk a cut-off PNG could land in the
  thumbnail cache other programs share.
- Folders added to the sort manager in two windows at the same moment both stay: its file is
  locked while it is read and written, as the keyword tree's.
- Keywords, a comment and alt text are saved in the background, not in the window's thread: a
  sidecar on a slow or stalled share no longer freezes the window when a field is left or the next
  image shown.
- Rename Many keeps the letters of names that are not UTF-8 (Latin-1 from an old camera or a
  Windows zip) where the pattern has *, instead of turning them into "�" for good.
- A collection's file is no longer written again when a move or rename leaves its images as they
  were, nor called gone for an unrelated folder renamed while it is shown.
- Renaming or moving a file with many marked files is quick again: all marks are looked through
  only when a folder is renamed (since 0.37.0).
- Two Glimt windows from different builds (an AppImage and a distribution's package) wait for each
  other's sidecar writes too: the lock is chosen by a fixed hash of the path, not Rust's own,
  which may differ between Rust releases.
- A sidecar on a share that stopped answering no longer holds up the sidecar writes of other
  images in the window, nor through them the window itself (since 0.37.1).
- Closing Glimt no longer waits for a plugin's program to end, such as an editor opened from Glimt
  (since 0.37.1).
- A decoder program that leaves a helper running with its error output open is stopped after its
  time limit too, instead of holding a decoding thread for good (since 0.37.0).
- A turn not saved yet is saved into its file under the new name after Rename, Rename Many or
  Rename Folder, instead of being dropped (since 0.37.0).
- The turn and mirror keys in an empty folder no longer end Glimt (since 0.37.0).

### Security

- A lossy WebP saved turned is no longer briefly readable by others while it is made: ffmpeg hands
  it over in memory, and Glimt writes it through its own temporary file, private from the start,
  as every other format.
- A JPEG, RAW or SVG file larger than Glimt decodes (512 MB) is refused by its size before it is
  read: a sparse or crafted file of gigabytes in a folder could fill memory and end Glimt when its
  thumbnail was made.

### Changed

- The About page shows Glimt's logo beside its name and version.

## [0.37.1] - 2026-10-18

### Changed

- Glimt prints the median, 90 % and slowest of its frames at exit, in the window thread's
  processor time, and the release tests check the median with 500,000 files selected and the
  Delete dialog open: the slowest frame on the clock varied from 20 to 64 ms between two runs of
  the same build, as it counts other threads' work too.

### Fixed

- Keywords, a comment, alt text or stars saved for the same image by two Glimt windows at the same
  moment both stay in its XMP sidecar: each window now waits for the other's write, through a lock
  in Glimt's runtime folder, not a file beside the photo.
- Closing Glimt while it prints to a PDF, moves files or a folder to the trash, puts a cut-short
  Rename Many right, runs a plugin or imports Lightroom collections waits for that to end, as it
  does for copies and moves, instead of ending it halfway.

## [0.37.0] - 2026-10-18

### Fixed

- The busy pointer goes when background work ends however it ends, also when a worker thread
  panics: each one now holds a guard instead of counting by hand.
- ffmpeg, ffprobe and decoder programs are all stopped after 30 seconds, with every program they
  started and also on the host in the Flatpak: ffmpeg's own limit counted only its working time,
  so one waiting on a share that stopped answering held the image for good.
- A turn not saved yet is saved into the file it was made on, whatever happens to the list
  meanwhile, and not at all when another program removed that file.

### Changed

- Delete, Rename and Rename Many act on the files they asked about, found by name wherever the
  list put them meanwhile (another image shown by --remote, the folder read again), instead of
  doing nothing when the list changed; nothing is done only when another folder is shown or the
  files are gone.
- A window writes only its own change to a collection's file (images taken out, renamed, added or
  put in order), never its whole list, so what other windows changed meanwhile always stays.
- Glimt prints its slowest frame at exit, beside the first image's time, and the release tests
  check it with 500,000 files selected, renamed, deleted and searched.

## [0.36.1] - 2026-10-18

### Changed

- The release workflow also checks the build without optional features, as people building from
  source get it, before it publishes.

### Security

- A file written again gets its permissions before its contents, so a private photo or sidecar is
  never readable by others while it is written.
- An SVG's pictures count by their decoded size, at most 1 GB for one SVG: a small plain picture
  that is large once decoded, named a thousand times, can no longer hold the decoding for minutes.
- An SVG gzipped twice is refused: its inner layer would have been unpacked without a limit, which
  a small file could use to take all memory.

### Fixed

- A file saved over under its own name by another program shows its new size and date in the list
  after the folder is read again.
- Turning Random Order on in a slideshow keeps the shown image and the ones read ahead, instead of
  decoding them all again.
- A file of the same name opened in another folder opens fitted, not at the zoom and place of the
  image shown before.
- A turned image that another program removes or renames no longer passes its turn, or its zoom,
  to the image shown in its place, where moving on wrote the turn into that file.
- Each Rename Many writes its plan to a file of its own, so a plan left by a crash is never
  written over by a new Rename Many of a Glimt that got the same process ID after a restart.
- Putting the old names back after a Rename Many cut short between the two halves of one rename
  now works, instead of leaving a photo under a hidden name and asking again at every start.
- Rename Folder while a collection is shown takes the collection's images in that folder along to
  the new name, in the collection's file too.
- A collection that another window emptied stays empty when this window saves it, instead of
  getting back every image.
- A failed delete puts back only the images it took out of the collection shown: not a folder that
  could not go to the trash, nor the images of another collection opened meanwhile; also when the
  same collection is opened again meanwhile.
- A PNG saved turned a quarter swaps its resolution across and down (pHYs), so a scan with unequal
  DPI does not print stretched.
- A PNG saved turned leaves out ImageMagick's copies of its EXIF and XMP ("Raw profile type"
  text), which still held the old orientation, so Geeqie and digiKam no longer turn it a second
  time.
- The man page says --name takes at most 64 bytes, and fewer in a Flatpak.
- The Bio-Formats decoder example cleans up its temporary folder and stops bfconvert when Glimt
  stops it after 30 seconds, instead of leaving them in /tmp.
- Find Duplicates by checksum counts its byte-by-byte check in the progress and stops it on
  Cancel, instead of sitting at 100 % while it reads every duplicate again.
- A key given to an action is taken from the action that had it, as the notice says: from another
  action's own key too, and a default key taken so no longer shows for that action in the menus
  and the Keyboard Map.

## [0.36.0] - 2026-10-18

### Added

- The AppStream file names each release and its date, so software centres show Glimt's version;
  the release workflow checks it is there, and publishes only a tag that is on main.

### Changed

- RAW thumbnails read the first 8 MB of the file, where cameras put the preview, and only read the
  rest when it is not there; showing a RAW file reads it once, not twice.
- Colour management changes an image a megabyte at a time: a large photo printed, exported or
  saved with its own profile no longer needs a second full-size copy.
- An image turned by its EXIF orientation is shrunk to the view first and then turned, not turned
  whole: a 100 MP PNG turned by EXIF no longer needs a second full-size buffer for a moment.
- Giving an action a key another action has says so: the new one takes it, and the other no longer
  answers to it.
- The Keyboard Map takes the list of keys once per frame, not twice for each key on it.
- The list of keys in use is made again only when an own key changes, not sorted many times in
  every frame.
- The size of a selection is worked out without making the selected files' paths in the window's
  thread, and Select All shows the folder's size, summed already.
- A mark: or stars: search, and Select by Mark, go from the marked files to the list instead of
  making a path for every file: in a folder of 500,000 each key press no longer costs tens of
  milliseconds.

### Fixed

- Printing when no image can be read (HEIC without ffmpeg, say) says so, and sends nothing to the
  printer nor puts an empty PDF in place of the file chosen.
- A thumbnail that failed because the file could not be read for a moment (a share or a stick
  away) is tried again later, instead of the file being marked as having none until it changes.
- ffprobe, which reads how a HEIC or AVIF photo is tiled, is stopped after 30 seconds as ffmpeg
  is: a file that kept it busy would hold the decoding with it.
- A file whose name is not UTF-8 (a Latin-1 "røntgen.dcm") is listed and shown by its decoder
  plugin, instead of being left out.
- Turning a JPEG changes the orientation in its XMP too, which Lightroom and digiKam write beside
  the EXIF's, so programs that read the XMP first no longer show it the old way.
- A --name too long for the socket's path (in a Flatpak from about 47 characters) is refused at
  the start with a message that says so, and a socket that cannot be made tells why, instead of
  saying a Glimt of that name runs.
- Find Duplicates by checksum compares the files of a group byte by byte before it offers them for
  the trash, instead of trusting a 64-bit hash.
- A plugin's command passes %% as %, as the desktop file specification says, instead of leaving it
  out.
- A collection renamed or moved to the trash in another window, or by another program, is not made
  again under its old name when this window changes it: Glimt says it is gone.
- A sidecar, collection or settings file written again keeps its permissions: one made private
  (0600) no longer becomes readable by others after a keyword edit.
- Saving settings keeps the settings file's comments, blank lines and order, so notes written in
  it by hand are no longer lost at the first change in a menu.
- The Help Manual names the thumbnail sizes as Preferences does: Small, Medium or Large.
- Files still being deleted when their folder is left and opened again no longer stay in its list
  once they are gone, also with auto refresh off.
- New Folder in a collection, or with several folders shown together, says it needs a folder
  shown, instead of trying to make the folder in /.
- The place [n/m] the status bar tells for an image in another pane is worked out again after a
  new search, which may have as many matches as the one before.

## [0.35.0] - 2026-10-18

### Changed

- The release tests allow 100 ms and 20 MB of slack beside the 1.5 times limit, as a few hundred
  ms vary by more than half when something else runs.

### Security

- An SVG takes at most 256 MB of linked or embedded pictures: a small one naming the same large
  picture a thousand times can no longer take all memory.
- A gzipped SVG (.svgz) is unpacked with a limit of 64 MB: one made of zeros, a few MB on disk,
  can no longer take all memory and end Glimt when its folder is opened. A gzipped SVG inside
  another SVG is not drawn.

### Fixed

- With Fit Width or Fit Height, an image shown larger after the window grew (or went full screen)
  is decoded again at the size shown, not at full resolution, which took many times the memory.
- An image taken out of a collection in one window no longer comes back when another window that
  shows the same collection saves it.
- A delete in a collection that fails, or is stopped, keeps the images that are still there in the
  collection, instead of taking them out of its file for good.
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
- A video, HEIC or plugin file that ffmpeg or the plugin cannot read is marked as having no
  thumbnail, as other files are, instead of being tried again on every visit; TypeScript sources
  named .ts, a video ending too, no longer start ffmpeg twice each time their folder opens. A
  program that is not installed still leaves no mark.
- The errors of ffmpeg and decoder plugins are read with a limit, the last 64 KB, so a damaged
  video cannot fill memory with them; and an error naming a file in another encoding is told, not
  lost.
- SVG thumbnails are drawn at the thumbnail's size, not at 2048 pixels and shrunk: a folder of
  icons gets its thumbnails many times faster, with a fraction of the memory.
- Closing Glimt waits for Export Frames, Save This Frame and Back Up Glimt's Data as it does for
  copies, and a frame is written whole or not at all, also over a file the dialog let it replace.
- A second file dialog does not open while one is open: its answer replaced the first one's, so a
  Copy to Folder could become a move, or nothing.
- Saving a JPEG's orientation reads and writes it in the background, as PNG and WebP already were,
  so the window no longer stops while a large photo on a share or a slow stick is written.
- Moving images of a collection no longer checks each file in the window's thread first, which
  froze it for seconds with thousands of files on a share; the collection follows the files that
  were moved, and only those.
- Rename Folder while several folders are shown together keeps the renamed folder's images in the
  list, read under its new name.
- Rename and a confirmed delete act when the folder was read again while their dialog was open, as
  long as the same files are meant, instead of saying the list changed: a folder another program
  writes to is read again every second.
- Rename Many keeps the view where it was: with Include Subfolders, several folders shown together
  or a slideshow with subfolders, it no longer opens the shown file's own folder.
- A screen reader hears the image's name as the list shows it, not the whole path, in a collection
  or several folders shown together.
- Back from a folder opened while a collection was shown goes back to the collection, not to the
  folder of its image.
- Save as Theme no longer writes over a theme file of the same name, which may be hand-edited, nor
  makes one named as a built-in theme, which would hide that one for good: it asks for another
  name.
- Rename with one file selected, by a right-click in the list say, renames that file, as Delete
  and Move take it, not the image shown beside it.
- When the folder is read again (another program writing to it, or Glimt's own sidecars and saves
  beside the files), the shown image keeps its zoom, turn and place and the images read ahead
  stay; a refresh in which nothing changed does nothing at all.
- README: it no longer promises that sidecars keep what another window wrote at the same moment;
  settings, collections, marks and the keyword tree do.
- Printing shrinks each image before it goes to sRGB, and decodes a JPEG at a fraction of its size
  when that is enough: a large photo with its own profile no longer takes hundreds of MB for a
  moment.
- A PNG saved turned keeps its text (a comment, the prompt an AI tool wrote), its DPI, its time
  and its gamma and colour space, instead of only its profile, EXIF and XMP: a 300 dpi scan no
  longer prints at 72 after a turn.

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

[Unreleased]: https://github.com/thornbux/glimt/compare/v0.80.0...HEAD
[0.80.0]: https://github.com/thornbux/glimt/compare/v0.79.0...v0.80.0
[0.79.0]: https://github.com/thornbux/glimt/compare/v0.78.0...v0.79.0
[0.78.0]: https://github.com/thornbux/glimt/compare/v0.77.0...v0.78.0
[0.77.0]: https://github.com/thornbux/glimt/compare/v0.76.0...v0.77.0
[0.76.0]: https://github.com/thornbux/glimt/compare/v0.75.0...v0.76.0
[0.75.0]: https://github.com/thornbux/glimt/compare/v0.74.0...v0.75.0
[0.74.0]: https://github.com/thornbux/glimt/compare/v0.73.0...v0.74.0
[0.73.0]: https://github.com/thornbux/glimt/compare/v0.72.0...v0.73.0
[0.72.0]: https://github.com/thornbux/glimt/compare/v0.71.0...v0.72.0
[0.71.0]: https://github.com/thornbux/glimt/compare/v0.70.0...v0.71.0
[0.70.0]: https://github.com/thornbux/glimt/compare/v0.69.0...v0.70.0
[0.69.0]: https://github.com/thornbux/glimt/compare/v0.68.0...v0.69.0
[0.68.0]: https://github.com/thornbux/glimt/compare/v0.67.0...v0.68.0
[0.67.0]: https://github.com/thornbux/glimt/compare/v0.66.1...v0.67.0
[0.66.1]: https://github.com/thornbux/glimt/compare/v0.66.0...v0.66.1
[0.66.0]: https://github.com/thornbux/glimt/compare/v0.65.1...v0.66.0
[0.65.1]: https://github.com/thornbux/glimt/compare/v0.65.0...v0.65.1
[0.65.0]: https://github.com/thornbux/glimt/compare/v0.64.0...v0.65.0
[0.64.0]: https://github.com/thornbux/glimt/compare/v0.63.0...v0.64.0
[0.63.0]: https://github.com/thornbux/glimt/compare/v0.62.0...v0.63.0
[0.62.0]: https://github.com/thornbux/glimt/compare/v0.61.0...v0.62.0
[0.61.0]: https://github.com/thornbux/glimt/compare/v0.60.0...v0.61.0
[0.60.0]: https://github.com/thornbux/glimt/compare/v0.59.0...v0.60.0
[0.59.0]: https://github.com/thornbux/glimt/compare/v0.58.0...v0.59.0
[0.58.0]: https://github.com/thornbux/glimt/compare/v0.57.0...v0.58.0
[0.57.0]: https://github.com/thornbux/glimt/compare/v0.56.0...v0.57.0
[0.56.0]: https://github.com/thornbux/glimt/compare/v0.55.0...v0.56.0
[0.55.0]: https://github.com/thornbux/glimt/compare/v0.54.0...v0.55.0
[0.54.0]: https://github.com/thornbux/glimt/compare/v0.53.0...v0.54.0
[0.53.0]: https://github.com/thornbux/glimt/compare/v0.52.0...v0.53.0
[0.52.0]: https://github.com/thornbux/glimt/compare/v0.51.0...v0.52.0
[0.51.0]: https://github.com/thornbux/glimt/compare/v0.50.0...v0.51.0
[0.50.0]: https://github.com/thornbux/glimt/compare/v0.49.0...v0.50.0
[0.49.0]: https://github.com/thornbux/glimt/compare/v0.48.0...v0.49.0
[0.48.0]: https://github.com/thornbux/glimt/compare/v0.47.0...v0.48.0
[0.47.0]: https://github.com/thornbux/glimt/compare/v0.46.0...v0.47.0
[0.46.0]: https://github.com/thornbux/glimt/compare/v0.45.0...v0.46.0
[0.45.0]: https://github.com/thornbux/glimt/compare/v0.44.0...v0.45.0
[0.44.0]: https://github.com/thornbux/glimt/compare/v0.43.2...v0.44.0
[0.43.2]: https://github.com/thornbux/glimt/compare/v0.43.1...v0.43.2
[0.43.1]: https://github.com/thornbux/glimt/compare/v0.43.0...v0.43.1
[0.43.0]: https://github.com/thornbux/glimt/compare/v0.42.2...v0.43.0
[0.42.2]: https://github.com/thornbux/glimt/compare/v0.42.1...v0.42.2
[0.42.1]: https://github.com/thornbux/glimt/compare/v0.42.0...v0.42.1
[0.42.0]: https://github.com/thornbux/glimt/compare/v0.41.1...v0.42.0
[0.41.1]: https://github.com/thornbux/glimt/compare/v0.41.0...v0.41.1
[0.41.0]: https://github.com/thornbux/glimt/compare/v0.40.1...v0.41.0
[0.40.1]: https://github.com/thornbux/glimt/compare/v0.40.0...v0.40.1
[0.40.0]: https://github.com/thornbux/glimt/compare/v0.39.0...v0.40.0
[0.39.0]: https://github.com/thornbux/glimt/compare/v0.38.5...v0.39.0
[0.38.5]: https://github.com/thornbux/glimt/compare/v0.38.4...v0.38.5
[0.38.4]: https://github.com/thornbux/glimt/compare/v0.38.3...v0.38.4
[0.38.3]: https://github.com/thornbux/glimt/compare/v0.38.2...v0.38.3
[0.38.2]: https://github.com/thornbux/glimt/compare/v0.38.1...v0.38.2
[0.38.1]: https://github.com/thornbux/glimt/compare/v0.38.0...v0.38.1
[0.38.0]: https://github.com/thornbux/glimt/compare/v0.37.2...v0.38.0
[0.37.2]: https://github.com/thornbux/glimt/compare/v0.37.1...v0.37.2
[0.37.1]: https://github.com/thornbux/glimt/compare/v0.37.0...v0.37.1
[0.37.0]: https://github.com/thornbux/glimt/compare/v0.36.1...v0.37.0
[0.36.1]: https://github.com/thornbux/glimt/compare/v0.36.0...v0.36.1
[0.36.0]: https://github.com/thornbux/glimt/compare/v0.35.0...v0.36.0
[0.35.0]: https://github.com/thornbux/glimt/compare/v0.34.1...v0.35.0
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
