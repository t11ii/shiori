//! Background folder indexing.

use crate::db::{open_conn, path_key, subtree_range, upsert_meta};
use anyhow::{Context as _, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

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

/// What the index knew about an entry of the folder being compared.
struct Known {
    id: i64,
    size: i64,
    mtime: i64,
    is_dir: bool,
    missing: bool,
}

/// Brings the index for `root` up to date with the disk at `db_path`.
///
/// Folders are compared one at a time with what the index already holds, and
/// only new or changed entries are written, so a rescan that finds nothing new
/// costs little more than listing the folders.
///
/// Files that disappeared are deleted from the index unless the user attached
/// metadata to them (tags, stars, labels, comments, virtual folders); those are
/// kept and flagged as missing so nothing the user typed is silently lost.
/// A folder that cannot be read is left as indexed (it may be offline).
pub fn scan_root(
    db_path: &Path,
    root: &str,
    tx: Option<&Sender<ScanMsg>>,
) -> Result<(usize, usize)> {
    let mut conn = open_conn(db_path)?;
    std::fs::read_dir(root).with_context(|| format!("cannot read {root}"))?;

    let mut count = 0usize;
    let mut removed = 0usize;
    let mut next_progress = 2000usize;
    let mut batch = conn.transaction()?;
    let mut stack = vec![PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue; // permission denied etc.
        };
        let mut known = known_children(&batch, &path_key(&dir))?;
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if skip_name(&name) {
                continue;
            }
            // does not follow symbolic links
            let Ok(md) = entry.metadata() else { continue };
            let path = entry.path();
            count += 1;
            match known.remove(&name) {
                Some(k) if !k.missing && k.is_dir == md.is_dir() && same_stamp(&k, &md) => {}
                Some(k) => {
                    if k.is_dir && !md.is_dir() {
                        // a folder became a file: what was inside is gone
                        removed += drop_gone(&batch, None, &path_key(&path))?;
                    }
                    upsert_meta(&batch, &path, &md)?;
                }
                None => upsert_meta(&batch, &path, &md)?,
            }
            if md.is_dir() {
                stack.push(path);
            }
        }
        // indexed but no longer on disk (or now skipped)
        for (name, k) in known {
            removed += drop_gone(&batch, Some(k.id), &path_key(&dir.join(name)))?;
        }
        if count >= next_progress {
            next_progress = count + 2000;
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
    Ok((count, removed))
}

fn known_children(conn: &Connection, parent: &str) -> Result<HashMap<String, Known>> {
    let mut st = conn.prepare_cached(
        "SELECT name, id, size, mtime, is_dir, missing FROM files WHERE parent = ?",
    )?;
    let rows = st.query_map([parent], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Known {
                id: r.get(1)?,
                size: r.get(2)?,
                mtime: r.get(3)?,
                is_dir: r.get::<_, i64>(4)? != 0,
                missing: r.get::<_, i64>(5)? != 0,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Size and modification time as `upsert_meta` stores them.
fn same_stamp(k: &Known, md: &std::fs::Metadata) -> bool {
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let size = if md.is_dir() { 0 } else { md.len() as i64 };
    k.mtime == mtime && k.size == size
}

/// Forgets an entry that is gone (row `id`, if given, and everything below
/// `path`): rows without user metadata are deleted, the rest flagged missing.
/// Returns how many rows were deleted.
fn drop_gone(conn: &Connection, id: Option<i64>, path: &str) -> Result<usize> {
    let (lo, hi) = subtree_range(path);
    let id = id.unwrap_or(-1);
    let removed = conn
        .prepare_cached(
            "DELETE FROM files WHERE (id = ?1 OR (path >= ?2 AND path < ?3))
               AND star = 0 AND label = 0 AND comment = ''
               AND NOT EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = files.id)
               AND NOT EXISTS (SELECT 1 FROM profile_items pi WHERE pi.file_id = files.id)",
        )?
        .execute(params![id, lo, hi])?;
    conn.prepare_cached(
        "UPDATE files SET missing = 1
         WHERE (id = ?1 OR (path >= ?2 AND path < ?3)) AND missing = 0",
    )?
    .execute(params![id, lo, hi])?;
    Ok(removed)
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
