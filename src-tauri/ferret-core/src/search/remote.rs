//! Servers in search: only connections whose settings say so (off by default), and only while
//! connected. Listed with one `find` over the session (the user's home and a few folders where
//! servers keep things: SERVER_ROOTS) when a search runs and it hasn't been listed yet, again
//! every 15 minutes while it stays connected, and dropped when it disconnects: nothing about a
//! server's files is kept once it's closed.

use super::engine::{Engine, Volume};
use super::index::Index;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::linux::Host;

const RESCAN_EVERY: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct State {
    scanned: HashMap<String, Instant>,
    busy: HashSet<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}

/// Indexes of the connected servers that are searched.
pub fn engine() -> &'static Arc<Engine> {
    static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
    ENGINE.get_or_init(Engine::new)
}

/// (Re)lists the connected servers that are due, in the background; drops the others.
pub fn refresh() {
    let searched: Vec<String> = crate::remote::connections().into_iter().filter(|c| c.search).map(|c| c.id).collect();
    let open: Vec<String> = crate::remote::ssh::connected_ids().into_iter().filter(|id| searched.contains(id)).collect();
    let roots: Vec<String> = open.iter().map(|id| crate::paths::remote_path(id, "/")).collect();
    // Search turned off, or disconnected: gone from search at once.
    let volumes = engine().volumes();
    if volumes.iter().any(|v| !roots.contains(&v.root)) {
        engine().set_volumes(volumes.into_iter().filter(|v| roots.contains(&v.root)).collect());
    }
    for id in open {
        {
            let mut st = state().lock();
            let due = st.scanned.get(&id).is_none_or(|t| t.elapsed() >= RESCAN_EVERY);
            if !due || !st.busy.insert(id.clone()) {
                continue;
            }
        }
        std::thread::spawn(move || scan(id));
    }
}

fn scan(id: String) {
    let root = crate::paths::remote_path(&id, "/");
    let Some(session) = crate::remote::ssh::connected(&id) else {
        state().lock().busy.remove(&id);
        return;
    };
    if !engine().volumes().iter().any(|v| v.root == root) {
        engine().add_volume(Volume::new(Index::new(&root, true), false));
    }
    let started = Instant::now();
    let mut roots = vec![session.home.as_str()];
    roots.extend(super::wsl::SERVER_ROOTS.iter().filter(|r| **r != session.home));
    // Servers that only allow SFTP can't run find: they're left out of search.
    let listed = session.arch.as_ref().and_then(|_| super::wsl::scan(&Host::Ssh(id.clone()), &root, &roots));
    let mut st = state().lock();
    st.busy.remove(&id);
    st.scanned.insert(id.clone(), Instant::now());
    drop(st);
    // Disconnected meanwhile: nothing is kept.
    let still = crate::remote::ssh::connected(&id).is_some();
    match listed.filter(|_| still) {
        Some(ix) => {
            log::info!("search: listed {} ({} items) in {:?}", session.name, ix.live(), started.elapsed());
            engine().add_volume(Volume::new(ix, true));
        }
        None => engine().set_volumes(engine().volumes().into_iter().filter(|v| v.root != root).collect()),
    }
}

/// A connection closed (or its search was turned off): its index goes.
pub fn forget(id: &str) {
    let root = crate::paths::remote_path(id, "/");
    state().lock().scanned.remove(id);
    engine().set_volumes(engine().volumes().into_iter().filter(|v| v.root != root).collect());
}
