//! Inline images through the Kitty graphics protocol's Unicode placeholders.
//!
//! An image is transmitted once, given a virtual placement of `cols x rows` cells, and then
//! drawn as ordinary text: each cell is U+10EEEE plus two diacritics naming its row and
//! column, coloured with the image id. The terminal paints the image wherever those cells
//! land, so images scroll, clip and redraw exactly like the rows around them.

mod diacritics;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::wrap::Row;
use diacritics::DIACRITICS;

const PLACEHOLDER: char = '\u{10EEEE}';
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Tallest image in rows, so one diagram cannot push the whole document off screen.
const MAX_ROWS: usize = 40;
/// Kitty caps each escape's payload at 4096 bytes of base64.
const CHUNK: usize = 4096;
/// Ids sit in a range other programs in the same terminal are unlikely to pick.
const FIRST_ID: u32 = 0x00A1_0001;
const FALLBACK_CELL_PX: (u32, u32) = (8, 16);

#[derive(Debug)]
pub(crate) struct Image {
    id: u32,
    png: Vec<u8>,
    width_px: u32,
    height_px: u32,
}

/// Whether the terminal speaks the Kitty graphics protocol with Unicode placeholders.
/// `PLANNOTATOR_IMAGES=0|1` overrides detection.
pub(crate) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let var = |name| std::env::var(name).unwrap_or_default();
        match var("PLANNOTATOR_IMAGES").as_str() {
            "0" => return false,
            "1" => return true,
            _ => {}
        }
        let term = var("TERM");
        matches!(var("TERM_PROGRAM").as_str(), "herdr" | "ghostty" | "kitty" | "WezTerm")
            || term.contains("kitty")
            || term.contains("ghostty")
    })
}

/// Pixel size of one terminal cell, from the tty when it reports one.
pub(crate) fn cell_size() -> (u32, u32) {
    ratatui::crossterm::terminal::window_size()
        .ok()
        .filter(|size| size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0)
        .map_or(FALLBACK_CELL_PX, |size| {
            (u32::from(size.width / size.columns).max(1), u32::from(size.height / size.rows).max(1))
        })
}

/// The id of the `index`th image in a document.
pub(crate) fn image_id(index: usize) -> u32 {
    FIRST_ID + u32::try_from(index).unwrap_or(0)
}

impl Image {
    /// Read a PNG from disk. Other formats and unreadable files give `None`.
    pub(crate) fn load(path: &Path, id: u32) -> Option<Self> {
        Self::from_png(std::fs::read(path).ok()?, id)
    }

    fn from_png(png: Vec<u8>, id: u32) -> Option<Self> {
        if !png.starts_with(PNG_SIGNATURE) {
            return None;
        }
        let dimension = |at: usize| png.get(at..at + 4)?.try_into().ok().map(u32::from_be_bytes);
        let (width_px, height_px) = (dimension(16)?, dimension(20)?);
        (width_px > 0 && height_px > 0).then_some(Self { id, png, width_px, height_px })
    }

    /// The cell box for this image: native size, shrunk to `max_cols` and `MAX_ROWS`.
    pub(crate) fn grid(&self, max_cols: usize, cell: (u32, u32)) -> (usize, usize) {
        let limit = max_cols.min(DIACRITICS.len()).max(1) as f64;
        let row_limit = MAX_ROWS.min(DIACRITICS.len()) as f64;
        let (cell_w, cell_h) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
        let native_cols = f64::from(self.width_px) / cell_w;
        let native_rows = f64::from(self.height_px) / cell_h;
        let scale = (limit / native_cols).min(row_limit / native_rows).min(1.0);
        let cols = (native_cols * scale).ceil().clamp(1.0, limit);
        let rows = (native_rows * scale).ceil().clamp(1.0, row_limit);
        (cols as usize, rows as usize)
    }

    /// Escape sequences that upload the PNG without displaying it.
    pub(crate) fn transmit(&self) -> String {
        let payload = crate::base64::encode(&self.png);
        let chunks: Vec<&str> =
            payload.as_bytes().chunks(CHUNK).filter_map(|chunk| std::str::from_utf8(chunk).ok()).collect();
        let mut out = String::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let more = u8::from(index + 1 < chunks.len());
            if index == 0 {
                let _ = write!(out, "\x1b_Ga=t,f=100,t=d,i={},q=2,m={more};{chunk}\x1b\\", self.id);
            } else {
                let _ = write!(out, "\x1b_Gm={more};{chunk}\x1b\\");
            }
        }
        out
    }

    /// The escape sequence that sizes the image's virtual placement to `cols x rows` cells.
    pub(crate) fn place(&self, (cols, rows): (usize, usize)) -> String {
        format!("\x1b_Ga=p,U=1,i={},p=1,c={cols},r={rows},C=1,q=2\x1b\\", self.id)
    }

    /// Placeholder rows that show the image; they map to no source text.
    pub(crate) fn rows(&self, (cols, rows): (usize, usize)) -> Vec<Row> {
        let [_, r, g, b] = self.id.to_be_bytes();
        let style = Style::new().fg(Color::Rgb(r, g, b));
        DIACRITICS
            .iter()
            .take(rows)
            .map(|row_mark| {
                let text: String = DIACRITICS
                    .iter()
                    .take(cols)
                    .flat_map(|col_mark| [PLACEHOLDER, *row_mark, *col_mark])
                    .collect();
                Row { line: Line::from(Span::styled(text, style)), cells: vec![None; cols] }
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(b"\0\0\0\x0dIHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn only_pngs_with_a_size_load() {
        assert!(Image::from_png(png(10, 10), 1).is_some());
        assert!(Image::from_png(png(0, 10), 1).is_none());
        assert!(Image::from_png(b"GIF89a".to_vec(), 1).is_none());
    }

    #[test]
    fn grid_keeps_aspect_within_width_and_row_cap() {
        let image = Image::from_png(png(800, 400), 1);
        let grid = image.as_ref().map(|i| i.grid(50, (8, 16)));
        assert_eq!(grid, Some((50, 13)));
        let tall = Image::from_png(png(80, 4000), 1).map(|i| i.grid(100, (8, 16)));
        assert_eq!(tall, Some((2, MAX_ROWS)));
        let small = Image::from_png(png(16, 16), 1).map(|i| i.grid(100, (8, 16)));
        assert_eq!(small, Some((2, 1)));
    }

    #[test]
    fn placeholder_cells_name_their_row_and_column_and_carry_the_id() {
        let image = Image::from_png(png(16, 32), 0x0012_3456).expect("a png");
        let rows = image.rows((2, 2));
        let second: Vec<char> = rows[1].line.to_string().chars().collect();
        assert_eq!(
            second,
            [PLACEHOLDER, DIACRITICS[1], DIACRITICS[0], PLACEHOLDER, DIACRITICS[1], DIACRITICS[1]]
        );
        assert_eq!(rows[0].line.spans[0].style.fg, Some(Color::Rgb(0x12, 0x34, 0x56)));
        assert!(rows.iter().all(|r| r.cells == [None, None]));
    }

    #[test]
    fn large_payloads_split_into_continued_chunks() {
        let mut bytes = png(10, 10);
        bytes.resize(10_000, 0);
        let image = Image::from_png(bytes, 7).expect("a png");
        let out = image.transmit();
        assert!(out.starts_with("\x1b_Ga=t,f=100,t=d,i=7,q=2,m=1;"));
        assert_eq!(out.matches("\x1b_G").count(), 4);
        assert!(out.contains("\x1b_Gm=0;"));
    }
}
