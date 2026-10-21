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

/// Room for the caption below the image, at 9 points.
const CAPTION: f32 = 18.0;
/// Room for a line of page text at the top or bottom.
const PAGE_TEXT: f32 = 16.0;
/// Pixels per inch printed; more only makes the PDF larger.
const PPI: f32 = 300.0;
/// Points in a millimetre.
pub const MM: f32 = 72.0 / 25.4;

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

/// Which way the paper is turned: by the image (one wide image a page goes landscape, a sheet
/// stays portrait), or always one way.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Orientation {
    Auto,
    Portrait,
    Landscape,
}

impl Orientation {
    /// From the setting: "portrait", "landscape", else by the image.
    pub fn from(setting: &str) -> Self {
        match setting {
            "portrait" => Self::Portrait,
            "landscape" => Self::Landscape,
            _ => Self::Auto,
        }
    }
}

/// How the images go on the pages.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The paper in points, portrait.
    pub paper: [f32; 2],
    /// Images on each page (`PER_PAGE`).
    pub per_page: usize,
    pub orientation: Orientation,
    /// Around the page, in points.
    pub margin: f32,
    /// Of the room it has, for one image a page: 1 fills it, 0.5 half as wide and high.
    pub scale: f32,
    /// Each image fills its box, cut to it, instead of fitting in it.
    pub fill: bool,
    /// Text at the top and bottom of every page; %page% and %pages% are its numbers.
    pub header: String,
    pub footer: String,
}

impl Layout {
    /// One image a page on `paper`, as large as fits within 10 mm.
    pub fn new(paper: [f32; 2], per_page: usize) -> Self {
        Layout {
            paper,
            per_page,
            orientation: Orientation::Auto,
            margin: 10.0 * MM,
            scale: 1.0,
            fill: false,
            header: String::new(),
            footer: String::new(),
        }
    }
}

impl Layout {
    /// The margin on paper of `size`: at most a third of its short side, so there is room left.
    fn margin_on(&self, [w, h]: [f32; 2]) -> f32 {
        self.margin.clamp(0.0, w.min(h) / 3.0)
    }
}

/// One page as laid out, in PDF's points from the bottom left: its size, the size of its
/// captions' type, and each image's place.
#[derive(Debug)]
pub struct Page {
    pub size: [f32; 2],
    pub type_size: f32,
    pub cells: Vec<Cell>,
    /// The lines of the page text at the top and the bottom ([x, baseline, width]), in the
    /// room left for them.
    pub header: [f32; 3],
    pub footer: [f32; 3],
}

/// Where one image goes: drawn at `image` ([x, y, w, h]), cut to `clip`, its caption on the
/// line at `caption` ([x, baseline, width]).
#[derive(Debug)]
pub struct Cell {
    pub image: [f32; 4],
    pub clip: [f32; 4],
    pub caption: [f32; 3],
}

/// The page for images of `sizes` (pixels), with room for captions if `captions`.
pub fn place(layout: &Layout, sizes: &[[u32; 2]], captions: bool) -> Page {
    let one = layout.per_page == 1;
    let wide = sizes.first().is_some_and(|s| s[0] > s[1]);
    let landscape = match layout.orientation {
        Orientation::Portrait => false,
        Orientation::Landscape => true,
        Orientation::Auto => one && wide,
    };
    let [pw, ph] = match landscape {
        true => [layout.paper[1], layout.paper[0]],
        false => layout.paper,
    };
    let [cols, rows] = match (landscape, grid(layout.per_page)) {
        (true, [c, r]) => [r, c],
        (false, g) => g,
    };
    // Smaller type, and a gap between the images, on a sheet of several.
    let (size, gap) = if one { (9.0, 0.0) } else { (7.0, 8.0) };
    let m = layout.margin_on([pw, ph]);
    let room = |text: &str| if text.is_empty() { 0.0 } else { PAGE_TEXT };
    let (top, bottom) = (ph - m - room(&layout.header), m + room(&layout.footer));
    let (cw, ch) = ((pw - 2.0 * m) / cols as f32, (top - bottom) / rows as f32);
    let below = if captions { CAPTION * size / 9.0 } else { 0.0 };
    let scale = if one {
        layout.scale.clamp(0.1, 1.0)
    } else {
        1.0
    };
    let cells = (sizes.iter().enumerate())
        .map(|(k, &[w, h])| {
            let (cx, cy) = (m + (k % cols) as f32 * cw, top - (k / cols + 1) as f32 * ch);
            let (bw, bh) = ((cw - gap) * scale, ((ch - gap - below) * scale).max(1.0));
            let (bx, by) = (
                cx + (cw - bw) / 2.0,
                cy + gap / 2.0 + below + (ch - gap - below - bh) / 2.0,
            );
            let (w, h) = (w.max(1) as f32, h.max(1) as f32);
            let s = match layout.fill {
                true => (bw / w).max(bh / h),
                false => (bw / w).min(bh / h),
            };
            let (dw, dh) = (w * s, h * s);
            let (x, y) = (bx + (bw - dw) / 2.0, by + (bh - dh) / 2.0);
            Cell {
                image: [x, y, dw, dh],
                clip: [bx, by, bw, bh],
                caption: [cx, y.max(by) - size - 4.0, cw - gap],
            }
        })
        .collect();
    Page {
        size: [pw, ph],
        type_size: size,
        cells,
        header: [m, ph - m - 11.0, pw - 2.0 * m],
        footer: [m, m + 3.0, pw - 2.0 * m],
    }
}

/// The longest side in pixels an image needs on a page of `layout`.
pub fn max_pixels(layout: &Layout) -> u32 {
    let [pw, ph] = layout.paper;
    let [cols, rows] = grid(layout.per_page);
    // As `place` has it: on small photo paper a wide margin is less than asked.
    let m = layout.margin_on([pw, ph]);
    let long = match layout.per_page {
        // Turned to landscape, or cut to fill: its long side may span the paper's.
        1 => pw.max(ph) - 2.0 * m,
        _ => ((pw - 2.0 * m) / cols as f32).max((ph - 2.0 * m) / rows as f32),
    };
    // An image cut to fill its box is drawn larger than the box.
    let fill = if layout.fill { 2.0 } else { 1.0 };
    (long / 72.0 * PPI * fill) as u32
}

/// The text under the image at `path`, `k`th of `n`, from `template`: the full screen info's
/// fields, read from the file here as the print job runs, in one line. `size`, the decoded one
/// upright, where known: the file's header says how it is stored (and of a RAW, its thumbnail).
pub fn caption(
    template: &str,
    path: &Path,
    k: usize,
    n: usize,
    size: Option<[usize; 2]>,
) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (exif, meta) = (std::cell::OnceCell::new(), std::cell::OnceCell::new());
    let text = crate::fill_template(template, |field| match field {
        "name" => name.to_string(),
        "path" => path.display().to_string(),
        "position" => format!("{}/{n}", k + 1),
        "dimensions" => (size.map(|[w, h]| (w as u32, h as u32)))
            .or_else(|| image::image_dimensions(path).ok())
            .map(|(w, h)| format!("{w} × {h}"))
            .unwrap_or_default(),
        "File size" => std::fs::metadata(path)
            .map(|m| crate::human_size(m.len()))
            .unwrap_or_default(),
        "keywords" => meta
            .get_or_init(|| crate::meta::read(path))
            .keywords
            .join(", "),
        "comment" => meta.get_or_init(|| crate::meta::read(path)).comment.clone(),
        label => (exif
            .get_or_init(|| crate::exif::read_and_all(path).0)
            .iter())
        .find(|r| r.0 == label)
        .map(|r| r.1.clone())
        .unwrap_or_default(),
    });
    text.replace('\n', " · ")
}

/// A caption cut to `width` points of `size` type, with ... at the end: Helvetica is about
/// half as wide as high.
fn fitted(text: &str, width: f32, size: f32) -> String {
    let fits = (width / (size * 0.5)) as usize;
    match text.chars().count() > fits {
        true => {
            text.chars()
                .take(fits.saturating_sub(3))
                .collect::<String>()
                + "..."
        }
        false => text.to_owned(),
    }
}

/// `text` centred on the line at `x`, `y` and `width`, as PDF text drawing.
fn centred(text: &str, size: f32, [x, y, width]: [f32; 3]) -> String {
    let text = fitted(text, width, size);
    let tw = text.chars().count() as f32 * size * 0.5;
    let tx = (x + (width - tw) / 2.0).max(x);
    format!(
        "BT /F {size} Tf {tx:.2} {y:.2} Td ({}) Tj ET",
        pdf_text(&text)
    )
}

/// A page text with its numbers filled in.
fn page_text(text: &str, page: usize, pages: usize) -> String {
    text.replace("%page%", &page.to_string())
        .replace("%pages%", &pages.to_string())
}

/// A PDF of `pages` pages with the images laid out by `layout`, each with its caption below
/// it if there is one, written to `out` as it is made: only one page of images is ever in
/// memory.
pub fn pdf(
    images: impl Iterator<Item = (image::DynamicImage, Option<String>)>,
    layout: &Layout,
    pages: usize,
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
    let ([cols, rows], mut images) = (grid(layout.per_page), images.into_iter());
    let (mut kids, mut next) = (Vec::new(), 4);
    loop {
        let on_page: Vec<_> = images.by_ref().take(cols * rows).collect();
        if on_page.is_empty() {
            break;
        }
        let sizes: Vec<[u32; 2]> = on_page
            .iter()
            .map(|i| [i.0.width(), i.0.height()])
            .collect();
        let captions = on_page.iter().any(|i| i.1.is_some());
        let placed = place(layout, &sizes, captions);
        let [pw, ph] = placed.size;
        let (page, contents) = (next, next + 1);
        next += 2;
        let (mut draw, mut names) = (Vec::new(), Vec::new());
        for (k, (cell, (_, caption))) in placed.cells.iter().zip(&on_page).enumerate() {
            let [x, y, dw, dh] = cell.image;
            let [cx, cy, cw, ch] = cell.clip;
            // Cut to its box when it fills it; a box is as large as the image else.
            draw.push(format!(
                "q {cx:.2} {cy:.2} {cw:.2} {ch:.2} re W n {dw:.2} 0 0 {dh:.2} {x:.2} {y:.2} cm /Im{k} Do Q"
            ));
            if let Some(text) = caption {
                draw.push(centred(text, placed.type_size, cell.caption));
            }
            names.push(format!("/Im{k} {} 0 R", next + k));
        }
        let number = kids.len() + 1;
        for (text, line) in [
            (&layout.header, placed.header),
            (&layout.footer, placed.footer),
        ] {
            if !text.is_empty() {
                draw.push(centred(&page_text(text, number, pages), 9.0, line));
            }
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
        for (img, _) in on_page {
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
    // With the usual limit: a CUPS server that does not answer would else hold the dialog.
    let run = |arg: &str| crate::external::limited("lpstat", &[arg], 1 << 16).unwrap_or_default();
    let names = run("-e").lines().map(str::to_owned).collect();
    // "system default destination: name"
    let default = run("-d").rsplit_once(": ").map(|d| d.1.trim().to_owned());
    (names, default)
}

/// `lp`'s options: the printer, copies, title, and both sides of the paper if asked; CUPS
/// prints one side on a printer that cannot do both.
fn lp_args(printer: &str, copies: u32, sides: Option<&str>, title: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-d", printer, "-n", &copies.to_string(), "-t", title]
        .map(str::to_owned)
        .into();
    if let Some(sides) = sides {
        args.extend(["-o".to_owned(), format!("sides={sides}")]);
    }
    args
}

/// CUPS's way of printing on both sides for `layout`: turned on the long edge for portrait
/// pages, on the short edge for landscape ones, so the back is the right way up.
pub fn sides(layout: &Layout) -> &'static str {
    match layout.orientation {
        Orientation::Landscape => "two-sided-short-edge",
        _ => "two-sided-long-edge",
    }
}

/// Send the PDF that `write` makes to `printer` with `lp`, through its input, so no file is
/// left behind (and the Flatpak's private /tmp does not matter).
pub fn send(
    printer: &str,
    copies: u32,
    sides: Option<&str>,
    title: &str,
    write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut child = crate::external::stoppable_command("lp")
        .process_group(0)
        .args(lp_args(printer, copies, sides, title))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("lp (from CUPS): {e}"))?;
    // A CUPS server that does not answer: lp is ended after two minutes and the print told as
    // failed, instead of the busy pointer for ever.
    let dog = crate::external::Watchdog::new(child.id(), crate::external::LIMIT * 4);
    let written = match child.stdin.take() {
        Some(input) => write(&mut std::io::BufWriter::new(input)),
        None => Err(std::io::Error::other("no input")),
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if dog.gave_up() {
        return Err(crate::lang::tr("No answer from the print server in two minutes").into());
    }
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    written.map_err(|e| e.to_string())
}

/// Images the preview shows at most: the most on one page.
const PREVIEWED: usize = 20;
/// The long side of an image in the preview, in pixels.
const PREVIEW_PX: u32 = 256;

/// The layout the print settings ask for.
pub fn layout_of(c: &crate::config::Config) -> Layout {
    let paper = (PAPERS.iter())
        .find(|p| p.0 == c.print_paper)
        .unwrap_or(&PAPERS[0])
        .1;
    Layout {
        orientation: Orientation::from(&c.print_orientation),
        margin: c.print_margin as f32 * MM,
        scale: c.print_scale as f32 / 100.0,
        fill: c.print_fill,
        header: c.print_header.clone(),
        footer: c.print_footer.clone(),
        ..Layout::new(paper, c.print_per_page)
    }
}

/// The first page as it will print, laid out by `place` as the PDF is: white paper, the images
/// (grey until decoded), their text and the page text.
fn preview(ui: &mut egui::Ui, dialog: &PrintDialog, c: &crate::config::Config) {
    let layout = layout_of(c);
    let n = layout.per_page.min(dialog.paths.len());
    let size = |k: usize| {
        (dialog.previews.get(k).and_then(Option::as_ref))
            .map_or([4, 3], |t| [t.size()[0] as u32, t.size()[1] as u32])
    };
    let sizes: Vec<[u32; 2]> = (0..n).map(size).collect();
    let page = place(&layout, &sizes, c.print_caption);
    let [pw, ph] = page.size;
    let f = 240.0 / pw.max(ph);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(pw * f, ph * f), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, egui::Color32::WHITE);
    let at = |x: f32, y: f32| egui::pos2(rect.left() + x * f, rect.bottom() - y * f);
    let grey = egui::Color32::from_gray(90);
    let text = |s: &str, size: f32, [x, y, w]: [f32; 3]| {
        let font = egui::FontId::proportional((size * f).max(5.0));
        p.text(
            at(x + w / 2.0, y),
            egui::Align2::CENTER_BOTTOM,
            fitted(s, w, size),
            font,
            grey,
        );
    };
    let captions = dialog
        .captions
        .as_ref()
        .filter(|c2| c2.0 == c.print_template);
    for (k, cell) in page.cells.iter().enumerate() {
        let [cx, cy, cw, ch] = cell.clip;
        let clip = egui::Rect::from_two_pos(at(cx, cy), at(cx + cw, cy + ch));
        let [x, y, w, h] = cell.image;
        let shown = egui::Rect::from_two_pos(at(x, y), at(x + w, y + h));
        match dialog.previews.get(k).and_then(Option::as_ref) {
            Some(t) => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                p.with_clip_rect(clip.intersect(rect)).image(
                    t.id(),
                    shown,
                    uv,
                    egui::Color32::WHITE,
                );
            }
            None => drop(p.with_clip_rect(clip).rect_filled(
                shown,
                0.0,
                egui::Color32::from_gray(200),
            )),
        }
        if c.print_caption {
            let name = || {
                dialog.paths[k]
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let line = captions
                .and_then(|c2| c2.1.get(k).cloned())
                .unwrap_or_else(name);
            text(&line, page.type_size, cell.caption);
        }
    }
    let pages = dialog.paths.len().div_ceil(layout.per_page.max(1));
    for (line, at) in [(&layout.header, page.header), (&layout.footer, page.footer)] {
        if !line.is_empty() {
            text(&page_text(line, 1, pages), 9.0, at);
        }
    }
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
            loop {
                match dialog.preview_rx.try_recv() {
                    Ok((k, size, pixels)) => {
                        let texture = ctx.load_texture(
                            format!("print preview {k}"),
                            pixels,
                            Default::default(),
                        );
                        (dialog.previews[k], dialog.sizes[k]) = (Some(texture), Some(size));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break dialog.decoded = true,
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if let Took::Answer(made) = dialog.captions_rx.take() {
                dialog.captions = Some(made);
            }
            self.ask_captions(ctx, &mut dialog);
            let mut go = false;
            // Enter in a text field (the page text, the template) ends the typing, not the dialog:
            // it would send the job to the printer with every copy.
            let typing = ctx.memory(|m| m.focused().is_some());
            let modal = egui::Modal::new(egui::Id::unique("print")).show(ctx, |ui| {
                let n = dialog.paths.len();
                ui.strong(match n {
                    1 => tr("Print the image").to_owned(),
                    n => tf!("Print {count}", count = count(n, "image")),
                });
                let c = &mut self.config;
                // The first page beside the settings that shape it.
                ui.horizontal_top(|ui| {
                    preview(ui, &dialog, c);
                    egui::Grid::new("print fields").show(ui, |ui| {
                            ui.label(tr("Printer:"));
                            let shown = |p: &str| if p.is_empty() { tr("PDF file") } else { p }.to_owned();
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
                            ui.label(tr("Orientation:"));
                            let turns = [
                                ("auto", tr("Automatic")),
                                ("portrait", tr("Portrait")),
                                ("landscape", tr("Landscape")),
                            ];
                            let shown = (turns.iter())
                                .find(|t| t.0 == c.print_orientation)
                                .map_or(turns[0].1, |t| t.1);
                            egui::ComboBox::from_id_salt("orientation")
                                .selected_text(shown)
                                .show_ui(ui, |ui| {
                                    for (key, label) in turns {
                                        ui.selectable_value(
                                            &mut c.print_orientation,
                                            key.to_owned(),
                                            label,
                                        );
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
                            if c.print_per_page == 1 {
                                ui.label(tr("Image size (%):"));
                                ui.add(egui::DragValue::new(&mut c.print_scale).range(10..=100));
                                ui.end_row();
                            }
                            ui.label(tr("Margin (mm):"));
                            ui.add(egui::DragValue::new(&mut c.print_margin).range(0..=50));
                            ui.end_row();
                            if !c.print_printer.is_empty() {
                                ui.label(tr("Copies:"));
                                ui.add(egui::DragValue::new(&mut dialog.copies).range(1..=99));
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(&mut c.print_two_sided, tr("Both sides of the paper"));
                                ui.end_row();
                            }
                    });
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut c.print_caption, tr("Text under each image:"));
                    ui.add_enabled(
                        c.print_caption,
                        egui::TextEdit::singleline(&mut c.print_template)
                            .hint_text("%name%")
                            .desired_width(220.0),
                    )
                    .on_hover_text(tr("As the full screen info: %name%, %path%, %position%, %dimensions%, %File size%, %Taken%, %Camera%, %keywords%, %comment% …"));
                });
                ui.checkbox(
                    &mut c.print_fill,
                    tr("Fill each box, cutting the image to it"),
                );
                let numbers = tr("%page% is the page's number, %pages% how many there are");
                ui.horizontal(|ui| {
                    ui.label(tr("Top of each page:"));
                    ui.add(egui::TextEdit::singleline(&mut c.print_header).desired_width(260.0))
                        .on_hover_text(numbers);
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Bottom of each page:"));
                    ui.add(
                        egui::TextEdit::singleline(&mut c.print_footer)
                            .hint_text("%page% / %pages%")
                            .desired_width(260.0),
                    )
                    .on_hover_text(numbers);
                });
                ui.horizontal(|ui| {
                    let label = if c.print_printer.is_empty() {
                        tr("Save PDF… (Enter)")
                    } else {
                        tr("Print (Enter)")
                    };
                    let enter = !typing && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    go = ui.button(label).clicked() || enter;
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if go {
                self.config.save();
                let c = &self.config;
                let job = PrintJob {
                    paths: dialog.paths,
                    layout: layout_of(c),
                    caption: c.print_caption.then(|| c.print_template.clone()),
                    two_sided: c.print_two_sided,
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

    /// File → Print: the dialog for the selected images, or the shown one. The printers are
    /// asked of CUPS, and the first page's images decoded small for the preview, in the
    /// background.
    pub(crate) fn open_print(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let first: Vec<PathBuf> = paths.iter().take(PREVIEWED).cloned().collect();
        let (shown_tx, shown_rx) = mpsc::channel();
        let (rotate, ctx2) = (self.config.exif_rotate, ctx.clone());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(printers());
            ctx.request_repaint();
        });
        std::thread::spawn(move || {
            for (k, path) in first.iter().enumerate() {
                // In the colours of the print: a photo with its own profile looks as on paper.
                let small = survive(|| {
                    let (img, icc, size) = decode::small(path, [PREVIEW_PX; 2], rotate)?;
                    Ok((crate::colour::to_srgb(img, icc.as_deref()), size))
                });
                // Gone with the dialog: the rest are not decoded.
                if let Ok((img, size)) = small
                    && shown_tx.send((k, size, decode::color_image(img))).is_err()
                {
                    break;
                }
                ctx2.request_repaint();
            }
        });
        self.print_window = Some(PrintDialog {
            previews: vec![None; paths.len().min(PREVIEWED)],
            sizes: vec![None; paths.len().min(PREVIEWED)],
            decoded: false,
            paths,
            printers: None,
            printers_rx: rx,
            copies: 1,
            preview_rx: shown_rx,
            captions: None,
            captions_rx: Receiving::none(),
        });
    }

    /// The text under the first page's images for the preview, made again in the background
    /// when the template changed: each reads EXIF and sidecars from its file.
    fn ask_captions(&self, ctx: &egui::Context, dialog: &mut PrintDialog) {
        let template = &self.config.print_template;
        let current = dialog.captions.as_ref().is_some_and(|c| &c.0 == template);
        // %dimensions% from the decoded sizes: made once the previews have them.
        let waiting = template.contains("%dimensions%") && !dialog.decoded;
        if !self.config.print_caption || current || waiting || dialog.captions_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (template, ctx, sizes) = (template.clone(), ctx.clone(), dialog.sizes.clone());
        let (first, n): (Vec<PathBuf>, usize) = (
            dialog.paths.iter().take(PREVIEWED).cloned().collect(),
            dialog.paths.len(),
        );
        std::thread::spawn(move || {
            let texts = (first.iter().enumerate())
                .map(|(k, p)| caption(&template, p, k, n, sizes[k]))
                .collect();
            let _ = tx.send((template, texts));
            ctx.request_repaint();
        });
        dialog.captions_rx = Receiving::of(rx);
    }

    /// Print `job` in the background.
    pub(crate) fn print(&mut self, ctx: &egui::Context, job: PrintJob, to: PrintTo) {
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        // A PDF half made would be lost, its temporary file left beside the one chosen.
        let writing = Working::new(&config::WRITING);
        std::thread::spawn(move || {
            let _writing = writing;
            let max = print::max_pixels(&job.layout);
            let failed = std::cell::Cell::new(0);
            let pages = job.paths.iter().enumerate().filter_map(|(k, path)| {
                // As for the viewer: a broken file that makes a decoder panic is a page short,
                // not a PDF half written and the thread gone.
                // Shrunk before it goes to sRGB: a 45 MP photo with its own profile would
                // need two full-size buffers else. A JPEG decoded at a fraction when that is
                // enough.
                let img = survive(|| {
                    let (img, icc, size) = decode::small(path, [max, max], job.rotate)?;
                    Ok((crate::colour::to_srgb(img, icc.as_deref()), size))
                });
                failed.set(failed.get() + img.is_err() as usize);
                let (img, size) = img.ok()?;
                let text = job
                    .caption
                    .as_deref()
                    .map(|t| print::caption(t, path, k, job.paths.len(), Some(size)));
                Some((img, text))
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
            // shortcut: the pages counted from the files, for %pages%; one that cannot be read
            // makes the count one too high at the end of a sheet. Read them all first to know.
            let total = job.paths.len().div_ceil(job.layout.per_page.max(1));
            let title = format!("Glimt: {}", count(job.paths.len(), "image"));
            let done = match &to {
                PrintTo::Printer(printer) => {
                    let sides = job.two_sided.then(|| print::sides(&job.layout));
                    print::send(printer, job.copies, sides, &title, |w| {
                        print::pdf(pages, &job.layout, total, w)
                    })
                }
                // Beside it first: the file chosen is replaced only by a whole PDF, after every
                // image was read (one of them may be the file chosen).
                PrintTo::File(file) => {
                    // As every file Glimt writes again: beside it, whole, then its name.
                    crate::config::replace_with(file, false, |f| {
                        let mut out = std::io::BufWriter::new(f);
                        print::pdf(pages, &job.layout, total, &mut out)?;
                        out.flush()
                    })
                    .map_err(|e| e.to_string())
                }
            };
            // Images, and the pages they fill: a contact sheet holds several on each.
            let n = job.paths.len() - failed.get();
            let what = match job.layout.per_page {
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
    /// A margin wider than a third of small paper is held there, and images are decoded for
    /// the room that leaves, not for less.
    #[test]
    fn decodes_for_the_room_left() {
        use super::{Layout, MM, PAPERS, max_pixels};
        let photo = PAPERS.iter().find(|p| p.0.starts_with("10")).unwrap().1;
        let wide = Layout {
            margin: 50.0 * MM,
            ..Layout::new(photo, 1)
        };
        let third = Layout {
            margin: photo[0] / 3.0,
            ..Layout::new(photo, 1)
        };
        assert_eq!(max_pixels(&wide), max_pixels(&third));
    }

    /// Both sides asked of lp only when chosen.
    #[test]
    fn asks_for_both_sides() {
        use super::{Layout, Orientation, PAPERS, lp_args, sides};
        assert!(!lp_args("p", 1, None, "t").contains(&"-o".to_owned()));
        let both = lp_args("p", 2, Some("two-sided-long-edge"), "t");
        assert_eq!(both[both.len() - 2..], ["-o", "sides=two-sided-long-edge"]);
        assert_eq!(both[..4], ["-d", "p", "-n", "2"]);
        // Landscape pages turn on the short edge.
        let wide = Layout {
            orientation: Orientation::Landscape,
            ..Layout::new(PAPERS[0].1, 1)
        };
        assert_eq!(sides(&wide), "two-sided-short-edge");
        assert_eq!(sides(&Layout::new(PAPERS[0].1, 1)), "two-sided-long-edge");
    }

    /// Text on every page, with its numbers, and room made for it: the image moves down from
    /// the top line.
    #[test]
    fn writes_page_text() {
        use super::{Layout, PAPERS, place};
        let a4 = PAPERS[0].1;
        let layout = Layout {
            header: "Trip".into(),
            footer: "Page %page% of %pages%".into(),
            ..Layout::new(a4, 1)
        };
        let mut pdf = Vec::new();
        let images = (0..2).map(|_| (image::DynamicImage::new_rgb8(20, 40), None));
        super::pdf(images, &layout, 2, &mut pdf).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("(Trip) Tj")
                && text.contains("(Page 1 of 2) Tj")
                && text.contains("(Page 2 of 2) Tj")
        );
        let top = |l: &Layout| {
            let c = &place(l, &[[20, 40]], false).cells[0];
            c.clip[1] + c.clip[3]
        };
        assert!(top(&layout) < top(&Layout::new(a4, 1)));
    }

    /// The text under an image from the template: its name, place and size, and a field
    /// it lacks left out with the words around it, as in the full screen info.
    #[test]
    fn captions_from_the_template() {
        let file =
            std::env::temp_dir().join(format!("glimt-test-caption-{}.png", std::process::id()));
        image::RgbImage::new(30, 20).save(&file).unwrap();
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let text = super::caption(
            "%name% (%position%)|%dimensions%|ISO %ISO%",
            &file,
            2,
            9,
            None,
        );
        assert_eq!(text, format!("{name} (3/9) · 30 × 20"));
        // The size decoded, upright, where known: not the stored one of the header.
        let turned = super::caption("%dimensions%", &file, 0, 1, Some([20, 30]));
        assert_eq!(turned, "20 × 30");
        std::fs::remove_file(file).unwrap();
    }

    /// Filling its box, a wide image is as high as the box and wider, cut to it in the PDF;
    /// fitted, it lies within it.
    #[test]
    fn fills_the_box() {
        use super::{Layout, PAPERS, place};
        let a4 = PAPERS[0].1;
        let cell = |fill| {
            place(
                &Layout {
                    fill,
                    ..Layout::new(a4, 4)
                },
                &[[300, 100]],
                false,
            )
            .cells
            .remove(0)
        };
        let (fit, fill) = (cell(false), cell(true));
        assert!(fit.image[2] <= fit.clip[2] + 0.01 && fit.image[3] < fit.clip[3]);
        assert!((fill.image[3] - fill.clip[3]).abs() < 0.01 && fill.image[2] > fill.clip[2]);
        let mut pdf = Vec::new();
        let img = image::DynamicImage::new_rgb8(30, 10);
        super::pdf(
            [(img, None)].into_iter(),
            &Layout {
                fill: true,
                ..Layout::new(a4, 4)
            },
            1,
            &mut pdf,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&pdf).contains(" re W n "));
    }

    /// One image a page at half size: half as wide, in the middle of the page; a sheet keeps
    /// its images whole.
    #[test]
    fn scales_one_image() {
        use super::{Layout, PAPERS, place};
        let a4 = PAPERS[0].1;
        let full = place(&Layout::new(a4, 1), &[[100, 100]], false).cells[0].image;
        let half = place(
            &Layout {
                scale: 0.5,
                ..Layout::new(a4, 1)
            },
            &[[100, 100]],
            false,
        )
        .cells[0]
            .image;
        assert!((half[2] - full[2] / 2.0).abs() < 0.01);
        assert!((half[0] + half[2] / 2.0 - a4[0] / 2.0).abs() < 0.01);
        let sheet = |scale| {
            place(
                &Layout {
                    scale,
                    ..Layout::new(a4, 4)
                },
                &[[100, 100]],
                false,
            )
            .cells[0]
                .image[2]
        };
        assert_eq!(sheet(0.5), sheet(1.0));
    }

    /// The margin around the page: none puts the image at the paper's edge, 20 mm in from it.
    #[test]
    fn keeps_the_margin() {
        use super::{Layout, MM, PAPERS, place};
        let a4 = PAPERS[0].1;
        let at = |mm: f32| {
            place(
                &Layout {
                    margin: mm * MM,
                    ..Layout::new(a4, 1)
                },
                &[[100, 100]],
                false,
            )
            .cells[0]
                .clip
        };
        assert_eq!(at(0.0)[0], 0.0);
        assert!((at(20.0)[0] - 20.0 * MM).abs() < 0.01);
        assert!((at(20.0)[2] - (a4[0] - 40.0 * MM)).abs() < 0.01);
    }

    /// The paper turned by the setting: a sheet turned to landscape turns its grid too; by
    /// the image, a wide one alone goes landscape and a sheet stays portrait.
    #[test]
    fn turns_the_paper() {
        use super::{Layout, Orientation, PAPERS, place};
        let a4 = PAPERS[0].1;
        let wide = [[60, 40]; 6];
        let auto = |per| place(&Layout::new(a4, per), &wide[..per], false);
        assert_eq!(auto(1).size, [a4[1], a4[0]]);
        assert_eq!(auto(6).size, a4);
        let landscape = Layout {
            orientation: Orientation::Landscape,
            ..Layout::new(a4, 6)
        };
        let page = place(&landscape, &wide, false);
        assert_eq!(page.size, [a4[1], a4[0]]);
        // Three across and two down on the wide page.
        let xs: std::collections::BTreeSet<i32> =
            page.cells.iter().map(|c| c.clip[0] as i32).collect();
        assert_eq!(xs.len(), 3);
        let portrait = Layout {
            orientation: Orientation::Portrait,
            ..Layout::new(a4, 1)
        };
        assert_eq!(place(&portrait, &wide[..1], false).size, a4);
    }

    #[test]
    fn writes_a_pdf() {
        let wide = image::DynamicImage::new_rgba8(40, 20);
        let tall = image::DynamicImage::new_rgb8(20, 40);
        let mut pdf = Vec::new();
        let layout = super::Layout::new(super::PAPERS[0].1, 1);
        super::pdf(
            [(wide, Some("Ålesund (1).jpg".into())), (tall, None)].into_iter(),
            &layout,
            2,
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
        super::pdf(
            pages,
            &super::Layout::new(super::PAPERS[0].1, 4),
            2,
            &mut pdf,
        )
        .unwrap();
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
