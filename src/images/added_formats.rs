//! Added formats: programs that turn a file of a format Glimt does not read itself (JPEG 2000,
//! FITS, DICOM, OpenEXR …) into an image, so they need no code or library in Glimt. Each is a
//! desktop file in `glimt/formats` in the XDG data folders (`~/.local/share/glimt/formats`
//! first), or in the folder's old name `glimt/decoders` (`tools::RENAMED_FOLDERS`), read once
//! at start:
//!
//! ```text
//! [Glimt Format]
//! Name=JPEG 2000
//! Extensions=jp2;j2k
//! Exec=ffmpeg -v error -f j2k_pipe -i %f -frames:v 1 -f image2pipe -vcodec ppm -
//! ```
//!
//! The group's header is not checked, so files with the header that formats had before 0.76.0
//! are read as well. In `Exec`, `%f` is the file, also inside a word, and `%%` is a `%`.
//! The other `%` codes are kept for later meanings, so a file should not use them.
//!
//! The command gets the file's absolute path as one argument of its own, never through a shell,
//! and writes the image (PNM, PNG, TIFF, BMP …) to its standard output. It has 30 seconds and
//! at most `MAX_DECODE_BYTES` bytes, as ffmpeg has.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A format added by a desktop file, and the program that reads it.
pub struct AddedFormat {
    pub name: String,
    /// The endings it takes, with their dot and in lower case: ".jp2", ".nii.gz".
    exts: Vec<String>,
    exec: Vec<String>,
    /// The folder of the desktop file: a program there is found without being on the PATH.
    dir: PathBuf,
}

impl AddedFormat {
    /// Returns whether a file named `name` (in lower case) is of this format: by the end of the
    /// name, so an ending of two parts (".nii.gz") is found too.
    fn handles_name(&self, name: &str) -> bool {
        (self.exts.iter()).any(|e| name.ends_with(e.as_str()) && name != e)
    }

    /// The image the command writes for `path`.
    pub fn decode(&self, path: &Path) -> Result<image::DynamicImage, String> {
        self.run(path, crate::files::external::PROGRAM_TIME_LIMIT)
    }

    fn run(&self, path: &Path, limit: Duration) -> Result<image::DynamicImage, String> {
        // Absolute, so the path never reads as an option or as ImageMagick's "format:" prefix.
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        let args = self.arguments(&path);
        let program = PathBuf::from(&args[0]);
        let local = self.dir.join(&program);
        let program = if local.is_file() { local } else { program };
        let mut child =
            crate::files::external::stoppable_host_command(program.to_string_lossy().as_ref())
                .args(&args[1..])
                .spawn()
                .map_err(|e| format!("{} ({}): {e}", self.name, program.display()))?;
        let max = crate::images::decode::MAX_DECODE_BYTES + 64;
        let (image, err, ok) = crate::files::external::output_of(&mut child, max, limit);
        let image = image.ok_or_else(|| format!("{}: image too large", self.name))?;
        if !ok || image.is_empty() {
            let last = crate::files::external::last_line(&err, "no image");
            return Err(format!("{}: {last}", self.name));
        }
        crate::images::decode::from_memory(&image).map_err(|e| format!("{}: {e}", self.name))
    }

    /// Returns the command's words for the file at `path`: `%f` is the path, also inside a
    /// word, and `%%` is a `%`.
    fn arguments(&self, path: &Path) -> Vec<std::ffi::OsString> {
        (self.exec.iter())
            .map(|w| {
                let mut arg = std::ffi::OsString::new();
                for (n, part) in w.split("%f").enumerate() {
                    if n > 0 {
                        arg.push(path);
                    }
                    arg.push(part.replace("%%", "%"));
                }
                arg
            })
            .collect()
    }
}

/// Returns the installed formats, read once, in the order of their files' names without
/// ".desktop". When two take the same ending, the one first by name does, so dicom.desktop comes
/// before dicom-imagemagick.desktop. A desktop file in the user's folder hides one of the same
/// name in the system's. New ones are seen after a restart, as tools are.
pub fn installed_formats() -> &'static [AddedFormat] {
    static ALL: std::sync::OnceLock<Vec<AddedFormat>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        (crate::features::tools::desktop_files("formats").into_iter())
            .filter_map(|(path, text)| {
                let format = parse(&text, path.parent()?)?;
                Some((path.file_stem()?.to_owned(), format))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect()
    })
}

/// The added format of a desktop file. Returns `None` without a name, extensions, or %f in its
/// command (also inside a word, as ImageMagick's %f[0] for a file's first frame).
fn parse(text: &str, dir: &Path) -> Option<AddedFormat> {
    let value = crate::features::tools::desktop_entry_values(text);
    let exts: Vec<String> = (value("Extensions")?.split(';'))
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .collect();
    let exec = crate::features::tools::command_words(value("Exec")?);
    if exts.is_empty() || !exec.iter().any(|w| w.contains("%f")) {
        return None;
    }
    Some(AddedFormat {
        name: value("Name")?.to_owned(),
        exts,
        exec,
        dir: dir.to_owned(),
    })
}

/// The added format of `path`, by its extension, unless Glimt reads that format itself.
pub fn for_path(path: &Path) -> Option<&'static AddedFormat> {
    // No added formats, as for most: no work for each file of a folder of 500,000.
    if installed_formats().is_empty() {
        return None;
    }
    if crate::model::listing::built_in_format(path) {
        return None;
    }
    // Lossy: a name in another encoding (a Latin-1 "røntgen.dcm") still has its ending.
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    installed_formats().iter().find(|f| f.handles_name(&name))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    /// A format's desktop file read, a program run on a file of its format and its image taken;
    /// one that takes too long stopped, also when it ignores TERM; a file without %f or
    /// extensions left out.
    #[test]
    fn decodes_through_a_program() {
        let dir = std::env::temp_dir().join(format!("glimt-test-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A PPM under another name: `cat` is the program, the path given inside a word (x%f).
        let file = dir.join("a picture.xyz");
        image::RgbImage::new(6, 3)
            .write_to(
                &mut std::fs::File::create(&file).unwrap(),
                image::ImageFormat::Pnm,
            )
            .unwrap();
        let cat = super::parse(
            "[Glimt Decoder]\nName=Test\nExtensions=xyz; .XY2\nExec=sh -c \"cat \\\"${0#x}\\\"\" x%f\n",
            &dir,
        )
        .unwrap();
        assert_eq!(cat.exts, [".xyz", ".xy2"]);
        let nifti = super::parse("Name=NIfTI\nExtensions=nii;nii.gz\nExec=x %f", &dir).unwrap();
        assert!(nifti.handles_name("brain.nii.gz") && nifti.handles_name("brain.nii"));
        assert!(
            !nifti.handles_name("notes.gz")
                && !nifti.handles_name(".nii")
                && !nifti.handles_name("a.niigz")
        );
        let img = cat.decode(&file).unwrap();
        assert_eq!((img.width(), img.height()), (6, 3));
        // Nothing written: an error that names the format.
        let empty = super::parse("Name=Empty\nExtensions=xyz\nExec=true %f", &dir).unwrap();
        assert!(empty.decode(&file).unwrap_err().starts_with("Empty"));
        let slow = "Name=Slow\nExtensions=xyz\nExec=sh -c \"sleep 30\" %f";
        let slow = super::parse(slow, &dir).unwrap();
        let start = std::time::Instant::now();
        let error = slow.run(&file, Duration::from_millis(300)).unwrap_err();
        assert_eq!(
            error,
            format!("Slow: {}", crate::files::external::TIMED_OUT)
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        // One that ignores TERM: killed a second later.
        let stubborn = "Name=Stubborn\nExtensions=xyz\nExec=sh -c \"trap '' TERM; sleep 30\" %f";
        let stubborn = super::parse(stubborn, &dir).unwrap();
        let start = std::time::Instant::now();
        assert!(stubborn.run(&file, Duration::from_millis(300)).is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
        // One that leaves a helper holding its errors open: stopped too, not waited for.
        let left = "Name=Left\nExtensions=xyz\nExec=sh -c \"sleep 30 >/dev/null & exit 0\" %f";
        let left = super::parse(left, &dir).unwrap();
        let start = std::time::Instant::now();
        assert!(left.run(&file, Duration::from_millis(300)).is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(super::parse("Name=No file\nExtensions=xyz\nExec=cat", &dir).is_none());
        assert!(super::parse("Name=No ext\nExec=cat %f", Path::new("/")).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A format's desktop files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_format_files() {
        let seed = b"[Glimt Decoder]\nName=JPEG 2000\nExtensions=jp2;j2k\n\
                     Exec=ffmpeg -v error -i \"%f\" -f image2pipe -vcodec ppm -\n";
        for bytes in crate::support::test_support::broken(seed) {
            super::parse(&String::from_utf8_lossy(&bytes), Path::new("/"));
        }
    }

    /// The desktop file of an added format is frozen in 1.x: Name, Extensions (separated by ;,
    /// the dot optional, endings of two parts) and Exec, with %f for the file, also inside a
    /// word, and %% for a %. Its first group is read whatever its header says, so both
    /// [Glimt Format], which the examples write since 0.76.0, and [Glimt Decoder] are read.
    #[test]
    fn a_formats_desktop_keys_and_codes_are_kept() {
        let dir = Path::new("/formats");
        for header in ["[Glimt Format]", "[Glimt Decoder]"] {
            let text =
                format!("{header}\nName=Test\nExtensions=a; .B;nii.gz\nExec=conv %f[0] 100%%\n");
            let format = super::parse(&text, dir).expect(header);
            assert_eq!(format.name, "Test");
            assert_eq!(format.exts, [".a", ".b", ".nii.gz"]);
            let words = format.arguments(Path::new("/p/a.b"));
            assert_eq!(
                words,
                ["conv", "/p/a.b[0]", "100%"].map(std::ffi::OsString::from)
            );
        }
    }
}
