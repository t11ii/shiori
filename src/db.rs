//! SQLite-backed metadata store: files, tags, stars, labels, comments and virtual folders.

use anyhow::{Context as _, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::path::{Path, PathBuf, MAIN_SEPARATOR};

use crate::query::Filter;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id       INTEGER PRIMARY KEY,
    path     TEXT NOT NULL UNIQUE,
    parent   TEXT NOT NULL,
    name     TEXT NOT NULL,
    ext      TEXT NOT NULL DEFAULT '',
    size     INTEGER NOT NULL DEFAULT 0,
    mtime    INTEGER NOT NULL DEFAULT 0,
    is_dir   INTEGER NOT NULL DEFAULT 0,
    star     INTEGER NOT NULL DEFAULT 0,
    label    INTEGER NOT NULL DEFAULT 0,
    comment  TEXT NOT NULL DEFAULT '',
    missing  INTEGER NOT NULL DEFAULT 0,
    seen     INTEGER NOT NULL DEFAULT 1,
    added    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_files_parent ON files(parent);
CREATE INDEX IF NOT EXISTS idx_files_name ON files(name);
CREATE TABLE IF NOT EXISTS tags (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS file_tags (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (file_id, tag_id)
);
CREATE INDEX IF NOT EXISTS idx_file_tags_tag ON file_tags(tag_id);
CREATE TABLE IF NOT EXISTS roots (
    id   INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS profiles (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL,
    query TEXT
);
CREATE TABLE IF NOT EXISTS profile_items (
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    PRIMARY KEY (profile_id, file_id)
);
"#;

/// Label colors. Index 0 means "no label".
pub const LABELS: [(&str, &str, [u8; 3]); 8] = [
    ("none", "なし", [0, 0, 0]),
    ("red", "赤", [230, 70, 70]),
    ("orange", "橙", [240, 150, 50]),
    ("yellow", "黄", [230, 200, 40]),
    ("green", "緑", [80, 180, 90]),
    ("blue", "青", [70, 130, 230]),
    ("purple", "紫", [160, 100, 220]),
    ("gray", "灰", [140, 140, 140]),
];

#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    pub is_dir: bool,
    pub star: u8,
    pub label: u8,
    pub comment: String,
    pub missing: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    /// `Some(query)` makes this a smart folder whose contents are computed from a search.
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    All,
    Folder { path: String, recursive: bool },
    Profile(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    Name,
    Modified,
    Size,
    Star,
}

pub struct Db {
    pub conn: Connection,
    pub path: PathBuf,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Normalizes a path to the string form stored in the database.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if s.len() > 1 && s.ends_with(MAIN_SEPARATOR) && !s.ends_with(&format!(":{MAIN_SEPARATOR}")) {
        s.trim_end_matches(MAIN_SEPARATOR).to_string()
    } else {
        s
    }
}

/// Returns `[lo, hi)` bounds that select every path strictly below `dir` using the path index.
pub fn subtree_range(dir: &str) -> (String, String) {
    let mut lo = dir.to_string();
    if !lo.ends_with(MAIN_SEPARATOR) {
        lo.push(MAIN_SEPARATOR);
    }
    let mut hi = lo.clone();
    hi.pop();
    hi.push(char::from_u32(MAIN_SEPARATOR as u32 + 1).unwrap());
    (lo, hi)
}

pub fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn row_to_file(r: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    let tags: Option<String> = r.get(11)?;
    let mut tags: Vec<String> = tags
        .map(|t| t.split('\u{1f}').map(str::to_string).collect())
        .unwrap_or_default();
    tags.sort_by_key(|t| t.to_lowercase());
    Ok(FileRow {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
        ext: r.get(3)?,
        size: r.get(4)?,
        mtime: r.get(5)?,
        is_dir: r.get::<_, i64>(6)? != 0,
        star: r.get::<_, i64>(7)? as u8,
        label: r.get::<_, i64>(8)? as u8,
        comment: r.get(9)?,
        missing: r.get::<_, i64>(10)? != 0,
        tags,
    })
}

const SELECT_FILE: &str = "SELECT f.id, f.path, f.name, f.ext, f.size, f.mtime, f.is_dir, f.star, f.label, f.comment, f.missing,
    (SELECT group_concat(t.name, char(31)) FROM file_tags ft JOIN tags t ON t.id = ft.tag_id WHERE ft.file_id = f.id)
    FROM files f";

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = open_conn(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn open_in(dir: &Path) -> Result<Self> {
        Self::open(&dir.join("shiori.db"))
    }

    // ---------- roots ----------

    pub fn add_root(&self, p: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO roots(path) VALUES (?)",
            [path_key(p)],
        )?;
        Ok(())
    }

    pub fn remove_root(&self, p: &str) -> Result<()> {
        self.conn.execute("DELETE FROM roots WHERE path = ?", [p])?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT path FROM roots ORDER BY path")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    // ---------- file registration ----------

    /// Inserts or refreshes a single path (used for drag & drop). Returns its id.
    pub fn register_path(&self, p: &Path) -> Result<i64> {
        upsert_path(&self.conn, p)?;
        let id =
            self.conn
                .query_row("SELECT id FROM files WHERE path = ?", [path_key(p)], |r| {
                    r.get(0)
                })?;
        Ok(id)
    }

    pub fn file(&self, id: i64) -> Result<Option<FileRow>> {
        let sql = format!("{SELECT_FILE} WHERE f.id = ?");
        Ok(self.conn.query_row(&sql, [id], row_to_file).optional()?)
    }

    // ---------- search ----------

    pub fn search(
        &self,
        scope: &Scope,
        filter: &Filter,
        sort: SortKey,
        desc: bool,
        limit: usize,
    ) -> Result<Vec<FileRow>> {
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut dirs_first = false;

        let mut filter = filter.clone();
        match scope {
            Scope::All => {}
            Scope::Folder { path, recursive } => {
                dirs_first = true;
                if *recursive {
                    let (lo, hi) = subtree_range(path);
                    conds.push("f.path >= ? AND f.path < ?".into());
                    args.push(lo.into());
                    args.push(hi.into());
                } else {
                    conds.push("f.parent = ?".into());
                    args.push(path.clone().into());
                }
            }
            Scope::Profile(pid) => {
                let q: Option<Option<String>> = self
                    .conn
                    .query_row("SELECT query FROM profiles WHERE id = ?", [pid], |r| {
                        r.get(0)
                    })
                    .optional()?;
                match q {
                    Some(Some(q)) => filter = filter.merged(&Filter::parse(&q)),
                    _ => {
                        conds.push(
                            "f.id IN (SELECT file_id FROM profile_items WHERE profile_id = ?)"
                                .into(),
                        );
                        args.push((*pid).into());
                    }
                }
            }
        }

        filter.to_sql(&mut conds, &mut args);

        let mut sql = SELECT_FILE.to_string();
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        let dir = if desc { "DESC" } else { "ASC" };
        let key = match sort {
            SortKey::Name => format!("f.name COLLATE NOCASE {dir}"),
            SortKey::Modified => format!("f.mtime {dir}, f.name COLLATE NOCASE"),
            SortKey::Size => format!("f.size {dir}, f.name COLLATE NOCASE"),
            SortKey::Star => format!("f.star {dir}, f.name COLLATE NOCASE"),
        };
        if dirs_first {
            sql.push_str(&format!(" ORDER BY f.is_dir DESC, {key}"));
        } else {
            sql.push_str(&format!(" ORDER BY {key}"));
        }
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map(params_from_iter(args.iter()), row_to_file)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------- attributes ----------

    fn update_many(&self, sql: &str, value: rusqlite::types::Value, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(sql)?;
            for id in ids {
                st.execute(params![value, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_star(&self, ids: &[i64], star: u8) -> Result<()> {
        self.update_many(
            "UPDATE files SET star = ? WHERE id = ?",
            (star.min(5) as i64).into(),
            ids,
        )
    }

    pub fn set_label(&self, ids: &[i64], label: u8) -> Result<()> {
        self.update_many(
            "UPDATE files SET label = ? WHERE id = ?",
            (label as i64).into(),
            ids,
        )
    }

    pub fn set_comment(&self, ids: &[i64], comment: &str) -> Result<()> {
        self.update_many(
            "UPDATE files SET comment = ? WHERE id = ?",
            comment.to_string().into(),
            ids,
        )
    }

    fn tag_id(&self, name: &str) -> Result<i64> {
        self.conn
            .execute("INSERT OR IGNORE INTO tags(name) VALUES (?)", [name])?;
        Ok(self
            .conn
            .query_row("SELECT id FROM tags WHERE name = ?", [name], |r| r.get(0))?)
    }

    pub fn add_tag(&self, ids: &[i64], name: &str) -> Result<()> {
        let name = name.trim().trim_start_matches('#');
        if name.is_empty() {
            return Ok(());
        }
        let tid = self.tag_id(name)?;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st =
                tx.prepare("INSERT OR IGNORE INTO file_tags(file_id, tag_id) VALUES (?, ?)")?;
            for id in ids {
                st.execute([id, &tid])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_tag(&self, ids: &[i64], name: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(
                "DELETE FROM file_tags WHERE file_id = ? AND tag_id = (SELECT id FROM tags WHERE name = ?)",
            )?;
            for id in ids {
                st.execute(params![id, name])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All tags with the number of files carrying them.
    pub fn tag_counts(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT t.name, COUNT(ft.file_id) FROM tags t LEFT JOIN file_tags ft ON ft.tag_id = t.id
             GROUP BY t.id ORDER BY t.name COLLATE NOCASE",
        )?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn rename_tag(&self, old: &str, new: &str) -> Result<()> {
        let new = new.trim();
        if new.is_empty() || old == new {
            return Ok(());
        }
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM tags WHERE name = ? AND name != ?",
                [new, old],
                |r| r.get(0),
            )
            .optional()?;
        let tx = self.conn.unchecked_transaction()?;
        if let Some(target) = existing {
            // merge into the existing tag
            tx.execute(
                "INSERT OR IGNORE INTO file_tags(file_id, tag_id)
                 SELECT file_id, ? FROM file_tags WHERE tag_id = (SELECT id FROM tags WHERE name = ?)",
                params![target, old],
            )?;
            tx.execute("DELETE FROM tags WHERE name = ?", [old])?;
        } else {
            tx.execute("UPDATE tags SET name = ? WHERE name = ?", [new, old])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tags WHERE name = ?", [name])?;
        Ok(())
    }

    // ---------- profiles (virtual folders) ----------

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        let mut st = self
            .conn
            .prepare("SELECT id, name, query FROM profiles ORDER BY name COLLATE NOCASE")?;
        let v = st
            .query_map([], |r| {
                Ok(Profile {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    query: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn create_profile(&self, name: &str, query: Option<&str>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO profiles(name, query) VALUES (?, ?)",
            params![name, query],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_profile(&self, id: i64, name: &str, query: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE profiles SET name = ?, query = ? WHERE id = ?",
            params![name, query, id],
        )?;
        Ok(())
    }

    pub fn delete_profile(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM profiles WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn add_to_profile(&self, pid: i64, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR IGNORE INTO profile_items(profile_id, file_id) VALUES (?, ?)",
            )?;
            for id in ids {
                st.execute([pid, *id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_from_profile(&self, pid: i64, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st =
                tx.prepare("DELETE FROM profile_items WHERE profile_id = ? AND file_id = ?")?;
            for id in ids {
                st.execute([pid, *id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn profiles_of(&self, file_id: i64) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare("SELECT profile_id FROM profile_items WHERE file_id = ?")?;
        let v = st
            .query_map([file_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn profile_count(&self, pid: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM profile_items WHERE profile_id = ?",
            [pid],
            |r| r.get(0),
        )?)
    }

    // ---------- file operations ----------

    /// Renames a file or folder on disk and keeps its metadata (and that of its children).
    pub fn rename(&self, id: i64, new_name: &str) -> Result<String> {
        let f = self.file(id)?.context("file not found")?;
        let new_name = new_name.trim();
        anyhow::ensure!(!new_name.is_empty(), "name is empty");
        anyhow::ensure!(
            !new_name.contains(['/', '\\']),
            "name must not contain path separators"
        );
        let old = PathBuf::from(&f.path);
        let new = old.with_file_name(new_name);
        anyhow::ensure!(!new.exists(), "a file with that name already exists");
        std::fs::rename(&old, &new).with_context(|| format!("rename {}", old.display()))?;
        let new_key = path_key(&new);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE files SET path = ?, name = ?, ext = ? WHERE id = ?",
            params![new_key, new_name, ext_of(&new), id],
        )?;
        if f.is_dir {
            move_subtree(&tx, &f.path, &new_key)?;
        }
        tx.commit()?;
        Ok(new_key)
    }

    /// Moves files to the OS trash and forgets them.
    pub fn trash(&self, ids: &[i64]) -> Result<()> {
        for id in ids {
            if let Some(f) = self.file(*id)? {
                if Path::new(&f.path).exists() {
                    trash::delete(&f.path).with_context(|| format!("trash {}", f.path))?;
                }
                self.forget(&f)?;
            }
        }
        Ok(())
    }

    /// Removes a row (and its subtree) from the database without touching the disk.
    pub fn forget_ids(&self, ids: &[i64]) -> Result<()> {
        for id in ids {
            if let Some(f) = self.file(*id)? {
                self.forget(&f)?;
            }
        }
        Ok(())
    }

    fn forget(&self, f: &FileRow) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE id = ?", [f.id])?;
        if f.is_dir {
            let (lo, hi) = subtree_range(&f.path);
            self.conn
                .execute("DELETE FROM files WHERE path >= ? AND path < ?", [lo, hi])?;
        }
        Ok(())
    }
}

/// Rewrites every stored path under `old_dir` to live under `new_dir`.
fn move_subtree(conn: &Connection, old_dir: &str, new_dir: &str) -> Result<()> {
    let (lo, hi) = subtree_range(old_dir);
    let new_prefix = subtree_range(new_dir).0;
    let n = lo.chars().count() as i64;
    conn.execute(
        "UPDATE files SET path = ?1 || substr(path, ?2 + 1),
                          parent = CASE WHEN parent = ?3 THEN ?4 ELSE ?1 || substr(parent, ?2 + 1) END
         WHERE path >= ?5 AND path < ?6",
        params![new_prefix, n, old_dir, new_dir, lo, hi],
    )?;
    Ok(())
}

/// Insert-or-update one path from the filesystem. Marks it as seen.
pub fn upsert_path(conn: &Connection, p: &Path) -> Result<()> {
    let md = std::fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
    upsert_meta(conn, p, &md)
}

pub fn upsert_meta(conn: &Connection, p: &Path, md: &std::fs::Metadata) -> Result<()> {
    let key = path_key(p);
    let parent = p.parent().map(path_key).unwrap_or_default();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| key.clone());
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let is_dir = md.is_dir();
    let size = if is_dir { 0 } else { md.len() as i64 };
    let ext = if is_dir { String::new() } else { ext_of(p) };
    conn.prepare_cached(
        "INSERT INTO files(path, parent, name, ext, size, mtime, is_dir, seen, missing, added)
         VALUES (?, ?, ?, ?, ?, ?, ?, 1, 0, ?)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime,
             is_dir = excluded.is_dir, seen = 1, missing = 0",
    )?
    .execute(params![
        key,
        parent,
        name,
        ext,
        size,
        mtime,
        is_dir as i64,
        now()
    ])?;
    Ok(())
}
