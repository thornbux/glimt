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

use std::io::Read;
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
        let home = crate::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt");
        let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".into());
        let mut seen = std::collections::HashSet::new();
        (home.into_iter())
            .chain(dirs.split(':').map(|d| Path::new(d).join("glimt")))
            .map(|d| d.join("decoders"))
            .flat_map(|dir| std::fs::read_dir(&dir).into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .filter(|p| seen.insert(p.file_name().map(|n| n.to_owned())))
            .filter_map(|p| {
                let decoder = parse(&std::fs::read_to_string(&p).ok()?, p.parent()?)?;
                Some((p.file_name()?.to_owned(), decoder))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect()
    })
}

/// The decoder of a desktop file; `None` without a name, extensions, or %f in its command (also
/// inside a word, as ImageMagick's %f[0] for a file's first frame).
fn parse(text: &str, dir: &Path) -> Option<Decoder> {
    let entry = text.split("\n[").next()?;
    let value = |key: &str| {
        (entry.lines())
            .find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('='))
            .map(str::trim)
    };
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
    if crate::files::own_ext(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_lowercase();
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
        self.run(path, Duration::from_secs(30))
    }

    fn run(&self, path: &Path, limit: Duration) -> Result<image::DynamicImage, String> {
        use std::process::Stdio;
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
        use std::os::unix::process::CommandExt;
        let mut child = crate::external::command(program.to_string_lossy().as_ref())
            .args(&args[1..])
            // A group of its own: a script's own children are stopped with it.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{} ({}): {e}", self.name, program.display()))?;
        // Stopped after `limit`: a file can keep a program busy for good.
        let id = child.id();
        let (done, timer) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            if timer.recv_timeout(limit).is_err() {
                // SAFETY: kill has no preconditions; the group is ours, and waited for below.
                unsafe { libc_kill(-(id as i32), 9) };
            }
        });
        let errors = child.stderr.take().map(|mut e| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = e.read_to_string(&mut text);
                text
            })
        });
        let max = crate::decode::MAX_DECODE + 64;
        let mut image = Vec::new();
        if let Some(out) = child.stdout.take() {
            let _ = out.take(max + 1).read_to_end(&mut image);
        }
        if image.len() as u64 > max {
            // SAFETY: as above.
            unsafe { libc_kill(-(id as i32), 9) };
        }
        let ok = child.wait().is_ok_and(|s| s.success());
        let _ = done.send(());
        let err = errors.and_then(|t| t.join().ok()).unwrap_or_default();
        if image.len() as u64 > max {
            return Err(format!("{}: image too large", self.name));
        }
        if !ok || image.is_empty() {
            let last = err.lines().last().unwrap_or("no image").trim().to_owned();
            return Err(format!("{}: {last}", self.name));
        }
        image::load_from_memory(&image).map_err(|e| format!("{}: {e}", self.name))
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, signal: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    /// A decoder file read, a program run on a file of its own format and its image taken; one
    /// that takes too long stopped; a file without %f or extensions left out.
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
