//! Runs list queries on a worker thread so typing in the search box or
//! switching folders never stalls drawing. The worker has its own SQLite
//! connection (WAL mode lets it read while the UI thread writes) and always
//! answers the newest request, skipping any that piled up while it was busy.

use crate::db::{Db, FileRow, Scope, SortKey};
use crate::query::Filter;
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

pub struct Searcher {
    tx: Sender<Request>,
    rx: Receiver<Response>,
}

impl Searcher {
    /// `wake` is called after each answer (the app asks egui for a repaint).
    pub fn new(db_path: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (res_tx, rx) = channel::<Response>();
        std::thread::spawn(move || {
            let db = Db::open(&db_path);
            while let Ok(mut req) = req_rx.recv() {
                // only the latest request matters
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let t = Instant::now();
                let rows = match &db {
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
        });
        Self { tx, rx }
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// The newest answer that arrived since the last call, if any.
    pub fn latest(&self) -> Option<Response> {
        let mut last = None;
        while let Ok(r) = self.rx.try_recv() {
            last = Some(r);
        }
        last
    }
}
