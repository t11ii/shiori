//! Background folder indexing.

use crate::db::{open_conn, subtree_range, upsert_meta};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use walkdir::WalkDir;

#[derive(Debug)]
pub enum ScanMsg {
    Progress {
        root: String,
        count: usize,
    },
    Done {
        root: String,
        count: usize,
        removed: usize,
    },
    Error {
        root: String,
        message: String,
    },
}

/// Names that are never indexed.
fn skip_name(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "$RECYCLE.BIN"
                | "System Volume Information"
                | "node_modules"
                | "Thumbs.db"
                | "desktop.ini"
        )
}

/// Walks `root` and updates the database at `db_path`.
///
/// Files that disappeared are deleted from the index unless the user attached
/// metadata to them (tags, stars, labels, comments, virtual folders); those are
/// kept and flagged as missing so nothing the user typed is silently lost.
pub fn scan_root(
    db_path: &Path,
    root: &str,
    tx: Option<&Sender<ScanMsg>>,
) -> Result<(usize, usize)> {
    let mut conn = open_conn(db_path)?;
    let (lo, hi) = subtree_range(root);
    conn.execute(
        "UPDATE files SET seen = 0 WHERE path >= ? AND path < ?",
        [&lo, &hi],
    )?;

    let mut count = 0usize;
    let mut batch = conn.transaction()?;
    let walker = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !skip_name(&e.file_name().to_string_lossy()));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // permission denied etc.
        };
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        upsert_meta(&batch, entry.path(), &md)?;
        count += 1;
        if count.is_multiple_of(2000) {
            batch.commit()?;
            batch = conn.transaction()?;
            if let Some(tx) = tx {
                let _ = tx.send(ScanMsg::Progress {
                    root: root.to_string(),
                    count,
                });
            }
        }
    }
    batch.commit()?;

    let removed = conn.execute(
        "DELETE FROM files WHERE seen = 0 AND path >= ?1 AND path < ?2
           AND star = 0 AND label = 0 AND comment = ''
           AND NOT EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = files.id)
           AND NOT EXISTS (SELECT 1 FROM profile_items pi WHERE pi.file_id = files.id)",
        [&lo, &hi],
    )?;
    conn.execute(
        "UPDATE files SET missing = 1, seen = 1 WHERE seen = 0 AND path >= ? AND path < ?",
        [&lo, &hi],
    )?;
    Ok((count, removed))
}

/// Spawns a thread that scans the given roots one after another.
pub fn spawn_scan(
    db_path: PathBuf,
    roots: Vec<String>,
    tx: Sender<ScanMsg>,
    ctx: eframe::egui::Context,
) {
    std::thread::spawn(move || {
        for root in roots {
            let msg = match scan_root(&db_path, &root, Some(&tx)) {
                Ok((count, removed)) => ScanMsg::Done {
                    root: root.clone(),
                    count,
                    removed,
                },
                Err(e) => ScanMsg::Error {
                    root: root.clone(),
                    message: format!("{e:#}"),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
    });
}
