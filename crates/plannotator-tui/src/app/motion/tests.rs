#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]

use plannotator_tui_schema::{DocumentSource, Provenance};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use super::{App, Word, words};
use crate::delivery::Discard;
use crate::wrap::Row;

/// Rows 0 and 2: `alpha beta` and `gamma-delta`, a gap between.
fn app() -> App {
    let source =
        DocumentSource::new("alpha beta\n\ngamma-delta\n".to_owned(), "plan.md", true, Provenance::Stdin);
    let mut app = App::open(source, 60, Box::new(Discard)).expect("app opens");
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    app
}

fn type_keys(app: &mut App, keys: &str) {
    for c in keys.chars() {
        let code = if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) };
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE))).expect("key");
    }
}

fn selected_text(app: &App) -> &str {
    let range = app.pending.as_ref().expect("a selection").range.clone();
    &app.open.doc.source[range]
}

#[test]
fn words_split_word_characters_from_punctuation_and_skip_unmapped_cells() {
    let row = Row {
        line: Line::from("• ab-cd e"),
        cells: vec![None, None, Some(0), Some(1), Some(2), Some(3), Some(4), None, Some(6)],
    };
    assert_eq!(
        words(&row),
        [
            Word { start: 2, end: 3 },
            Word { start: 4, end: 4 },
            Word { start: 5, end: 6 },
            Word { start: 8, end: 8 }
        ]
    );
}

#[test]
fn word_motions_cross_rows_like_vim() {
    let mut app = app();
    let mut visited = Vec::new();
    for key in ["w", "w", "e", "w", "w", "b", "b", "b"] {
        type_keys(&mut app, key);
        visited.push(app.cursor);
    }
    assert_eq!(visited, [(0, 6), (2, 0), (2, 4), (2, 5), (2, 6), (2, 5), (2, 0), (0, 6)]);
    type_keys(&mut app, "2w");
    assert_eq!(app.cursor, (2, 5));
}

#[test]
fn a_count_before_g_goes_to_that_source_line_or_the_next_rendered_one() {
    let mut app = app();
    type_keys(&mut app, "3G");
    assert_eq!(app.cursor, (2, 0));
    type_keys(&mut app, "1g");
    assert_eq!(app.cursor, (0, 0));
    type_keys(&mut app, "2G");
    assert_eq!(app.cursor, (2, 0));
}

#[test]
fn a_selection_can_cover_part_of_a_line_word_by_word() {
    let mut app = app();
    type_keys(&mut app, "wve\n");
    assert_eq!(selected_text(&app), "beta");
}

#[test]
fn goto_word_labels_the_nearest_words_first_and_extends_a_selection() {
    let mut app = app();
    type_keys(&mut app, "f");
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    let screen = format!("{:?}", terminal.backend().buffer());
    assert!(screen.contains("aapha abta") && screen.contains("acmma-adlta"), "{screen}");
    type_keys(&mut app, "ad");
    assert_eq!(app.cursor, (2, 6));
    type_keys(&mut app, "vfab\n");
    assert_eq!(selected_text(&app), "gamma-d");
}
