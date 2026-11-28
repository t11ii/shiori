//! Loads file lists and sidebar counts on a worker thread with its own SQLite
//! connection. Requests of each kind are coalesced while the worker is busy.

use crate::db::{Db, FileRow, Scope, SortKey};
use crate::query::Filter;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

pub struct Request {
    pub generation: u64,
    pub scope: Scope,
    pub filter: Filter,
    pub sort: SortKey,
    pub desc: bool,
    pub limit: usize,
}

pub struct Response {
    pub generation: u64,
    pub rows: anyhow::Result<Vec<FileRow>>,
    pub ms: f64,
}

#[derive(Default)]
pub struct SidebarResponse {
    pub generation: u64,
    pub profile_counts: HashMap<i64, i64>,
    pub tag_counts: HashMap<String, i64>,
}

enum Work {
    Files(Request),
    Sidebar(u64),
}

pub struct Searcher {
    tx: Sender<Work>,
    rx: Receiver<Response>,
    sidebar_rx: Receiver<SidebarResponse>,
}

impl Searcher {
    /// `wake` is called after each answer (the app asks egui for a repaint).
    pub fn new(db_path: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        let (tx, req_rx) = channel::<Work>();
        let (res_tx, rx) = channel::<Response>();
        let (sidebar_tx, sidebar_rx) = channel::<SidebarResponse>();
        std::thread::spawn(move || {
            let mut db = None;
            while let Ok(work) = req_rx.recv() {
                let mut files = None;
                let mut sidebar = None;
                for pending in std::iter::once(work).chain(req_rx.try_iter()) {
                    match pending {
                        Work::Files(req) => files = Some(req),
                        Work::Sidebar(generation) => sidebar = Some(generation),
                    }
                }
                let connection = db.get_or_insert_with(|| Db::open(&db_path));
                if let Some(req) = files {
                    let t = Instant::now();
                    let rows = match &*connection {
                        Ok(db) => db.search(&req.scope, &req.filter, req.sort, req.desc, req.limit),
                        Err(e) => Err(anyhow::anyhow!("{e:#}")),
                    };
                    let res = Response {
                        generation: req.generation,
                        rows,
                        ms: t.elapsed().as_secs_f64() * 1000.0,
                    };
                    if res_tx.send(res).is_err() {
                        return;
                    }
                    wake();
                }
                if let Some(generation) = sidebar {
                    let res = match &*connection {
                        Ok(db) => load_sidebar(db, generation),
                        Err(_) => SidebarResponse {
                            generation,
                            ..Default::default()
                        },
                    };
                    if sidebar_tx.send(res).is_err() {
                        return;
                    }
                    wake();
                }
                if connection.is_err() {
                    db = None;
                }
            }
        });
        Self { tx, rx, sidebar_rx }
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(Work::Files(req));
    }

    pub fn send_sidebar(&self, generation: u64) {
        let _ = self.tx.send(Work::Sidebar(generation));
    }

    /// The newest answer that arrived since the last call, if any.
    pub fn latest(&self) -> Option<Response> {
        self.rx.try_iter().last()
    }

    pub fn latest_sidebar(&self) -> Option<SidebarResponse> {
        self.sidebar_rx.try_iter().last()
    }
}

fn load_sidebar(db: &Db, generation: u64) -> SidebarResponse {
    let profile_counts = db
        .profiles()
        .unwrap_or_default()
        .into_iter()
        .filter(|profile| profile.query.is_none())
        .map(|profile| (profile.id, db.profile_count(profile.id).unwrap_or(0)))
        .collect();
    let tag_counts = db.tag_counts().unwrap_or_default().into_iter().collect();
    SidebarResponse {
        generation,
        profile_counts,
        tag_counts,
    }
}
