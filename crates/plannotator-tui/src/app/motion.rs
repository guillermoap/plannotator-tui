//! Cursor motions over the rendered text: vim word jumps and counts, `{n}G` to a source
//! line, and Helix-style jump labels on the visible words.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use super::{App, Mode};
use crate::keys::{self, Action};
use crate::theme::palette;
use crate::wrap::Row;

const LABEL_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// One word on a row: the columns of its first and last characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Word {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punctuation,
}

/// `goto-word` is waiting for a label: each visible word's two letters and its position.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Jump {
    targets: Vec<([char; 2], (usize, usize))>,
    first: Option<char>,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

/// The words on a row, as vim splits them: runs of word characters or of punctuation.
/// Cells that map to no source (bullets, borders, image placeholders) split words like
/// spaces, since nothing there can be selected.
fn words(row: &Row) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut previous = Class::Space;
    let mut col = 0;
    for c in row.line.spans.iter().flat_map(|span| span.content.chars()) {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            continue;
        }
        let mapped = row.cells.get(col).copied().flatten().is_some();
        let here = if mapped { class(c) } else { Class::Space };
        match words.last_mut() {
            Some(word) if here != Class::Space && here == previous => word.end = col,
            _ if here != Class::Space => words.push(Word { start: col, end: col }),
            _ => {}
        }
        previous = here;
        col += width;
    }
    words
}

impl App {
    /// The cursor motions shared by visual and roaming modes, `count` times. True when
    /// `key` was one.
    pub(super) fn motion_key(&mut self, key: KeyEvent, count: Option<usize>) -> bool {
        let Some(action) = self.keys.action(&key, keys::MOTION) else { return false };
        for _ in 0..count.unwrap_or(1).max(1) {
            match action {
                Action::Left => self.move_cursor(0, -1),
                Action::Right => self.move_cursor(0, 1),
                Action::Down => self.move_cursor(1, 0),
                Action::Up => self.move_cursor(-1, 0),
                Action::WordForward | Action::WordBack | Action::WordEnd => self.move_word(action),
                Action::LineStart => self.cursor.1 = 0,
                Action::LineEnd => {
                    self.cursor.1 =
                        self.open.layout.row(self.cursor.0).map_or(0, |r| r.cells.len().saturating_sub(1));
                }
                Action::GotoWord => {
                    self.start_jump();
                    break;
                }
                _ => return false,
            }
        }
        self.ensure_cursor_visible();
        true
    }

    /// Move the keyboard cursor by rows/columns, skipping gap rows and clamping to text.
    pub(super) fn move_cursor(&mut self, rows: i64, cols: i64) {
        let total = self.open.layout.total_rows;
        if total == 0 {
            return;
        }
        let mut row = self.cursor.0;
        for _ in 0..rows.unsigned_abs() {
            let next = if rows > 0 { row + 1 } else { row.saturating_sub(1) };
            if next >= total || next == row {
                break;
            }
            row = next;
            if self.open.layout.row(row).is_none() {
                // gap row: keep going in the same direction
                let after = if rows > 0 { row + 1 } else { row.saturating_sub(1) };
                if after < total && self.open.layout.row(after).is_some() {
                    row = after;
                }
            }
        }
        let width = self.open.layout.row(row).map_or(0, |r| r.cells.len());
        let col = (self.cursor.1 as i64 + cols).clamp(0, width.saturating_sub(1) as i64) as usize;
        self.move_to((row, col));
    }

    /// `word-forward`, `word-end`, `word-back`: the next word start, the next word end, or
    /// the previous word start, carrying on to other rows the way vim does.
    fn move_word(&mut self, action: Action) {
        let (row, col) = self.cursor;
        let words_on = |r: usize| self.open.layout.row(r).map(words).unwrap_or_default();
        let target = if action == Action::WordBack {
            (0..=row).rev().find_map(|r| {
                words_on(r).iter().rev().map(|w| w.start).find(|&c| r < row || c < col).map(|c| (r, c))
            })
        } else {
            let edge = |w: &Word| if action == Action::WordEnd { w.end } else { w.start };
            (row..self.open.layout.total_rows)
                .find_map(|r| words_on(r).iter().map(edge).find(|&c| r > row || c > col).map(|c| (r, c)))
        };
        if let Some(target) = target {
            self.move_to(target);
        }
    }

    fn move_to(&mut self, at: (usize, usize)) {
        self.cursor = at;
        if let Some(block) = self.open.layout.block_at_row(at.0) {
            self.selected = block;
        }
        self.ensure_cursor_visible();
    }

    /// A digit typed before a command, vim's count: `5w`, `10G`. `0` only extends a count
    /// already started, so alone it stays `line-start`; a digit bound to an action keeps it.
    pub(super) fn count_digit(&mut self, key: &KeyEvent) -> bool {
        let KeyCode::Char(c) = key.code else { return false };
        let Some(digit) = c.to_digit(10) else { return false };
        let bound =
            self.keys.action(key, keys::DOCUMENT).is_some() || self.keys.action(key, keys::MOTION).is_some();
        let counts = if digit == 0 { self.count.is_some() } else { !bound };
        if !counts || key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let count = self.count.unwrap_or(0).saturating_mul(10).saturating_add(digit as usize);
        self.count = Some(count);
        self.status = Some(count.to_string());
        true
    }

    /// `{n}G`: the cursor to the first rendered character of source line `n`, or the first
    /// one after it when the line renders nothing (a fence, a blank line).
    pub(super) fn goto_line(&mut self, line: usize) {
        let source = &self.open.doc.source;
        let start: usize = source.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum();
        let target = (0..self.open.layout.total_rows).find_map(|r| {
            let cells = &self.open.layout.row(r)?.cells;
            cells.iter().position(|c| c.is_some_and(|offset| offset >= start)).map(|col| (r, col))
        });
        let Some(target) = target else {
            self.select_block(self.open.doc.blocks.len().saturating_sub(1));
            return;
        };
        if self.selection.is_none() {
            self.pending = None;
            self.roam = true;
        }
        self.move_to(target);
        self.status = Some(format!("line {line}"));
    }

    /// `goto-word`: label every visible word, nearest the cursor first, and wait for one.
    fn start_jump(&mut self) {
        let height = usize::from(self.geometry.doc.height.max(1));
        let rows = self.scroll..(self.scroll + height).min(self.open.layout.total_rows);
        let mut spots: Vec<(usize, usize)> = rows
            .filter_map(|r| Some((r, words(self.open.layout.row(r)?))))
            .flat_map(|(r, words)| words.into_iter().filter(|w| w.end > w.start).map(move |w| (r, w.start)))
            .collect();
        let (row, col) = self.cursor;
        spots.sort_by_key(|&(r, c)| (r.abs_diff(row), c.abs_diff(col)));
        let labels = LABEL_ALPHABET
            .iter()
            .flat_map(|&a| LABEL_ALPHABET.iter().map(move |&b| [char::from(a), char::from(b)]));
        let targets: Vec<_> = labels.zip(spots).collect();
        if targets.is_empty() {
            self.status = Some("no words to jump to".into());
            return;
        }
        self.status = Some(format!("jump: type a label · {} cancel", self.keys.hint(Action::Back)));
        self.mode = Mode::Jump(Jump { targets, first: None });
    }

    /// A key while the labels show: the first letter narrows them, the second jumps; any
    /// other key cancels. The jump extends a selection being made, else roams the cursor.
    pub(super) fn jump_key(&mut self, key: KeyEvent) {
        let Mode::Jump(jump) = &mut self.mode else { return };
        let typed = match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => Some(c),
            _ => None,
        };
        let landing = match (typed, jump.first) {
            (Some(c), None) if jump.targets.iter().any(|(label, _)| label[0] == c) => {
                jump.first = Some(c);
                return;
            }
            (Some(c), Some(first)) => {
                jump.targets.iter().find(|(label, _)| *label == [first, c]).map(|t| t.1)
            }
            _ => None,
        };
        self.mode = Mode::Browse;
        self.status = None;
        let Some(at) = landing else { return };
        match self.selection.as_mut() {
            Some(selection) if selection.dragging => selection.set_head(at),
            _ => {
                self.clear_selection();
                self.roam = true;
            }
        }
        self.move_to(at);
    }

    /// Dim the document under the labels and draw each one over its word's first letters.
    pub(super) fn draw_jump_labels(&self, buf: &mut Buffer, doc: Rect) {
        let Mode::Jump(jump) = &self.mode else { return };
        buf.set_style(doc, Style::new().add_modifier(Modifier::DIM));
        let style =
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD).remove_modifier(Modifier::DIM);
        for (label, (row, col)) in &jump.targets {
            let shown: String = match jump.first {
                None => label.iter().collect(),
                Some(first) if label[0] == first => label[1].to_string(),
                Some(_) => continue,
            };
            let Some(y) = row.checked_sub(self.scroll).and_then(|y| u16::try_from(y).ok()) else { continue };
            let Ok(x) = u16::try_from(*col) else { continue };
            if y < doc.height && x < doc.width {
                buf.set_stringn(doc.x + x, doc.y + y, shown, usize::from(doc.width - x), style);
            }
        }
    }
}

#[cfg(test)]
mod tests;
