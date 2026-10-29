//! Noticing that the reviewed file changed on disk, typically an agent acting on sent
//! notes: the document reloads and those notes are archived, `undo-finish` brings them
//! back. A reload waits while the reviewer is selecting, typing or choosing.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use plannotator_tui_schema::Provenance;

use super::{App, Mode};
use crate::keys::Action;

/// How often the event loop may stat the open file.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// What a file looked like on disk: enough to tell that it was rewritten without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiskStamp {
    modified: SystemTime,
    len: u64,
}

impl DiskStamp {
    /// The file's stamp now, or `None` when it is missing or cannot be read.
    pub(super) fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self { modified: meta.modified().ok()?, len: meta.len() })
    }

    /// The stamp of a document's file, when the document is one.
    pub(super) fn of_source(provenance: &Provenance) -> Option<Self> {
        match provenance {
            Provenance::File { path } => Self::of(path),
            _ => None,
        }
    }
}

impl App {
    /// Called from the event loop on every wake; checks at most once per `CHECK_EVERY`.
    /// Returns whether the screen needs a redraw.
    pub(crate) fn poll_disk(&mut self) -> bool {
        let now = Instant::now();
        if self.disk_checked.is_some_and(|last| now.duration_since(last) < CHECK_EVERY) {
            return false;
        }
        self.disk_checked = Some(now);
        self.check_disk()
    }

    /// Compare the open file with the stamp it had when it was loaded; on a change, reload
    /// and archive the sent notes. A missing or unreadable file says nothing, and a reload
    /// that fails is reported once per change.
    pub(super) fn check_disk(&mut self) -> bool {
        let Some(loaded) = self.open.disk else { return false };
        let Some(now) = DiskStamp::of_source(&self.open.source.provenance) else { return false };
        if now == loaded || self.open.disk_noticed == Some(now) {
            return false;
        }
        if self.mode != Mode::Browse || self.pending.is_some() || self.selection.is_some() {
            return false;
        }
        self.open.disk_noticed = Some(now);
        let archived = self.reload().and_then(|()| self.archive_open_sent());
        match archived {
            Ok(0) => self.status = self.status.take().map(|status| format!("file changed: {status}")),
            Ok(count) => {
                let undo = self.keys.labelled(Action::UndoFinish, "undo").map(|undo| format!(" · {undo}"));
                self.status = Some(format!(
                    "file changed: reloaded, archived {count} sent note(s){}",
                    undo.unwrap_or_default()
                ));
            }
            Err(err) => self.status = Some(format!("file changed; reload failed: {err:#}")),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests assert by panicking")]

    use std::path::Path;
    use std::time::Duration;

    use plannotator_tui_schema::Kind;

    use crate::app::review_test_support::{file_app, press};

    /// Rewrite `file_app`'s document with different content and a later mtime.
    fn edit_on_disk(root: &Path, app: &crate::app::App) {
        let path = root.join("docs/a.md");
        std::fs::write(&path, "# Plan\n\none\n\ntwo\n\nthree\n\nfour, from the agent\n").expect("rewrite");
        let later = app.open.disk.expect("stamp").modified + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).expect("open").set_modified(later).expect("mtime");
    }

    #[test]
    fn an_unchanged_file_says_nothing() {
        let (root, mut app, _) = file_app("disk-unchanged");
        assert!(!app.check_disk());
        assert!(!app.poll_disk());
        assert_eq!(app.status, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_change_on_disk_reloads_and_archives_only_the_sent_notes_until_undone() {
        let (root, mut app, _) = file_app("disk-changed");
        app.add_quote_annotation("one", Kind::Comment, "sent".into()).expect("sent");
        press(&mut app, 'E');
        app.add_quote_annotation("two", Kind::Comment, "unsent".into()).expect("unsent");
        edit_on_disk(&root, &app);
        assert!(app.poll_disk());
        assert!(app.open.doc.source.contains("four, from the agent"));
        assert_eq!(app.status.as_deref(), Some("file changed: reloaded, archived 1 sent note(s) · U undo"));
        let bodies: Vec<String> = app.open.store.placed().iter().map(|p| p.annotation.body.clone()).collect();
        assert_eq!(bodies, ["unsent"]);
        assert!(!app.check_disk());
        press(&mut app, 'U');
        assert_eq!(app.open.store.placed().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_change_waits_while_a_selection_is_open() {
        let (root, mut app, _) = file_app("disk-busy");
        press(&mut app, 'v');
        edit_on_disk(&root, &app);
        assert!(!app.check_disk());
        assert!(!app.open.doc.source.contains("four, from the agent"));
        app.handle_event(&ratatui::crossterm::event::Event::Key(
            ratatui::crossterm::event::KeyCode::Esc.into(),
        ))
        .expect("esc");
        assert!(app.check_disk());
        assert!(app.open.doc.source.contains("four, from the agent"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_removed_file_says_nothing() {
        let (root, mut app, _) = file_app("disk-removed");
        std::fs::remove_file(root.join("docs/a.md")).expect("remove");
        assert!(!app.check_disk());
        assert_eq!(app.status, None);
        let _ = std::fs::remove_dir_all(root);
    }
}
