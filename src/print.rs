//! Printing, as Geeqie's File → Print: the images in a PDF, one or more per page, which CUPS prints
//! (`lp`) or which is saved as a file. The PDF is written here, with the JPEG of each image and
//! the file name under it in Helvetica, which every PDF reader has.
//! Several on a page make a contact sheet, as Geeqie's.

use crate::*;
use std::io::Write;

/// Paper sizes in points (1/72 inch), portrait: office papers, and photo papers in cm.
pub const PAPERS: [(&str, [f32; 2]); 7] = [
    ("A4", [595.28, 841.89]),
    ("Letter", [612.0, 792.0]),
    ("A3", [841.89, 1190.55]),
    ("A5", [419.53, 595.28]),
    ("Legal", [612.0, 1008.0]),
    ("10 × 15 cm", [283.46, 425.2]),
    ("13 × 18 cm", [368.5, 510.24]),
];

/// Margin around the image, 10 mm.
const MARGIN: f32 = 28.35;
/// Room for the caption below the image.
const CAPTION: f32 = 18.0;
/// Pixels per inch printed; more only makes the PDF larger.
const PPI: f32 = 300.0;

/// Images per page, and their grid on a portrait page as [columns, rows].
pub const PER_PAGE: [(usize, [usize; 2]); 7] = [
    (1, [1, 1]),
    (2, [1, 2]),
    (4, [2, 2]),
    (6, [2, 3]),
    (9, [3, 3]),
    (12, [3, 4]),
    (20, [4, 5]),
];

fn grid(per_page: usize) -> [usize; 2] {
    PER_PAGE
        .iter()
        .find(|p| p.0 == per_page)
        .map_or([1, 1], |p| p.1)
}

/// The longest side in pixels an image needs on `paper` with `per_page` on each page.
pub fn max_pixels(paper: [f32; 2], per_page: usize) -> u32 {
    let [cols, rows] = grid(per_page);
    let cell =
        ((paper[0] - 2.0 * MARGIN) / cols as f32).max((paper[1] - 2.0 * MARGIN) / rows as f32);
    let long = if per_page == 1 {
        paper[0].max(paper[1]) - 2.0 * MARGIN
    } else {
        cell
    };
    (long / 72.0 * PPI) as u32
}

/// A PDF with the images on pages of `paper`, `per_page` on each in a grid (one: as large as
/// fits, turned to landscape for a wide image), each with its caption below it if there is one,
/// written to `out` as it is made: only one page of images is ever in memory.
pub fn pdf(
    pages: impl Iterator<Item = (image::DynamicImage, Option<String>)>,
    paper: [f32; 2],
    per_page: usize,
    out: impl Write,
) -> std::io::Result<()> {
    let mut pdf = Pdf {
        out,
        at: 0,
        offsets: Vec::new(),
    };
    pdf.put(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")?;
    // Objects by number: 1 catalog, 2 page tree (written last, when the pages are counted),
    // 3 font, then for each page the page, its contents and its images.
    pdf.object(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    )?;
    let ([cols, rows], mut pages) = (grid(per_page), pages.into_iter());
    let (mut kids, mut next) = (Vec::new(), 4);
    loop {
        let images: Vec<_> = pages.by_ref().take(cols * rows).collect();
        let Some(first) = images.first() else { break };
        let wide = first.0.width() > first.0.height();
        let [pw, ph] = if images.len() == 1 && cols * rows == 1 && wide {
            [paper[1], paper[0]]
        } else {
            paper
        };
        // Smaller type, and a gap between the images, on a sheet of several.
        let (size, gap) = if cols * rows == 1 {
            (9.0, 0.0)
        } else {
            (7.0, 8.0)
        };
        let (cw, ch) = (
            (pw - 2.0 * MARGIN) / cols as f32,
            (ph - 2.0 * MARGIN) / rows as f32,
        );
        let (page, contents) = (next, next + 1);
        next += 2;
        let (mut draw, mut names) = (Vec::new(), Vec::new());
        for (k, (img, caption)) in images.iter().enumerate() {
            // The cell, from the top left; PDF counts y from the bottom.
            let (cx, cy) = (
                MARGIN + (k % cols) as f32 * cw,
                ph - MARGIN - (k / cols + 1) as f32 * ch,
            );
            let below = if caption.is_some() {
                CAPTION * size / 9.0
            } else {
                0.0
            };
            let (bw, bh) = (cw - gap, ch - gap - below);
            let (w, h) = (img.width() as f32, img.height() as f32);
            let scale = (bw / w).min(bh / h);
            let (dw, dh) = (w * scale, h * scale);
            let (x, y) = (
                cx + (cw - dw) / 2.0,
                cy + gap / 2.0 + below + (bh - dh) / 2.0,
            );
            draw.push(format!(
                "q {dw:.2} 0 0 {dh:.2} {x:.2} {y:.2} cm /Im{k} Do Q"
            ));
            if let Some(text) = caption {
                // Centred by an estimate: Helvetica is about half as wide as high. A name too
                // long for its cell is cut, with ... at the end.
                let fits = ((cw - gap) / (size * 0.5)) as usize;
                let text = match text.chars().count() > fits {
                    true => {
                        text.chars()
                            .take(fits.saturating_sub(3))
                            .collect::<String>()
                            + "..."
                    }
                    false => text.clone(),
                };
                let tw = text.chars().count() as f32 * size * 0.5;
                let tx = (cx + (cw - tw) / 2.0).max(cx);
                draw.push(format!(
                    "BT /F {size} Tf {tx:.2} {:.2} Td ({}) Tj ET",
                    y - size - 4.0,
                    pdf_text(&text)
                ));
            }
            names.push(format!("/Im{k} {nextk} 0 R", nextk = next + k));
        }
        kids.push(format!("{page} 0 R"));
        pdf.object(
            page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Contents \
                 {contents} 0 R /Resources << /XObject << {} >> /Font << /F 3 0 R >> >> >>",
                names.join(" ")
            )
            .as_bytes(),
        )?;
        let draw = draw.join(" ");
        let mut stream = format!("<< /Length {} >>\nstream\n", draw.len()).into_bytes();
        stream.extend(draw.as_bytes());
        stream.extend(b"\nendstream");
        pdf.object(contents, &stream)?;
        for (img, _) in images {
            pdf.object(next, &image_object(img))?;
            next += 1;
        }
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

/// An image as a PDF XObject: JPEG, with transparent parts on white, as they show on paper.
fn image_object(img: image::DynamicImage) -> Vec<u8> {
    let rgba = img.into_rgba8();
    let flat = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let q = rgba.get_pixel(x, y);
        let a = q[3] as u32;
        image::Rgb([0, 1, 2].map(|c| ((q[c] as u32 * a + 255 * (255 - a)) / 255) as u8))
    });
    let mut jpeg = Vec::new();
    let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&flat);
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
    stream
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
        crate::external::command("lpstat")
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
    let mut child = crate::external::command("lp")
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

// ---- Print: the dialog and the job in the background ----

impl crate::App {
    /// File → Print: the printer or a PDF, the paper, and images or contact sheets.
    pub(crate) fn print_dialog(&mut self, ctx: &egui::Context) {
        if let Some(mut dialog) = self.print_window.take() {
            if let Ok((names, default)) = dialog.printers_rx.try_recv() {
                // The printer used last, else CUPS's default; a PDF file if neither is there.
                let c = &mut self.config;
                if !names.contains(&c.print_printer) {
                    c.print_printer = default.filter(|d| names.contains(d)).unwrap_or_default();
                }
                dialog.printers = Some(names);
            }
            let mut go = false;
            let modal = egui::Modal::new(egui::Id::new("print")).show(ctx, |ui| {
                let n = dialog.paths.len();
                ui.strong(match n {
                    1 => "Print the image".to_owned(),
                    n => tf!("Print {count}", count = count(n, "image")),
                });
                let c = &mut self.config;
                egui::Grid::new("print fields").show(ui, |ui| {
                    ui.label(tr("Printer:"));
                    let shown = |p: &str| if p.is_empty() { "PDF file" } else { p }.to_owned();
                    egui::ComboBox::from_id_salt("printer")
                        .selected_text(shown(&c.print_printer))
                        .show_ui(ui, |ui| {
                            for p in dialog.printers.iter().flatten() {
                                ui.selectable_value(&mut c.print_printer, p.clone(), p);
                            }
                            ui.selectable_value(
                                &mut c.print_printer,
                                String::new(),
                                tr("PDF file"),
                            );
                        });
                    match &dialog.printers {
                        None => drop(ui.spinner()),
                        Some(p) if p.is_empty() => drop(ui.weak(tr("No printers found"))),
                        Some(_) => {}
                    }
                    ui.end_row();
                    ui.label(tr("Paper:"));
                    egui::ComboBox::from_id_salt("paper")
                        .selected_text(c.print_paper.clone())
                        .show_ui(ui, |ui| {
                            for (name, _) in print::PAPERS {
                                ui.selectable_value(&mut c.print_paper, name.to_owned(), name);
                            }
                        });
                    ui.end_row();
                    ui.label(tr("Images per page:"));
                    egui::ComboBox::from_id_salt("per page")
                        .selected_text(c.print_per_page.to_string())
                        .show_ui(ui, |ui| {
                            for (n, [cols, rows]) in print::PER_PAGE {
                                let label = match n {
                                    1 => "1".to_owned(),
                                    n => format!("{n} ({cols} × {rows})"),
                                };
                                ui.selectable_value(&mut c.print_per_page, n, label);
                            }
                        });
                    ui.end_row();
                    if !c.print_printer.is_empty() {
                        ui.label(tr("Copies:"));
                        ui.add(egui::DragValue::new(&mut dialog.copies).range(1..=99));
                        ui.end_row();
                    }
                });
                ui.checkbox(&mut c.print_caption, tr("File name under each image"));
                ui.horizontal(|ui| {
                    let label = if c.print_printer.is_empty() {
                        tr("Save PDF… (Enter)")
                    } else {
                        tr("Print (Enter)")
                    };
                    go =
                        ui.button(label).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if go {
                self.config.save();
                let c = &self.config;
                let paper = (print::PAPERS.iter())
                    .find(|p| p.0 == c.print_paper)
                    .unwrap_or(&print::PAPERS[0])
                    .1;
                let job = PrintJob {
                    paths: dialog.paths,
                    paper,
                    per_page: c.print_per_page,
                    caption: c.print_caption,
                    rotate: c.exif_rotate,
                    copies: dialog.copies,
                };
                match c.print_printer.clone() {
                    p if p.is_empty() => self.pick(ctx, false, Pick::SavePdf(job)),
                    p => self.print(ctx, job, PrintTo::Printer(p)),
                }
            } else if !modal.should_close() {
                self.print_window = Some(dialog);
            }
        }
    }

    /// File → Print: the dialog for the selected images, or the shown one.
    pub(crate) fn open_print(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(print::printers());
            ctx.request_repaint();
        });
        self.print_window = Some(PrintDialog {
            paths: self.targets().iter().map(|&i| self.path(i)).collect(),
            printers: None,
            printers_rx: rx,
            copies: 1,
        });
    }

    /// Print `job` in the background.
    pub(crate) fn print(&mut self, ctx: &egui::Context, job: PrintJob, to: PrintTo) {
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        // A PDF half made would be lost, its temporary file left beside the one chosen.
        let writing = Working::new(&self.writing);
        std::thread::spawn(move || {
            let _writing = writing;
            let max = print::max_pixels(job.paper, job.per_page);
            let failed = std::cell::Cell::new(0);
            let pages = job.paths.iter().filter_map(|path| {
                // As for the viewer: a broken file that makes a decoder panic is a page short,
                // not a PDF half written and the thread gone.
                // Shrunk before it goes to sRGB: a 45 MP photo with its own profile would
                // need two full-size buffers else. A JPEG decoded at a fraction when that is
                // enough.
                let img = survive(|| {
                    let (img, icc) = match decode::scaled_jpeg(path, [max, max], job.rotate) {
                        Some((img, icc, _)) => (img, icc),
                        None => decode::load_icc(path, job.rotate)?,
                    };
                    Ok(crate::colour::to_srgb(
                        shrink(img, [max, max])?,
                        icc.as_deref(),
                    ))
                });
                failed.set(failed.get() + img.is_err() as usize);
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                Some((img.ok()?, job.caption.then(|| name.into_owned())))
            });
            // Not one image could be read (HEIC without ffmpeg, say): nothing is sent to the
            // printer, and the file chosen is not replaced by an empty PDF.
            let mut pages = pages.peekable();
            if pages.peek().is_none() {
                let _ = tx.send(tr("No image could be read: nothing printed").into());
                drop(pointer);
                ctx.request_repaint();
                return;
            }
            // Written as it is made, so only one page is in memory, not the whole PDF.
            let title = format!("Glimt: {}", count(job.paths.len(), "image"));
            let done = match &to {
                PrintTo::Printer(printer) => print::send(printer, job.copies, &title, |w| {
                    print::pdf(pages, job.paper, job.per_page, w)
                }),
                // Beside it first: the file chosen is replaced only by a whole PDF, after every
                // image was read (one of them may be the file chosen).
                PrintTo::File(file) => {
                    let tmp = crate::config::tmp_path(file);
                    let written = std::fs::File::create(&tmp)
                        .and_then(|f| {
                            let mut out = std::io::BufWriter::new(f);
                            print::pdf(pages, job.paper, job.per_page, &mut out)?;
                            out.into_inner().map_err(|e| e.into_error())?.sync_all()
                        })
                        .and_then(|_| std::fs::rename(&tmp, file));
                    if written.is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                    written.map_err(|e| e.to_string())
                }
            };
            // Images, and the pages they fill: a contact sheet holds several on each.
            let n = job.paths.len() - failed.get();
            let what = match job.per_page {
                1 => count(n, "page"),
                per => tf!(
                    "{images} on {pages}",
                    images = count(n, "image"),
                    pages = count(n.div_ceil(per), "page")
                ),
            };
            let done = done.map(|()| match &to {
                PrintTo::Printer(printer) => tf!("{what} sent to {printer}", what, printer),
                PrintTo::File(file) => {
                    tf!("{what} saved in {file}", what, file = file.display())
                }
            });
            let _ = tx.send(match (done, failed.get()) {
                (Ok(msg), 0) => msg,
                (Ok(msg), f) => tf!("{msg}; {f} could not be read", msg, f),
                (Err(e), _) => tf!("Printing failed: {e}", e),
            });
            drop(pointer);
            ctx.request_repaint();
        });
    }
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
            1,
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

    /// Five images four to a page: two pages, five images, each with its name, and every
    /// object where the table says.
    #[test]
    fn writes_a_contact_sheet() {
        let pages = (0..5u8).map(|k| {
            let img = image::RgbImage::from_pixel(60, 40, image::Rgb([k * 50, 0, 0]));
            (img.into(), Some(format!("img{k}.jpg")))
        });
        let mut pdf = Vec::new();
        super::pdf(pages, super::PAPERS[0].1, 4, &mut pdf).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 2"));
        assert_eq!(text.matches("/Subtype /Image").count(), 5);
        assert!(text.contains("/Im3 ") && text.contains("(img4.jpg) Tj"));
        // A sheet stays portrait, also with wide images.
        assert!(!text.contains("/MediaBox [0 0 841.89 595.28]"));
        let xref = pdf.windows(5).rposition(|w| w == b"xref\n").unwrap();
        let table = String::from_utf8_lossy(&pdf[xref..]);
        for (i, line) in table.lines().skip(3).enumerate() {
            let Ok(at) = line.get(..10).unwrap_or("x").parse::<usize>() else {
                break;
            };
            assert!(pdf[at..].starts_with(format!("{} 0 obj", i + 1).as_bytes()));
        }
    }
}
