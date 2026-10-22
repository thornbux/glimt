//! The Help menu's pages: manual, keyboard shortcuts, keyboard map and About.

use crate::actions::Action;
use crate::actions::shortcuts as all_shortcuts;
use eframe::egui;
use egui::Key;

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Manual,
    Shortcuts,
    Keymap,
    About,
}

impl Page {
    fn title(self) -> &'static str {
        // The window's title in the interface's language; the pages themselves stay English.
        crate::lang::tr(match self {
            Page::Manual => "Help Manual",
            Page::Shortcuts => "Keyboard Shortcuts",
            Page::Keymap => "Keyboard Map",
            Page::About => "About Glimt",
        })
    }
}

/// The open help page in a window; closing the window sets `page` to `None`.
pub fn window(ctx: &egui::Context, page: &mut Option<Page>) {
    let Some(shown) = *page else { return };
    let mut open = true;
    egui::Window::new(shown.title())
        .id(egui::Id::unique("help"))
        .open(&mut open)
        .collapsible(false)
        .default_height(520.0)
        .show(ctx, |ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical().show(ui, |ui| match shown {
                Page::Manual => manual(ui),
                Page::Shortcuts => shortcuts(ui),
                Page::Keymap => keymap(ui),
                Page::About => about(ui),
            });
        });
    if !open {
        *page = None;
    }
}

const MANUAL: &[(&str, &str)] = &[
    (
        "Opening images",
        "Start Glimt with a folder or an image (glimt ~/Pictures), drop one on the window, use \
         File → Open, type a path in the folder field above the folder list and press Enter, or \
         click a folder in the list. ⬆ .. goes to the parent folder.\n\n\
         Network shares work too: type smb://server/share (or sftp://…) and Glimt mounts it with \
         gio, asking for user, domain and password if the share needs them (a password saved in \
         the keyring from Files is used without asking).\n\n\
         Several folders show as one list:\n\n\
         - glimt ~/Holiday ~/Family\n\
         - File → Open Several Folders…\n\
         - Show Together with These Images in a folder's right-click menu.\n\n\
         A slideshow then goes through all of them, and random order mixes them.\n\n\
         The toolbar's arrows (Go → Back and Forward) go to the folders shown before, Up to the \
         parent and Home to the home folder. F5 (Go → Refresh) reads the folder again; with \
         Refresh the folder when files change (Preferences, on by default) Glimt does so itself, \
         looking once a second. With Start in the last used folder, Glimt opens that one when it \
         is given none.\n\n\
         Ctrl+N (File → New Window) starts another Glimt in the same folder; Ctrl+W and Ctrl+Q \
         close it. Starting Glimt a certain way, and controlling it from scripts, is under Command \
         line at the end.",
    ),
    (
        "Browsing",
        "The keys along the list:\n\n\
         - Space, Page Down and → show the next image\n\
         - Backspace, Page Up and ← the previous\n\
         - Home and End the first and last.\n\n\
         The images on each side are decoded in advance, so browsing is instant. Click a name in \
         the file list to show it.\n\n\
         In a TIFF with several pages, Ctrl+Page Down and Ctrl+Page Up go to the next and previous \
         page. In an animation (GIF, WebP, APNG) they stop it at the next or previous frame, and A \
         plays on.\n\n\
         For such a file, File → Save This Frame… saves the frame or page shown as a PNG, and \
         Export Frames… every one of them, at full size, in a new folder.",
    ),
    (
        "Zoom",
        "X fits the image to the window, Z shows it 1:1, + and - zoom. H fits the width and W the \
         height, and View → Zoom → Fill the View fills it (cut at the sides): these stay for the \
         next images until X.\n\n\
         The mouse wheel zooms towards the pointer; with Mouse wheel zooms off in Preferences it \
         browses, and Ctrl+wheel zooms. Preferences also say whether Zoom to Fit enlarges images \
         smaller than the window (on by default), whether the zoom and position are kept when \
         changing image, and the colour behind the image, or a checkerboard behind a transparent \
         one. A zoomed image larger than the window \
         can be dragged or moved with the arrow keys.\n\n\
         - ] and [ rotate the view\n\
         - Shift+R turns it upside down\n\
         - Shift+M mirrors and Shift+F flips it (Edit → Orientation).\n\n\
         The next image is shown as it is. Alt+X turns Auto rotate using EXIF off and on, to see \
         an image as it is stored.\n\n\
         Edit → Orientation → Save Orientation to File stores the rotation in a JPEG as its EXIF \
         orientation, without touching the pixels; a setting does it on every change of image. PNG \
         and WebP are turned and saved again with their EXIF data: PNG and lossless WebP without \
         loss, a lossy WebP as lossy again (through ffmpeg, at the quality set in Preferences).",
    ),
    (
        "Slideshow",
        "The keys of the slideshow:\n\n\
         - S starts and stops\n\
         - P pauses\n\
         - R switches random order on and off\n\
         - Ctrl+= and Ctrl+- change the delay.\n\n\
         Browsing during a slideshow gives the new image the full delay. With \"Include the images \
         in subfolders\" in Preferences it shows those too; the list goes back to the folder when \
         the slideshow stops.\n\n\
         Each image fades into the next, in every pane of a layout. Preferences sets the fade to \
         Off, Short (the default) or Long, and CPU use Power saving turns it off. Browsing by hand \
         shows the image at once.",
    ),
    (
        "Selecting and file operations",
        "In the grid of icons (Ctrl+I), drag a rectangle to select every image it touches, added \
         to the selection with Ctrl or Shift held (in a collection, start it between the images, \
         as dragging an image moves it).\n\n\
         - Ctrl+click toggles a file\n\
         - Shift+click or Shift with the arrow keys, Page Up/Down, Home and End selects a range\n\
         - Ctrl+A selects all (the matches while searching)\n\
         - Ctrl+Shift+A none\n\
         - Ctrl+Shift+I inverts the selection.\n\n\
         Copy, Move, Rename and Delete act on the selected files, otherwise on the shown image; \
         right-click a file for a menu with them. Delete moves files to the trash and Shift+Delete \
         deletes them for good. Both ask first, as set under Deleting in Preferences, which can \
         also turn the Delete key off or use no trash at all.\n\n\
         Edit → Undo (Ctrl+Z) puts back what this window did last: files or a folder moved to the \
         trash come back from it, moved files go back, renamed ones get their old names. Never over \
         a file that has taken the name since. Their marks and stars, and their place in a \
         collection, do not come back with them.\n\n\
         While files are copied, moved, deleted or given keywords, the status bar counts them \
         (Copying 120 of 3,000…), and Stop ends it after the file at hand (to the trash: after the \
         100 at hand); one that took long says when it is done. A copy is written under a hidden \
         name and gets its own when whole, and closing Glimt waits for these to end (close again \
         to stop them).\n\n\
         Rename with several files selected asks for a pattern: * is the old name and # a number \
         (### gives 001); existing files are never overwritten. If a crash or a power cut stops it \
         halfway, the next start offers to put the old names back or to finish.\n\n\
         File → Open Recent has the last ten folders and collections opened, in every window; \
         Clear Recent there empties it, and Preferences can keep none at all.\n\n\
         File → Open With (also in the right-click menu) opens them in another application, such \
         as GIMP, and Open With Default App in the desktop's own. File → Set as Wallpaper makes \
         the shown image the desktop's background, through the desktop portal (GNOME and KDE), \
         which keeps a copy of its own. Ctrl+Shift+C copies the image \
         itself to the clipboard, to paste into other programs. Select → Copy Files to Clipboard \
         copies the files, to paste in a file manager, and Copy Path to Clipboard their paths, \
         quoted for a terminal or unquoted.\n\n\
         Right-click a folder, in the list or the tree, to rename it or move it and all in it to \
         the trash; File → New Folder (Ctrl+F) makes one.",
    ),
    (
        "Finding and sorting",
        "Type three or more letters in the search field above the file list (/ puts the cursor \
         there, and shows the field if it was hidden): the list shows the matching names, best \
         first, ignoring case and gaps (like fzf).\n\n\
         Start with exif: to search the EXIF, keywords and comments instead, for example \
         exif:canon 2024:05 for photos from a Canon camera taken in May 2024; every word must \
         match. Start with re: for a regular expression instead, ignoring case: \
         re:^img_\\d+\\.jpe?g$, or exif:re:canon.*2024:0[5-7] for the EXIF.\n\n\
         A setting in Preferences makes the field a regular expression always, from the first \
         letter and in list order, as Geeqie's file filter; Case beside the field then tells upper \
         and lower case apart. The first EXIF search in a folder reads all files once, which takes \
         a while in large folders.\n\n\
         With the marks feature, 1–6 toggle a mark, Ctrl+1–6 select the files with it and Alt+0–5 \
         give stars. mark:2 or stars:3 in the search field shows those files; a setting also \
         writes the stars to XMP sidecars for darktable and digiKam.\n\n\
         These can be combined:\n\n\
         - terms one after the other must all match (sunset exif:canon stars:3)\n\
         - a - before exif:, re:, mark: or stars: means it must not (sunset -exif:iphone)\n\
         - | between terms means either (exif:canon | exif:nikon); | binds loosest.\n\n\
         Parentheses group, as words of their own with spaces around them: sunset ( exif:canon | \
         exif:nikon ), and -( … ) leaves a group out; a regular expression keeps its own \
         (re:^(IMG|DSC)). The words after a prefix belong to it, so a search of the names comes \
         first. Combined, the list is in its own order.\n\n\
         The sort menu at the bottom left sorts by name, number, date, EXIF date, size or \
         extension; the Types menu beside it chooses what the list shows: images, RAW files and \
         videos.",
    ),
    (
        "Keywords and comments",
        "The EXIF panel (Ctrl+K) shows the keywords and comment of the image below its EXIF; Alt+K \
         puts the cursor in the keywords, separated by commas.\n\n\
         They are saved when you leave the field, to the sidecar photo.jpg.xmp that darktable and \
         digiKam also read (as dc:subject and dc:description); keywords written into the image by \
         other programs are shown too. With several files selected, a button adds the keywords to \
         all of them. exif: searches find keywords and comments. Renaming, moving, copying and \
         deleting in Glimt take the sidecar along.\n\n\
         The keyword tree below them, as Geeqie's, holds keywords in levels (type \
         Places|Norway|Oslo and Add): a box is ticked when the image has that keyword, and a click \
         sets it on the selected images, or the shown one, or takes it off. The path goes into the \
         sidecar too, as lr:hierarchicalSubject, which darktable, digiKam and Lightroom read. \
         Right-click a keyword to remove it from the tree, with the levels below it.\n\n\
         Below the comment, Alt text and Extended description are IPTC's fields for accessibility \
         (Iptc4xmpCore:AltTextAccessibility and ExtDescrAccessibility): what the picture shows, \
         for someone who cannot see it. Screen readers and web pages read them; they are saved to \
         the sidecar too, when edited.",
    ),
    (
        "Printing",
        "File → Print (Ctrl+P) prints the selected images, or the shown one, one per page, as \
         large as fits or at an Image size of 10 to 100 %, or several on a page as a contact sheet \
         (Images per page: 2 to 20, in a grid). The dialog shows the first page as it will \
         print.\n\n\
         - Paper: A4, Letter, A3, A5, Legal, or photo paper of 10 × 15 and 13 × 18 cm\n\
         - Orientation: by the image (a wide one alone goes landscape), portrait or landscape\n\
         - Margin: 0 to 50 mm.\n\n\
         Fill each box cuts each image to its box instead of fitting it in. The text under each \
         image comes from a template, as the full screen info's: %name% by default, or %Taken%, \
         %Camera%, %keywords%, %comment% and the other fields. Text at the top and bottom of each \
         page may hold %page% and %pages%.\n\n\
         It goes to a printer through CUPS (lp), on both sides of the paper if the printer can, or \
         into a PDF file. Every choice is remembered.",
    ),
    (
        "Plugins",
        "Edit → Plugins runs a program on the selected files, or the shown \
         one, as Geeqie's plugins do, and shows them again afterwards.\n\n\
         A plugin is a desktop file in ~/.local/share/glimt/plugins (the menu opens that folder), \
         for example Name=Rotate and Exec=mogrify -rotate 90 %f:\n\n\
         - %f runs it once for each file\n\
         - %F once for all\n\
         - without either it runs once in the folder.\n\n\
         A program next to the desktop file is found there. New plugins show after a restart. The \
         files are given to the program directly, never through a shell.\n\n\
         /usr/share/doc/glimt/examples/plugins (examples/plugins in the tar.gz and the \
         source) has one that writes a description of each image as its \
         comment and alt text, made by a model on this computer, and one that reads the alt text \
         of the selected images aloud (speech-dispatcher), without a screen reader.",
    ),
    (
        "Formats",
        "Glimt reads:\n\n\
         - JPEG, PNG, WebP, GIF, WebP and PNG (APNG) animations\n\
         - TIFF (every page; grey TIFFs of floats from science stretched to show)\n\
         - BMP, ICO, QOI, PNM, Radiance HDR (tone-mapped), Zeiss LSM and .gel scans\n\
         - camera RAW files through their embedded preview\n\
         - HEIC, AVIF and video through ffmpeg, JPEG XL and SVG, in the builds that have them, as \
         the packages do.\n\n\
         A video shows as a still frame from it; Enter (File → Play Video) plays it in the player \
         set in Preferences, a command line such as mpv --fs, or else in the desktop's default \
         player.\n\n\
         A decoder plugin teaches Glimt a format it does not read itself, such as JPEG 2000, FITS, \
         DICOM or OpenEXR: a desktop file in ~/.local/share/glimt/decoders with Extensions=jp2;j2k \
         and an Exec command that writes the image (PNM, PNG …) to its standard output, %f being \
         the file. Such files are then listed and shown as any other, with thumbnails. The command \
         runs without a shell, for 30 seconds at most.\n\n\
         /usr/share/doc/glimt/examples/decoders (examples/decoders in the tar.gz and the \
         source) has decoders for:\n\n\
         - JPEG 2000 (ffmpeg)\n\
         - FITS from telescopes (astropy)\n\
         - DICOM from medical and dental imaging, also compressed (pydicom)\n\
         - FITS and DICOM through ImageMagick\n\
         - NIfTI brain scans\n\
         - whole pathology slides (OpenSlide)\n\
         - microscope files (Bio-Formats)\n\
         - cryo-EM maps (MRC) and HDF5, NetCDF-4 and NeXus data\n\
         - Photoshop, TGA and OpenEXR, and GIMP's own files (ImageMagick).\n\n\
         New decoders are seen after a restart.",
    ),
    (
        "Sort manager",
        "Shift+S shows the sort manager on the right, as in Geeqie: a list of folders, added with \
         Add This Folder or Add….\n\n\
         A click on a folder copies the selected files, or the shown one, there, or moves them \
         with Move chosen; × takes the folder off the list. Handy for sorting a card's photos into \
         albums.",
    ),
    (
        "Pan view",
        "Ctrl+J (View → Pan View) shows the images of the list as a \
         timeline, as Geeqie's pan view does: thumbnails under a heading for each day, oldest \
         first, by the file date or, with EXIF date ticked, the date the photo was taken.\n\n\
         A large folder shows the days read so far while the rest is read. A click shows the \
         image, a double click also closes the view, as Escape does.\n\n\
         Calendar shows the months that have images, a week to a row, each day with its first \
         image and how many it has; a click on a day shows it in the timeline. With Include \
         Subfolders it covers the whole tree.",
    ),
    (
        "Collections",
        "A collection is a list of images from any folders, as in Geeqie.\n\n\
         - C makes a new one of the selected files (or the shown image)\n\
         - File → Add to Collection (also in the right-click menu) adds to one\n\
         - File → Collections or O opens one.\n\n\
         The file list then shows its images, which browse, sort and play as a slideshow like a \
         folder. Delete takes the shown or selected images out of the collection, leaving the \
         files.\n\n\
         Drag them in the file list to another place (a line shows where they go), or Ctrl+Shift+↑ \
         and ↓ move them one place earlier or later, Ctrl+Shift+Home and End to the start or end \
         (File → Move in Collection), and the slideshow plays that order. Renaming and moving in \
         Glimt keep them in it.\n\n\
         File → Rename Collection… and Move Collection to Trash act on the open collection (not on \
         a plain list, which is yours); its images stay where they are. Each change is saved at \
         once, as a Geeqie .gqv file in \
         ~/.local/share/glimt/collections; Geeqie's own collections open too (O, or a .gqv file on \
         the command line).\n\n\
         A plain list opens the same way: a .txt or .m3u file with one image on each line, \
         relative to the list or absolute, # for comments, as a script or a music player writes \
         it; glimt --slideshow show.txt plays it in its order. Glimt never writes a list: what \
         changes in the window stays there.\n\n\
         File → Import Lightroom Catalog… reads a Lightroom Classic catalog (.lrcat, with \
         Lightroom closed) through the sqlite3 program:\n\n\
         - its collections become Glimt's (named with their sets, as Travel - Norway; one of the \
         same name gets the images it lacks)\n\
         - its keywords go into the keyword tree\n\
         - its stars into the marks.\n\n\
         Lightroom keeps the folders where they were on its computer: say where each is now. Next \
         then says what the import will do: the collections that are there already, and the images \
         whose stars in Glimt differ, where you choose whose stars to keep. Stars go to the XMP \
         sidecars too when they are set to.\n\n\
         A box (kept for the next import) also writes each image's keywords into its XMP sidecar, \
         with their place in the tree, beside the keywords it has: what Lightroom kept only in its \
         catalog, unless it was set to write XMP. Smart collections, colour labels and flags are \
         left out.",
    ),
    (
        "Backing up",
        "Marks, stars, collections, the keyword tree and the settings are in Glimt's own files, \
         not in the photos.\n\n\
         File → Back Up Glimt's Data… copies them to a new folder glimt-backup-DATE wherever you \
         choose, and glimt --back-up FOLDER does the same from a script or a timer, without a \
         window. To restore, close Glimt and copy them back (see the README).\n\n\
         Keywords, comments and alt text are in the XMP sidecars beside the photos, and go with a \
         backup of the photo folders.",
    ),
    (
        "Checking images",
        "File → Check Images… opens each image of the list in the background, as the viewer would, \
         and lists those that cannot be:\n\n\
         - broken files\n\
         - photos cut off in copying (a JPEG without its end, which shows grey at the bottom)\n\
         - files that are no image under an image's name.\n\n\
         They get an orange mark in the file list too, in the grid of thumbnails as well, until \
         another folder is shown.\n\n\
         A click on one shows it, Select Them selects them all, to move or delete; Stop or \
         Escape ends a long check. A folder of 100,000 photos takes some minutes.",
    ),
    (
        "Finding duplicates",
        "D (File → Find Duplicates) opens a window that compares the images in the list by:\n\n\
         - checksum (identical files; only files of the same size are read)\n\
         - name\n\
         - size\n\
         - dimensions\n\
         - how alike they look (95, 90 or 85%, from the thumbnails).\n\n\
         Turn on Include Subfolders first to search a whole tree.\n\n\
         The groups found are listed; click a name to show it, check the ones to remove (or check \
         all but the first of each group) and move them to the trash.",
    ),
    (
        "Themes",
        "View → Theme, or Theme at the top of Edit → Preferences, changes the look:\n\n\
         - Dark (the default)\n\
         - Light\n\
         - Desktop (light or dark and the accent colour as the desktop)\n\
         - Nord\n\
         - for low vision, High Contrast (white on black) and Contrast (black on white), both with \
         larger text.\n\n\
         Themes are small text files:\n\n\
         - Save as Theme… writes the look shown as one to edit\n\
         - Open Themes Folder shows where they go (~/.config/glimt/themes)\n\
         - Get More Themes… opens the collection on GitHub.\n\n\
         Without an accent colour of its own, a theme takes the desktop's. When the desktop asks \
         for high contrast (GNOME's and KDE's accessibility settings), Glimt shows High Contrast \
         whatever theme is chosen; a box in Preferences turns that off.\n\n\
         Preferences also sets the size of the menus, lists and text, from 50 to 300 %, and makes \
         them larger still when GNOME's Large Text or text scaling is on.\n\n\
         View → Filter shows the images and thumbnails in greyscale, inverted or with more \
         contrast, and Shift+G goes to the next filter; the files stay as they are.",
    ),
    (
        "Projector",
        "Window → Projector opens a second window with only the image, for another screen or a \
         projector, while you browse in this one: it follows the shown image, the slideshow and, \
         when zoomed, shows the same part of the image.\n\n\
         Drag it to the other screen and press F or double-click it for full screen there (Wayland \
         lets no program choose the screen). Esc leaves full screen, and the arrow keys and the \
         slideshow keys work in it too.\n\n\
         A yellow ring shows on it where the pointer is over the image here, to point things out; \
         Window → Show the Pointer on the Projector turns it off.",
    ),
    (
        "Layouts",
        "View → Layout shows several images at once: two side by side, two above each other, or \
         four (2 × 2). L changes to the next, U to two above each other and Y back to one.\n\n\
         The first pane is the shown image; the others show the images that follow it in the list, \
         so with random order (R) they are random too.\n\n\
         Zoom and moving a zoomed image apply to every pane, with the mouse wheel and dragging \
         over any of them, and a click on a pane or Alt+arrow makes the image there the shown one. \
         The status bar tells about the pane under the pointer (outlined), else the first: its \
         size, place and zoom, with a small map of the panes beside it that shows which.\n\n\
         For each layout, the slideshow either changes all images together or one at a time, round \
         the panes; choose in the same menu, as well as Show File Names, in every pane or none. \
         With full screen (F) the panes fill the screen, as two or four borderless pictures.",
    ),
    (
        "Panels",
        "What the window shows beside the image:\n\n\
         - T shows thumbnails (Small, Medium or Large in Preferences)\n\
         - Ctrl+I shows the file list as a grid of icons (↑ and ↓ move a row)\n\
         - Ctrl+T shows the folders as a tree\n\
         - View → Include Subfolders lists the images of every folder below (File → Open Folder \
         Recursive… opens a folder with it on)\n\
         - View → Show Hidden Files lists files and folders whose names start with a dot\n\
         - K shows a histogram (RGB, brightness, off) and J makes it logarithmic\n\
         - A stops and starts animations\n\
         - E compares side by side: the next image, or the other of two selected files, at the \
         same zoom and position\n\
         - Ctrl+K the EXIF panel (its GPS position opens a map)\n\
         - Ctrl+E a window with every EXIF field\n\
         - Ctrl+H hides the file list\n\
         - F or F11 shows the image full screen, Esc leaves full screen.\n\n\
         In full screen, I shows or hides the name, place and EXIF over the image. Preferences → \
         Image info template says what it shows, in Geeqie's form: %name%, %keywords%, %Camera% \
         and the other EXIF panel labels, | between parts that are left out when empty, \\n for a \
         new line.\n\n\
         Moving the pointer to the bottom shows a film strip of the images around the shown one; a \
         click on one shows it. Thumbnails are read from ~/.cache/thumbnails, which the file \
         manager shares; new ones are saved there only with Save new thumbnails on in \
         Preferences.\n\n\
         View also shows or hides the toolbar and the folder path, and Preferences the search \
         field and each file's size and date in the list. A click opens a menu; with one open, \
         pointing at another opens that, and it closes a moment after the pointer leaves. Full \
         screen keeps the screen saver off, unless Preferences say otherwise.",
    ),
    (
        "Colour",
        "Photos with an ICC profile (Adobe RGB, Display P3 …) are shown in their own colours: \
         converted to sRGB, or, for a calibrated or wide-gamut screen, straight to the screen \
         profile chosen under View → Colour Management (also in Preferences).\n\n\
         The desktop's screen profile, which GNOME and KDE keep in colord, goes before the chosen \
         one when there is one; Use the desktop's screen profile off leaves it out.\n\n\
         A photo without one whose EXIF says Adobe RGB, as from a camera set to it, is taken as \
         Adobe RGB; other images without one as sRGB, or the profile chosen there for them (Adobe \
         RGB, Display P3 or a file). Use the image's own profile off takes that one for every \
         image.\n\n\
         The EXIF panel (Ctrl+K) says which profile an image is shown from. The rendering intent \
         there decides how colours the target cannot show are brought in; only profiles with \
         tables for each intent (LUT profiles) tell them apart.\n\n\
         With a screen profile every image is converted, a few milliseconds each in the \
         background; files Glimt writes and the thumbnails it shares with other programs stay \
         sRGB.",
    ),
    (
        "Screen readers",
        "With the accessibility feature (in the packages), a screen reader such as Orca reads the \
         menus, buttons, lists and dialogs.\n\n\
         The image itself is read as its alt text, its name and place in the list, its stars, \
         keywords, comment and extended description, again each time you browse: the image area \
         keeps the keyboard focus while nothing else has it, and the arrow keys and every shortcut \
         still work there.\n\n\
         Tab moves to the panels and menus, and a ring in the accent colour shows where the \
         keyboard focus is; a click puts it away. Write the alt text yourself in the EXIF panel, \
         or let a plugin write it (see Plugins).",
    ),
    (
        "Preferences",
        "Edit → Preferences (Ctrl+O) has the settings: preloading, slideshow, thumbnails, the \
         theme, what is shown, window and memory use, and how deleting works.\n\n\
         Language: the interface in English or Norwegian bokmål, by the system's language (LANG) \
         or as chosen; a text not translated shows in English, and this manual stays English. \
         Under Keyboard shortcuts, Change gives an action the next key you press; \
         Reset gives it Geeqie's keys back.\n\n\
         Memory use at the top sets several at once:\n\n\
         - Low decodes one image at a time and preloads one on each side\n\
         - Medium (the default) four and two\n\
         - High eight and five.\n\n\
         Details shows them, and other values are Custom. On slow storage such as SMB or NFS \
         shares, Medium or High keep browsing quick, as the next images are read while you look; \
         Low waits for each file.\n\n\
         CPU use below it:\n\n\
         - Power saving makes thumbnails one at a time and runs searches, the EXIF index, Find \
         Duplicates and pan view on two cores, kinder to a laptop battery\n\
         - Balanced (the default) makes two thumbnails at once and runs the rest on every core\n\
         - Full makes thumbnails on every core too.\n\n\
         It takes effect at once.",
    ),
    (
        "Command line",
        "glimt [OPTIONS] [FILE_OR_FOLDER …] opens a file with the rest of its folder, a folder at \
         its first image, or several folders as one list (glimt ~/Holiday ~/Family).\n\n\
         The options before them start it a certain way: glimt --fullscreen --layout 2x2 --random \
         --slideshow ~/Pictures shows four random images at a time on the whole screen, and glimt \
         --slideshow show.txt plays a list in its order.\n\n\
         glimt --remote COMMAND sends a command to the Glimt used last, glimt --remote-all to \
         every one: next, prev, slideshow or open PATH from a script or a desktop shortcut. \
         current, selection and select PATH … tell a script (or an AI assistant) what is shown and \
         selected, or select files for you; they print each path followed by a NUL byte, and none \
         of them changes a file.\n\n\
         They reach only the Glimt windows of the same user on this computer, through a private \
         socket, never over the network; for another computer, run the command there (ssh). \
         Several can be told apart by name: start them with glimt --name left and glimt --name \
         right, and glimt --name left --remote slideshow reaches only the left one.\n\n\
         glimt --kiosk is for a picture screen: full screen with only the image, browsing and the \
         slideshow but nothing that changes files, and no way out but glimt --remote quit (in a \
         kiosk session such as cage or GNOME Kiosk, the desktop's keys are gone too). Where remote \
         control cannot work, without $XDG_RUNTIME_DIR, Ctrl+Q closes it.\n\n\
         glimt --lock-settings, or a file /etc/glimt/lock-settings put there by the administrator, \
         is for a shared computer: Preferences do not open, and what is changed in the menus lasts \
         only until Glimt closes.\n\n\
         glimt --check-config tells what is wrong in the settings file, line by line; Glimt also \
         tells the first of it when it starts. man glimt says more, with examples. \
         The options and commands:",
    ),
];

/// Where Glimt differs from Geeqie: what, Glimt, Geeqie. Shown in the Help Manual (F1).
const DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "Language",
        "English or Norwegian bokmål, by LANG or chosen in Preferences; the manual in English",
        "Some 40 languages by LANG only",
    ),
    (
        "R",
        "Toggles random order in the slideshow, also while it runs",
        "Refresh",
    ),
    ("Refresh", "F5", "R"),
    (
        "Remote control",
        "glimt --remote with next, prev, first, last, slideshow, random, fullscreen (each also \
         with on or off), subfolders on|off, open, quit, current, selection or select; \
         --remote-all sends it to every window",
        "The options themselves go to the running Geeqie (geeqie --next, --tell, \
         --get-selection, --file= …), and there are many more",
    ),
    (
        "Shift+G",
        "Goes to the next view filter: greyscale, inverted, more contrast, none",
        "Greyscale on and off",
    ),
    (
        "Random order",
        "R, the View menu and Preferences",
        "Only in Preferences",
    ),
    (
        "Arrow keys",
        "←/↑ previous, →/↓ next image; they move a zoomed image that is larger than the window",
        "Scroll a zoomed image, move in the file list",
    ),
    (
        "Mouse wheel",
        "Zooms by default; a setting makes it go to the next or previous image",
        "Next or previous image",
    ),
    (
        "Esc / Q",
        "Stops the slideshow and leaves full screen; never quits",
        "Leaves full screen",
    ),
    (
        "Slideshow delay",
        "Whole seconds 1–60, Ctrl+= / Ctrl+- change it by 1 s",
        "Tenths of a second, Faster/Slower change it by 0.5 s",
    ),
    (
        "Browsing in a slideshow",
        "The new image gets the full delay",
        "",
    ),
    (
        "Small images",
        "\"Enlarge small images to fit the window\", on by default",
        "\"Allow enlargement\"",
    ),
    (
        "Order of a collection",
        "Drag and drop in the file list, or Ctrl+Shift+↑, ↓, Home and End; saved at once",
        "Drag and drop in the collection window",
    ),
    (
        "Close Window (Ctrl+W)",
        "Same as Quit (one window per process)",
        "Closes one of several windows",
    ),
    (
        "Keypad / and *",
        "/ puts the cursor in the search field, as the other /; * does nothing (+ and - zoom, as in Geeqie)",
        "Zoom 1:1 and fit",
    ),
    (
        "Marks and stars (marks feature)",
        "1–6 toggle, Ctrl+1–6 select, Alt+0–5 stars (main keyboard); kept in ~/.local/state/glimt/marks; mark: and stars: in the search field",
        "Alt+keypad for stars, kept in XMP; marks filter buttons",
    ),
    (
        "Copy, Move (Ctrl+C, Ctrl+M)",
        "Target folder from a file dialog; never overwrites",
        "Own dialog",
    ),
    (
        "New Window (Ctrl+N)",
        "A new process in the same folder",
        "A new window in the same process, saved layouts",
    ),
    (
        "Keywords and comment",
        "In the EXIF panel (Ctrl+K) with a keyword tree, whose boxes show the shown image's \
         keywords; kept in the sidecar photo.jpg.xmp; found by exif: searches",
        "Info sidebar with a keyword tree that can also mark images and hide keywords; kept in \
         the image, a sidecar or Geeqie's own folder",
    ),
    (
        "Print (Ctrl+P)",
        "Its own dialog with a preview of the first page: 1 or 2 to 20 images per page, image and \
         page text from templates, paper, orientation, margins, two-sided; to a CUPS printer or \
         a PDF file; no font choice",
        "GTK print dialog: several images per page, image and page text with a font, page setup",
    ),
    (
        "Set as Wallpaper",
        "File → Set as Wallpaper, through the desktop portal",
        "A plugin (Edit → Plugins)",
    ),
    (
        "Open Recent",
        "The last ten folders and collections",
        "Collections only",
    ),
    (
        "Plugins",
        "Desktop files in ~/.local/share/glimt/plugins, in Edit → Plugins; no hotkeys or menu paths",
        "Desktop files in geeqie/applications, also with hotkeys, filters and menu paths",
    ),
    (
        "Sort manager (Shift+S)",
        "Copies or moves; kept in ~/.config/glimt/sort-folders",
        "Also links, copies the path, and undoes the last one",
    ),
    (
        "Pan view (Ctrl+J)",
        "A timeline by day, or a calendar, in a window over the main one",
        "Its own window: timeline, calendar, folder tree and grid, with zoom",
    ),
    (
        "Collections (C, O)",
        "Shown in the file list of the main window; C makes one of the selected files; saved at every change",
        "Own window: C opens an empty one, S saves, drag to change the order",
    ),
    (
        "Find Duplicates (D)",
        "Compares the images in the list; checked files go to the trash",
        "Files and folders dropped on its window; more methods, two sets, results to a collection",
    ),
    (
        "Layouts (View → Layout)",
        "1, 2 side by side, 2 above each other or 2 × 2, from the list; the slideshow changes them together or in turn",
        "Split view (E, U, Y) with a file list for each pane; one image in a slideshow",
    ),
    (
        "Images as icons (Ctrl+I)",
        "Ctrl+I switches between list and icons",
        "Ctrl+I icons, Ctrl+L list",
    ),
    (
        "Search field",
        "Above the file list; fuzzy (like fzf) by default, re: for a regular expression, exif:, \
         mark: and stars:, combined with -, | and ( ); a setting makes it a regular expression \
         always, as Geeqie's, with a Case checkbox beside it",
        "File filter (View → Show File Filter): a regular expression, with a Case checkbox",
    ),
    (
        "Undo (Ctrl+Z)",
        "The last move to the trash, move or rename in this window",
        "None",
    ),
    (
        "Folders",
        "Right-click in the list or the tree: rename, or move to the trash with all in it; never \
         deleted permanently",
        "Rename and delete (to the trash or permanently, as for files) in the folder pane's menu",
    ),
    (
        "Fit the width (H) and height (W)",
        "Modes that stay for the next images until X, with Fill the View as a third, as \
         ImageGlass has them",
        "One zoom of the shown image",
    ),
    (
        "Menu bar",
        "With a menu open, pointing at another opens it; the menu closes a moment after the \
         pointer leaves the bar and the menus",
        "Pointing at another opens it too; a menu stays open until a click",
    ),
    (
        "File types",
        "Types menu beside the sort menu: images, RAW and videos; a file opened directly is \
         listed whatever it says",
        "File classes in the file filter, and each file type in Preferences → File Filters",
    ),
    (
        "Hidden files",
        "View → Show Hidden Files, for files and folders; off by default; no key (Ctrl+H hides \
         the file list)",
        "Preferences → Files, and the folder pane's menu",
    ),
    (
        "Compare (E)",
        "E shows the next image (or the other of two selected) beside the shown one, at the same zoom \
         and offset; U and Y as in Geeqie; L the next layout; a click on a pane or Alt+arrow shows \
         the image there in the first pane",
        "E, U and Y split into panes with an image each; Y back to one",
    ),
    (
        "Colour management",
        "The image's own profile, Adobe RGB from its EXIF, or a chosen one for images without, \
         to sRGB or a screen profile, the desktop's from colord when it has one; the profile \
         used is in the EXIF panel; absolute colorimetric is shown as relative colorimetric",
        "The same, with the profile used in the status bar, the desktop's from X \
         (_ICC_PROFILE, so not on Wayland), and absolute colorimetric keeping the paper white \
         of the profile",
    ),
    (
        "Files on the clipboard",
        "Select → Copy Files to Clipboard copies them for a file manager; no Cut (Ctrl+X)",
        "Ctrl+C copies, Ctrl+X cuts",
    ),
    (
        "Histogram (K, J)",
        "In the image's corner: K cycles off, RGB and brightness, J logarithmic",
        "In the overlay: K cycles channels, J modes",
    ),
    (
        "Image info in full screen (I)",
        "On or off; its template has no :limit, :extra or markup",
        "I cycles through several overlay layouts; :limit, :extra and markup in the template",
    ),
    (
        "Thumbnail cache",
        "The shared ~/.cache/thumbnails; new ones are saved only if the setting is on",
        "Own cache, or the shared one",
    ),
    (
        "Refresh on change",
        "Setting, on by default; checks the folder once a second",
        "Setting",
    ),
];

fn manual(ui: &mut egui::Ui) {
    for (heading, text) in MANUAL {
        ui.strong(*heading);
        ui.label(*text);
        // The same lists as glimt --help, so they are never behind.
        if *heading == "Command line" {
            table(ui, "options", crate::START_OPTIONS);
            ui.add_space(4.0);
            table(ui, "commands", crate::remote::COMMANDS);
        }
        ui.add_space(6.0);
    }
    egui::CollapsingHeader::new("Differences from Geeqie").show(ui, |ui| {
        egui::Grid::new("differences").striped(true).show(ui, |ui| {
            ui.strong("");
            ui.strong("Glimt");
            ui.strong("Geeqie");
            ui.end_row();
            for (what, glimt, geeqie) in DIFFERENCES {
                ui.label(*what);
                ui.label(*glimt);
                ui.label(*geeqie);
                ui.end_row();
            }
        });
    });
}

/// Two columns, the first in code: options or commands and what they do.
fn table(ui: &mut egui::Ui, id: &str, rows: &[(&str, &str)]) {
    egui::Grid::new(id).striped(true).show(ui, |ui| {
        for (what, does) in rows {
            ui.code(*what);
            ui.label(*does);
            ui.end_row();
        }
    });
}

/// Mouse use, for the help window.
const MOUSE: &[(&str, &str)] = &[
    ("Mouse wheel", "Zoom, or next/previous image (Preferences)"),
    ("Ctrl+mouse wheel", "Zoom"),
    ("Drag", "Move a zoomed image"),
    ("Double-click", "Full Screen"),
    ("Ctrl+click in the list", "Select or unselect a file"),
    (
        "Shift+click in the list",
        "Select the files from the last one clicked",
    ),
    (
        "Drag in the grid of icons",
        "Select with a rectangle (Ctrl or Shift: add to the selection)",
    ),
    (
        "Drag in a collection's list",
        "Move the images to another place",
    ),
];

/// Every shortcut of an action on one row, in menu order, then the mouse.
fn shortcuts(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let mut rows: Vec<(Action, Vec<String>)> = Vec::new();
    for &(action, mods, key) in all_shortcuts().iter() {
        if action == Action::Play && !cfg!(feature = "video") {
            continue;
        }
        let text = ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key));
        match rows.iter_mut().find(|r| r.0 == action) {
            Some(row) => row.1.push(text),
            None => rows.push((action, vec![text])),
        }
    }
    rows.sort_by_key(|r| r.0 as usize);
    egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
        for (action, keys) in rows {
            ui.label(keys.join(", "));
            ui.label(action.label());
            ui.end_row();
        }
        for (what, does) in MOUSE {
            ui.label(*what);
            ui.label(*does);
            ui.end_row();
        }
    });
}

/// A keyboard with the keys Glimt uses highlighted; hover a key to see what it does.
fn keymap(ui: &mut egui::Ui) {
    use Key::*;
    // (label, key, width in key units); `None` keys are drawn but never used.
    type Row = &'static [(&'static str, Option<Key>, f32)];
    const ROWS: [Row; 6] = [
        &[
            ("Esc", Some(Escape), 1.0),
            ("", None, 0.5),
            ("F1", Some(F1), 1.0),
            ("F2", Some(F2), 1.0),
            ("F3", Some(F3), 1.0),
            ("F4", Some(F4), 1.0),
            ("", None, 0.5),
            ("F5", Some(F5), 1.0),
            ("F6", Some(F6), 1.0),
            ("F7", Some(F7), 1.0),
            ("F8", Some(F8), 1.0),
            ("", None, 0.5),
            ("F9", Some(F9), 1.0),
            ("F10", Some(F10), 1.0),
            ("F11", Some(F11), 1.0),
            ("F12", Some(F12), 1.0),
        ],
        &[
            ("`", Some(Backtick), 1.0),
            ("1", Some(Num1), 1.0),
            ("2", Some(Num2), 1.0),
            ("3", Some(Num3), 1.0),
            ("4", Some(Num4), 1.0),
            ("5", Some(Num5), 1.0),
            ("6", Some(Num6), 1.0),
            ("7", Some(Num7), 1.0),
            ("8", Some(Num8), 1.0),
            ("9", Some(Num9), 1.0),
            ("0", Some(Num0), 1.0),
            ("-", Some(Minus), 1.0),
            ("=", Some(Equals), 1.0),
            ("Bksp", Some(Backspace), 2.0),
        ],
        &[
            ("Tab", Some(Tab), 1.5),
            ("Q", Some(Q), 1.0),
            ("W", Some(W), 1.0),
            ("E", Some(E), 1.0),
            ("R", Some(R), 1.0),
            ("T", Some(T), 1.0),
            ("Y", Some(Y), 1.0),
            ("U", Some(U), 1.0),
            ("I", Some(I), 1.0),
            ("O", Some(O), 1.0),
            ("P", Some(P), 1.0),
            ("[", Some(OpenBracket), 1.0),
            ("]", Some(CloseBracket), 1.0),
            ("\\", Some(Backslash), 1.5),
        ],
        &[
            ("Caps", None, 1.75),
            ("A", Some(A), 1.0),
            ("S", Some(S), 1.0),
            ("D", Some(D), 1.0),
            ("F", Some(F), 1.0),
            ("G", Some(G), 1.0),
            ("H", Some(H), 1.0),
            ("J", Some(J), 1.0),
            ("K", Some(K), 1.0),
            ("L", Some(L), 1.0),
            (";", Some(Semicolon), 1.0),
            ("'", Some(Quote), 1.0),
            ("Enter", Some(Enter), 2.25),
        ],
        &[
            ("Shift", None, 2.25),
            ("Z", Some(Z), 1.0),
            ("X", Some(X), 1.0),
            ("C", Some(C), 1.0),
            ("V", Some(V), 1.0),
            ("B", Some(B), 1.0),
            ("N", Some(N), 1.0),
            ("M", Some(M), 1.0),
            (",", Some(Comma), 1.0),
            (".", Some(Period), 1.0),
            ("/", Some(Slash), 1.0),
            ("Shift", None, 2.75),
        ],
        &[
            ("Ctrl", None, 1.5),
            ("Alt", None, 1.5),
            ("Space", Some(Space), 7.0),
            ("Alt", None, 1.5),
            ("Ctrl", None, 1.5),
        ],
    ];
    const NAV: [[(&str, Key); 3]; 2] = [
        [("Ins", Insert), ("Home", Home), ("PgUp", PageUp)],
        [("Del", Delete), ("End", End), ("PgDn", PageDown)],
    ];
    const ARROWS: [(&str, Key, f32, f32); 4] = [
        ("↑", ArrowUp, 1.0, 0.0),
        ("←", ArrowLeft, 0.0, 1.0),
        ("↓", ArrowDown, 1.0, 1.0),
        ("→", ArrowRight, 2.0, 1.0),
    ];

    ui.label("Highlighted keys do something; bright ones also without Ctrl, Shift or Alt.");
    ui.label("Hover a key to see its actions.");
    ui.add_space(8.0);
    let unit = 34.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(unit * 19.5, unit * 6.6), egui::Sense::hover());
    // Once for the map, not twice for each of its keys.
    let all = all_shortcuts();
    let draw = |ui: &mut egui::Ui, label: &str, key: Option<Key>, at: egui::Rect| {
        let actions: Vec<String> = key
            .map(|k| {
                all.iter()
                    .copied()
                    .filter(|s| s.2 == k && (s.0 != Action::Play || cfg!(feature = "video")))
                    .map(|(a, m, k)| {
                        let keys = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(m, k));
                        format!("{keys}: {}", a.label())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let plain =
            key.is_some_and(|k| all.iter().any(|s| s.2 == k && s.1 == egui::Modifiers::NONE));
        let v = ui.visuals();
        let fill = if plain {
            v.selection.bg_fill
        } else if !actions.is_empty() {
            v.selection.bg_fill.gamma_multiply(0.45)
        } else {
            v.widgets.inactive.weak_bg_fill
        };
        let r = at.shrink(2.0);
        ui.painter().rect_filled(r, 4.0, fill);
        let color = if actions.is_empty() {
            v.weak_text_color()
        } else {
            v.strong_text_color()
        };
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
        if !actions.is_empty() {
            let id = ui
                .scope_id()
                .with(("key", label, at.min.x as i32, at.min.y as i32));
            ui.interact(r, id, egui::Sense::hover())
                .on_hover_text(actions.join("\n"));
        }
    };
    for (y, row) in ROWS.iter().enumerate() {
        let mut x = 0.0;
        // A gap under the function keys, as on a keyboard.
        let top = rect.min.y + y as f32 * unit + if y > 0 { unit * 0.3 } else { 0.0 };
        for &(label, key, width) in *row {
            let at = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + x * unit, top),
                egui::vec2(width * unit, unit),
            );
            if !label.is_empty() {
                draw(ui, label, key, at);
            }
            x += width;
        }
    }
    let left = rect.min.x + 15.5 * unit;
    for (y, row) in NAV.iter().enumerate() {
        for (x, &(label, key)) in row.iter().enumerate() {
            let min = egui::pos2(left + x as f32 * unit, rect.min.y + (1.3 + y as f32) * unit);
            draw(
                ui,
                label,
                Some(key),
                egui::Rect::from_min_size(min, egui::vec2(unit, unit)),
            );
        }
    }
    for (label, key, x, y) in ARROWS {
        let min = egui::pos2(left + x * unit, rect.min.y + (4.3 + y) * unit);
        draw(
            ui,
            label,
            Some(key),
            egui::Rect::from_min_size(min, egui::vec2(unit, unit)),
        );
    }
}

/// Libraries and assets Glimt is built with, and their licenses.
const CREDITS: &[(&str, &str, &str)] = &[
    (
        "egui / eframe",
        "Emil Ernerfeldt and contributors",
        "MIT or Apache-2.0",
    ),
    (
        "winit, glutin",
        "The winit and glutin contributors",
        "Apache-2.0",
    ),
    ("sctk-adwaita", "Window frame on Wayland", "MIT"),
    (
        "AccessKit (accessibility builds)",
        "The AccessKit contributors: screen readers",
        "MIT or Apache-2.0",
    ),
    (
        "image, png, zune-jpeg",
        "The image-rs and zune contributors",
        "MIT or Apache-2.0",
    ),
    ("fast_image_resize", "Kirill Kuzminykh", "MIT or Apache-2.0"),
    (
        "jpeg-decoder, tiff",
        "The image-rs contributors",
        "MIT or Apache-2.0; MIT",
    ),
    ("moxcms", "Radzivon Bartoshyk", "BSD-3-Clause or Apache-2.0"),
    ("regex", "The Rust Project Developers", "MIT or Apache-2.0"),
    ("arboard", "The arboard contributors", "MIT or Apache-2.0"),
    (
        "resvg, usvg (SVG builds)",
        "The resvg authors",
        "Apache-2.0 or MIT",
    ),
    (
        "jxl-oxide (JPEG XL builds)",
        "Wonwoo Choi",
        "MIT or Apache-2.0",
    ),
    (
        "Inter",
        "The Inter Project Authors",
        "SIL Open Font License 1.1",
    ),
    ("Phosphor Icons", "Phosphor Icons", "MIT"),
    ("Ubuntu font", "Dalton Maag", "Ubuntu Font Licence 1.0"),
    ("Noto Emoji", "Google", "SIL Open Font License 1.1"),
    ("emoji-icon-font, Hack", "Font authors", "MIT"),
    (
        "ffmpeg (optional, run as a program)",
        "The FFmpeg developers",
        "LGPL-2.1 or GPL",
    ),
];

fn about(ui: &mut egui::Ui) {
    // The logo as tall as the five lines beside it, so the two line up.
    ui.horizontal(|ui| {
        if let Some(logo) = logo(ui.ctx()) {
            let tall = 5.0 * ui.text_style_height(&egui::TextStyle::Body)
                + 4.0 * ui.spacing().item_spacing.y;
            ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(tall, tall)));
        }
        ui.vertical(|ui| {
            ui.heading(format!("Glimt {}", env!("CARGO_PKG_VERSION")));
            ui.label("A fast, memory-frugal image viewer for Linux.");
            ui.label("Glimt is Norwegian for \"glimpse\": a quick look.");
            ui.weak("Copyright © 2026 Hans-Henry Jakobsen");
            // Opened by the desktop's browser (gio, xdg-open), as Show on OpenStreetMap.
            ui.horizontal(|ui| {
                for (text, url) in [
                    ("glimt.pario.no", "https://glimt.pario.no"),
                    (
                        "MIT License",
                        "https://github.com/thornbux/glimt/blob/main/LICENSE",
                    ),
                ] {
                    if ui.link(text).on_hover_text(url).clicked() {
                        crate::external::open_url(url);
                    }
                }
            });
        });
    });
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Credits")
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Inspired by Geeqie (geeqie.org); Glimt shares no code with it.");
            ui.label("Written with Claude Code (Anthropic).");
            ui.add_space(4.0);
            egui::Grid::new("credits").striped(true).show(ui, |ui| {
                for (what, who, license) in CREDITS {
                    ui.label(*what);
                    ui.weak(*who);
                    ui.label(*license);
                    ui.end_row();
                }
            });
        });
    egui::CollapsingHeader::new("License (MIT)").show(ui, |ui| {
        ui.label(include_str!("../LICENSE"));
    });
}

/// Glimt's logo as a texture: made the first time About is shown, then kept in egui's memory.
fn logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique("glimt-logo");
    if let Some(logo) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(logo);
    }
    let img = image::load_from_memory(crate::LOGO).ok()?.into_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let logo = ctx.load_texture("glimt-logo", pixels, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, logo.clone()));
    Some(logo)
}
