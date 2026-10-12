//! Printing, as Geeqie's File → Print: the images one per page in a PDF, which CUPS prints
//! (`lp`) or which is saved as a file. The PDF is written here, with the JPEG of each image and
//! the file name under it in Helvetica, which every PDF reader has.
//! ponytail: one image per page; Geeqie also puts several on a page.

use std::io::Write;

/// Paper sizes in points (1/72 inch), portrait.
pub const PAPERS: [(&str, [f32; 2]); 2] = [("A4", [595.28, 841.89]), ("Letter", [612.0, 792.0])];

/// Margin around the image, 10 mm.
const MARGIN: f32 = 28.35;
/// Room for the caption below the image.
const CAPTION: f32 = 18.0;
/// Pixels per inch printed; more only makes the PDF larger.
const PPI: f32 = 300.0;

/// The longest side in pixels an image needs on `paper`.
pub fn max_pixels(paper: [f32; 2]) -> u32 {
    ((paper[0].max(paper[1]) - 2.0 * MARGIN) / 72.0 * PPI) as u32
}

/// A PDF with each image on a page of `paper`, as large as fits, turned to landscape for wide
/// images, with its caption below it if there is one, written to `out` as it is made: the
/// images are taken one at a time, and only one page is ever in memory.
pub fn pdf(
    pages: impl Iterator<Item = (image::DynamicImage, Option<String>)>,
    paper: [f32; 2],
    out: impl Write,
) -> std::io::Result<()> {
    let mut pdf = Pdf {
        out,
        at: 0,
        offsets: Vec::new(),
    };
    pdf.put(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")?;
    // Objects by number: 1 catalog, 2 page tree (written last, when the pages are counted),
    // 3 font, then a page, its contents and its image for each.
    pdf.object(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    )?;
    let mut kids = Vec::new();
    for (i, (img, caption)) in pages.into_iter().enumerate() {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let [pw, ph] = if w > h { [paper[1], paper[0]] } else { paper };
        let below = if caption.is_some() { CAPTION } else { 0.0 };
        let (bw, bh) = (pw - 2.0 * MARGIN, ph - 2.0 * MARGIN - below);
        let scale = (bw / w).min(bh / h);
        let (dw, dh) = (w * scale, h * scale);
        let (x, y) = (MARGIN + (bw - dw) / 2.0, MARGIN + below + (bh - dh) / 2.0);
        let mut draw = format!("q {dw:.2} 0 0 {dh:.2} {x:.2} {y:.2} cm /Im Do Q");
        if let Some(text) = caption {
            // Centred by an estimate: Helvetica is about half as wide as high.
            let tw = text.chars().count() as f32 * 9.0 * 0.5;
            let tx = ((pw - tw) / 2.0).max(MARGIN);
            draw += &format!(
                " BT /F 9 Tf {tx:.2} {:.2} Td ({}) Tj ET",
                y - 13.0,
                pdf_text(&text)
            );
        }
        let (page, contents, image) = (4 + 3 * i, 5 + 3 * i, 6 + 3 * i);
        kids.push(format!("{page} 0 R"));
        pdf.object(
            page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Contents \
                 {contents} 0 R /Resources << /XObject << /Im {image} 0 R >> /Font << /F 3 0 R \
                 >> >> >>"
            )
            .as_bytes(),
        )?;
        let mut stream = format!("<< /Length {} >>\nstream\n", draw.len()).into_bytes();
        stream.extend(draw.as_bytes());
        stream.extend(b"\nendstream");
        pdf.object(contents, &stream)?;
        // Transparent parts printed on white, as they show on paper.
        let rgba = img.into_rgba8();
        let flat = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
            let q = rgba.get_pixel(x, y);
            let a = q[3] as u32;
            image::Rgb([0, 1, 2].map(|c| ((q[c] as u32 * a + 255 * (255 - a)) / 255) as u8))
        });
        let mut jpeg = Vec::new();
        let _ =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&flat);
        let mut stream = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            flat.width(),
            flat.height(),
            jpeg.len()
        )
        .into_bytes();
        stream.extend(jpeg);
        stream.extend(b"\nendstream");
        pdf.object(image, &stream)?;
    }
    let tree = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        kids.len()
    );
    pdf.object(2, tree.as_bytes())?;
    let (xref, count) = (pdf.at, pdf.offsets.len() + 1);
    let mut table = format!("xref\n0 {count}\n0000000000 65535 f \n");
    for o in &pdf.offsets {
        table += &format!("{o:010} 00000 n \n");
    }
    table += &format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n");
    pdf.put(table.as_bytes())?;
    pdf.out.flush()
}

/// A PDF being written: where it is, and where each object starts, for the table at the end.
struct Pdf<W> {
    out: W,
    at: usize,
    offsets: Vec<usize>,
}

impl<W: Write> Pdf<W> {
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.out.write_all(bytes)?;
        self.at += bytes.len();
        Ok(())
    }

    fn object(&mut self, num: usize, body: &[u8]) -> std::io::Result<()> {
        self.offsets.resize(self.offsets.len().max(num), 0);
        self.offsets[num - 1] = self.at;
        self.put(format!("{num} 0 obj\n").as_bytes())?;
        self.put(body)?;
        self.put(b"\nendobj\n")
    }
}

/// `text` as a PDF string in WinAnsi (Latin-1 for the letters most names have); others become ?.
fn pdf_text(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            ' '..='~' => c.to_string(),
            '\u{a0}'..='\u{ff}' => format!("\\{:03o}", c as u32),
            _ => "?".into(),
        })
        .collect()
}

/// The printers CUPS knows, and its default, from `lpstat`.
pub fn printers() -> (Vec<String>, Option<String>) {
    let run = |arg: &str| {
        crate::command("lpstat")
            .arg(arg)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let names = run("-e").lines().map(str::to_owned).collect();
    // "system default destination: name"
    let default = run("-d").rsplit_once(": ").map(|d| d.1.trim().to_owned());
    (names, default)
}

/// Send the PDF that `write` makes to `printer` with `lp`, through its input, so no file is
/// left behind (and the Flatpak's private /tmp does not matter).
pub fn send(
    printer: &str,
    copies: u32,
    title: &str,
    write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut child = crate::command("lp")
        .args(["-d", printer, "-n", &copies.to_string(), "-t", title])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("lp (from CUPS): {e}"))?;
    let written = match child.stdin.take() {
        Some(input) => write(&mut std::io::BufWriter::new(input)),
        None => Err(std::io::Error::other("no input")),
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    written.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_a_pdf() {
        let wide = image::DynamicImage::new_rgba8(40, 20);
        let tall = image::DynamicImage::new_rgb8(20, 40);
        let mut pdf = Vec::new();
        super::pdf(
            [(wide, Some("Ålesund (1).jpg".into())), (tall, None)].into_iter(),
            super::PAPERS[0].1,
            &mut pdf,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Count 2"));
        // The wide image on a landscape page, the tall one on a portrait page.
        assert!(text.contains("/MediaBox [0 0 841.89 595.28]"));
        assert!(text.contains("/MediaBox [0 0 595.28 841.89]"));
        assert!(text.contains("(\\305lesund \\(1\\).jpg) Tj"));
        // Every object where the cross-reference table says.
        let xref = pdf.windows(5).rposition(|w| w == b"xref\n").unwrap();
        let table = String::from_utf8_lossy(&pdf[xref..]);
        for (i, line) in table.lines().skip(3).take(9).enumerate() {
            let at: usize = line[..10].parse().unwrap();
            assert!(pdf[at..].starts_with(format!("{} 0 obj", i + 1).as_bytes()));
        }
    }
}
