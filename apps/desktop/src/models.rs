//! Speech models on this Mac, and their downloads: one place for Settings
//! and calls, so a download started in one shows in the other and outlives
//! the call that asked for it.

use std::collections::HashMap;

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use quipcall_core::models::{self, Download, Progress};
use quipcall_core::{ModelInfo, catalog};

use crate::store;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// The model and the VAD are here.
    Ready,
    /// Bytes so far, of everything this model needs.
    Downloading {
        done: u64,
        total: u64,
    },
    Missing,
    Failed(String),
}

struct Fetch {
    done: u64,
    total: u64,
    _task: Task<()>,
}

pub struct Models {
    fetching: HashMap<&'static str, Fetch>,
    failed: HashMap<&'static str, String>,
}

struct GlobalModels(Entity<Models>);
impl Global for GlobalModels {}

impl Models {
    pub fn global(cx: &mut App) -> Entity<Models> {
        if let Some(g) = cx.try_global::<GlobalModels>() {
            return g.0.clone();
        }
        let models = cx.new(|_| Models {
            fetching: HashMap::new(),
            failed: HashMap::new(),
        });
        cx.set_global(GlobalModels(models.clone()));
        models
    }

    /// The global, if anything made it yet (for render paths).
    pub fn global_ref(cx: &App) -> Option<Entity<Models>> {
        cx.try_global::<GlobalModels>().map(|g| g.0.clone())
    }

    pub fn state(&self, info: &'static ModelInfo) -> State {
        if let Some(f) = self.fetching.get(info.id) {
            return State::Downloading {
                done: f.done,
                total: f.total,
            };
        }
        if let Some(e) = self.failed.get(info.id) {
            return State::Failed(e.clone());
        }
        let dir = store::models_dir();
        if models::installed(info, &dir) && models::installed(&catalog::VAD, &dir) {
            State::Ready
        } else {
            State::Missing
        }
    }

    pub fn is_ready(&self, info: &'static ModelInfo) -> bool {
        self.state(info) == State::Ready
    }

    /// Download `info`, and the VAD with it if it's missing.
    pub fn fetch(&mut self, info: &'static ModelInfo, cx: &mut Context<Self>) {
        if self.fetching.contains_key(info.id) || self.is_ready(info) {
            return;
        }
        self.failed.remove(info.id);
        let dir = store::models_dir();
        let mut wanted: Vec<&'static ModelInfo> = vec![info];
        if !models::installed(&catalog::VAD, &dir) {
            wanted.insert(0, &catalog::VAD);
        }
        let total = wanted.iter().map(|m| m.bytes).sum();
        let task = cx.spawn(async move |this, cx| {
            let mut before = 0;
            for model in wanted {
                let download: Download = models::download(model, dir.clone());
                let last = loop {
                    match download.progress.recv().await {
                        Ok(Progress::Bytes(n)) => {
                            let ok = this.update(cx, |this, cx| {
                                if let Some(f) = this.fetching.get_mut(info.id) {
                                    f.done = before + n;
                                    cx.notify();
                                }
                            });
                            if ok.is_err() {
                                return;
                            }
                        }
                        Ok(last) => break last,
                        Err(_) => break Progress::Cancelled,
                    }
                };
                before += model.bytes;
                let failed = match last {
                    Progress::Done(_) => None,
                    Progress::Failed(e) => Some(e),
                    Progress::Cancelled | Progress::Bytes(_) => Some(String::new()),
                };
                if let Some(e) = failed {
                    let _ = this.update(cx, |this, cx| {
                        this.fetching.remove(info.id);
                        if !e.is_empty() {
                            this.failed.insert(info.id, e);
                        }
                        cx.notify();
                    });
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.fetching.remove(info.id);
                cx.notify();
            });
        });
        self.fetching.insert(
            info.id,
            Fetch {
                done: 0,
                total,
                _task: task,
            },
        );
        cx.notify();
    }

    /// Stop a download; what came is kept for next time.
    pub fn cancel(&mut self, info: &'static ModelInfo, cx: &mut Context<Self>) {
        if self.fetching.remove(info.id).is_some() {
            cx.notify();
        }
    }

    pub fn remove(&mut self, info: &'static ModelInfo, cx: &mut Context<Self>) {
        self.cancel(info, cx);
        self.failed.remove(info.id);
        if let Err(e) = models::remove(info, &store::models_dir()) {
            tracing::warn!("removing {}: {e}", info.file);
        }
        cx.notify();
    }
}
