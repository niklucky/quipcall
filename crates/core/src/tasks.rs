//! Engine-owned async work on its runtime. Child tasks inherit the
//! engine's scope; completed tasks leave it, and teardown cancels leftovers.
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Weak},
};
use tokio::{
    sync::Notify,
    task::{AbortHandle, JoinHandle},
};

tokio::task_local! { static CURRENT: Tasks; }

#[derive(Clone)]
pub(crate) struct Tasks(Arc<Inner>);
struct Inner {
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    changed: Notify,
    cancelled: Notify,
}
#[derive(Default)]
struct State {
    next: u64,
    closed: bool,
    active: BTreeMap<u64, AbortHandle>,
}
struct Finished(Weak<Inner>, u64);
impl Drop for Finished {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            inner.state.lock().active.remove(&self.1);
            inner.changed.notify_one();
        }
    }
}
impl Tasks {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self(Arc::new(Inner {
            runtime,
            state: Default::default(),
            changed: Notify::new(),
            cancelled: Notify::new(),
        }))
    }
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut state = self.0.state.lock();
        if state.closed {
            drop(state);
            drop(future);
            let task = self.0.runtime.spawn(std::future::pending::<F::Output>());
            task.abort();
            return task;
        }
        state.next += 1;
        let id = state.next;
        let finished = Finished(Arc::downgrade(&self.0), id);
        let task = self
            .0
            .runtime
            .spawn(CURRENT.scope(self.clone(), async move {
                let _finished = finished;
                future.await
            }));
        state.active.insert(id, task.abort_handle());
        task
    }

    pub async fn cancelled(&self) {
        let notified = self.0.cancelled.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.0.state.lock().closed {
            notified.await;
        }
    }

    pub fn cancel(&self) {
        let mut state = self.0.state.lock();
        state.closed = true;
        for task in state.active.values() {
            task.abort();
        }
        self.0.cancelled.notify_waiters();
    }
    pub async fn shutdown(&self) {
        self.cancel();
        loop {
            let changed = self.0.changed.notified();
            if self.0.state.lock().active.is_empty() {
                break;
            }
            changed.await;
        }
    }
}

/// For helpers already running in an engine task, including their descendants.
pub(crate) fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    CURRENT.with(|tasks| tasks.spawn(future))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Probe(async_channel::Sender<()>);
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }

    #[test]
    fn engine_drop_cancels_nested_waiters_and_late_callbacks_without_stopping_other_work() {
        crate::runtime().unwrap().block_on(async {
            let (other, _) = crate::Engine::new().unwrap();
            let (release, waiting) = async_channel::bounded::<()>(1);
            let other_task = other.tasks.spawn(async move {
                waiting.recv().await.unwrap();
                7
            });
            for _ in 0..12 {
                let (engine, _) = crate::Engine::new().unwrap();
                let scope = engine.tasks.clone();
                let (ended, dropped) = async_channel::unbounded();
                let (started, ready) = async_channel::unbounded();
                let parent = Probe(ended.clone());
                let child = Probe(ended);
                // Keep the channel sender alive outside the engine indefinitely.
                let (_sender, receiver) = async_channel::unbounded::<()>();
                let task = engine.tasks.spawn(async move {
                    let _parent = parent;
                    let child_started = started.clone();
                    spawn(async move {
                        let _child = child;
                        child_started.send(()).await.unwrap();
                        receiver.recv().await.unwrap();
                    });
                    started.send(()).await.unwrap();
                    std::future::pending::<()>().await;
                });
                ready.recv().await.unwrap();
                ready.recv().await.unwrap();
                drop(engine);
                tokio::time::timeout(Duration::from_secs(2), async {
                    dropped.recv().await.unwrap();
                    dropped.recv().await.unwrap();
                    task.await.unwrap_err();
                })
                .await
                .unwrap();
                assert!(scope.0.state.lock().closed);
                // A native callback retaining its scope cannot restart work.
                assert!(
                    scope
                        .spawn(async { panic!("late callback ran") })
                        .await
                        .unwrap_err()
                        .is_cancelled()
                );
            }
            release.send(()).await.unwrap();
            assert_eq!(other_task.await.unwrap(), 7);
            assert_eq!(
                crate::runtime().unwrap().spawn(async { 9 }).await.unwrap(),
                9
            );
        });
    }

    #[test]
    fn completed_tasks_are_removed_and_timed_out_goodbye_is_cancelled() {
        crate::runtime().unwrap().block_on(async {
            let (engine, _) = crate::Engine::new().unwrap();
            for _ in 0..50 {
                engine.tasks.spawn(async {}).await.unwrap();
            }
            assert!(engine.tasks.0.state.lock().active.is_empty());
            let (ended, dropped) = async_channel::bounded(2);
            let untracked = Probe(ended.clone());
            let (started, ready) = async_channel::bounded(1);
            engine.rt().spawn(async move {
                tokio::spawn(async move {
                    let _probe = untracked;
                    started.send(()).await.unwrap();
                    std::future::pending::<()>().await;
                });
            });
            ready.recv().await.unwrap();
            let probe = Probe(ended);
            let goodbye = engine.tasks.spawn(async move {
                let _probe = probe;
                std::future::pending::<()>().await;
            });
            use futures_util::FutureExt as _;
            engine
                .state
                .lock()
                .closing
                .push(goodbye.map(|_| ()).boxed().shared());
            drop(engine);
            tokio::time::timeout(crate::CLOSE_TIMEOUT + Duration::from_secs(2), async {
                dropped.recv().await.unwrap();
                dropped.recv().await
            })
            .await
            .unwrap()
            .unwrap();
        });
    }
}
