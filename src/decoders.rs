//! Decoder plugins: programs that turn a file of a format Glimt does not read itself (JPEG 2000,
//! FITS, DICOM, OpenEXR …) into an image, so they need no code or library in Glimt. Each is a
//! desktop file in `glimt/decoders` in the XDG data folders (`~/.local/share/glimt/decoders`
//! first), read once at start:
//!
//! ```text
//! [Glimt Decoder]
//! Name=JPEG 2000
//! Extensions=jp2;j2k
//! Exec=ffmpeg -v error -f j2k_pipe -i %f -frames:v 1 -f image2pipe -vcodec ppm -
//! ```
//!
//! The command gets the file's absolute path as one argument of its own, never through a shell,
//! and writes the image (PNM, PNG, TIFF, BMP …) to its standard output. It has 30 seconds and
//! at most `MAX_DECODE` bytes, as ffmpeg has.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Decoder {
    pub name: String,
    /// The endings it takes, with their dot and in lower case: ".jp2", ".nii.gz".
    exts: Vec<String>,
    exec: Vec<String>,
    /// The folder of the desktop file: a program there is found without being on the PATH.
    dir: PathBuf,
}

/// The decoders, read once, by the names of their files: when two take the same ending, the one
/// first by name does. A desktop file in the user's folder hides one of the same name in the
/// system's. New ones are seen after a restart, as plugins are.
pub fn all() -> &'static [Decoder] {
    static ALL: std::sync::OnceLock<Vec<Decoder>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        (crate::plugins::desktop_files("decoders").into_iter())
            .filter_map(|(path, text)| {
                let decoder = parse(&text, path.parent()?)?;
                Some((path.file_name()?.to_owned(), decoder))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect()
    })
}

/// The decoder of a desktop file; `None` without a name, extensions, or %f in its command (also
/// inside a word, as ImageMagick's %f[0] for a file's first frame).
fn parse(text: &str, dir: &Path) -> Option<Decoder> {
    let value = crate::plugins::values(text);
    let exts: Vec<String> = (value("Extensions")?.split(';'))
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .collect();
    let exec = crate::plugins::words(value("Exec")?);
    if exts.is_empty() || !exec.iter().any(|w| w.contains("%f")) {
        return None;
    }
    Some(Decoder {
        name: value("Name")?.to_owned(),
        exts,
        exec,
        dir: dir.to_owned(),
    })
}

/// The decoder for `path`, by its extension, unless Glimt reads that format itself.
pub fn for_path(path: &Path) -> Option<&'static Decoder> {
    // No decoders, as for most: no work for each file of a folder of 500,000.
    if all().is_empty() {
        return None;
    }
    if crate::files::built_in_format(path) {
        return None;
    }
    // Lossy: a name in another encoding (a Latin-1 "røntgen.dcm") still has its ending.
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    all().iter().find(|d| d.takes(&name))
}

impl Decoder {
    /// Whether a file named `name` (in lower case) is of this decoder's format: by the end of
    /// the name, so an ending of two parts (".nii.gz") is found too.
    fn takes(&self, name: &str) -> bool {
        (self.exts.iter()).any(|e| name.ends_with(e.as_str()) && name != e)
    }

    /// The image the command writes for `path`.
    pub fn decode(&self, path: &Path) -> Result<image::DynamicImage, String> {
        self.run(path, crate::external::LIMIT)
    }

    fn run(&self, path: &Path, limit: Duration) -> Result<image::DynamicImage, String> {
        // Absolute, so the path never reads as an option or as ImageMagick's "format:" prefix.
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        let args: Vec<std::ffi::OsString> = (self.exec.iter())
            .map(|w| {
                let mut arg = std::ffi::OsString::new();
                for (n, part) in w.split("%f").enumerate() {
                    if n > 0 {
                        arg.push(&path);
                    }
                    arg.push(part.replace("%%", "%"));
                }
                arg
            })
            .collect();
        let program = PathBuf::from(&args[0]);
        let local = self.dir.join(&program);
        let program = if local.is_file() { local } else { program };
        let mut child = crate::external::stoppable_command(program.to_string_lossy().as_ref())
            .args(&args[1..])
            .spawn()
            .map_err(|e| format!("{} ({}): {e}", self.name, program.display()))?;
        let max = crate::decode::MAX_DECODE + 64;
        let (image, err, ok) = crate::external::output_of(&mut child, max, limit);
        let image = image.ok_or_else(|| format!("{}: image too large", self.name))?;
        if !ok || image.is_empty() {
            let last = err.lines().last().unwrap_or("no image").trim().to_owned();
            return Err(format!("{}: {last}", self.name));
        }
        image::load_from_memory(&image).map_err(|e| format!("{}: {e}", self.name))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    /// A decoder file read, a program run on a file of its own format and its image taken; one
    /// that takes too long stopped, also when it ignores TERM; a file without %f or extensions left out.
    #[test]
    fn decodes_through_a_program() {
        let dir = std::env::temp_dir().join(format!("glimt-test-decoders-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A PPM under another name: `cat` is the decoder, the path given inside a word (x%f).
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
        assert!(nifti.takes("brain.nii.gz") && nifti.takes("brain.nii"));
        assert!(!nifti.takes("notes.gz") && !nifti.takes(".nii") && !nifti.takes("a.niigz"));
        let img = cat.decode(&file).unwrap();
        assert_eq!((img.width(), img.height()), (6, 3));
        // Nothing written: an error that names the decoder.
        let empty = super::parse("Name=Empty\nExtensions=xyz\nExec=true %f", &dir).unwrap();
        assert!(empty.decode(&file).unwrap_err().starts_with("Empty"));
        let slow = "Name=Slow\nExtensions=xyz\nExec=sh -c \"sleep 30\" %f";
        let slow = super::parse(slow, &dir).unwrap();
        let start = std::time::Instant::now();
        assert!(slow.run(&file, Duration::from_millis(300)).is_err());
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

    /// Decoder files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_decoder_files() {
        let seed = b"[Glimt Decoder]\nName=JPEG 2000\nExtensions=jp2;j2k\n\
                     Exec=ffmpeg -v error -i \"%f\" -f image2pipe -vcodec ppm -\n";
        for bytes in crate::broken(seed) {
            super::parse(&String::from_utf8_lossy(&bytes), Path::new("/"));
        }
    }
}
