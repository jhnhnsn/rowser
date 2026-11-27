//! Requests that wait for an answer, for `rowser pending`, `rowser approve`
//! and `rowser deny`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use tokio::time::Instant;

use super::{Dismiss, Notice, Subject};
use crate::config::Target;
use crate::protocol::{PendingInfo, RequestKind};

/// An answer from the terminal.
#[derive(Debug)]
pub(super) enum TerminalAnswer {
    /// `rowser approve`, with the `--open` Target when given.
    Approve(Option<Target>),
    Deny,
}

struct Entry {
    notice: Notice,
    since: Instant,
    answer: oneshot::Sender<TerminalAnswer>,
    dismiss: Dismiss,
}

/// What `take` gives to the terminal: where its answer goes.
pub(super) struct Taken {
    answer: oneshot::Sender<TerminalAnswer>,
    dismiss: Dismiss,
}

impl Taken {
    /// Gives the answer to the waiting request and closes its notification
    /// or dialog.
    pub(super) fn answer(self, answer: TerminalAnswer) {
        let _ = self.answer.send(answer);
        self.dismiss.dismiss();
    }
}

#[derive(Default)]
pub(super) struct Pending {
    state: Mutex<(u64, BTreeMap<u64, Entry>)>,
}

/// Removes its request from `Pending` when dropped.
pub(super) struct Guard {
    pending: Arc<Pending>,
    pub(super) id: u64,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.pending.take(self.id);
    }
}

impl Pending {
    /// Adds a request that waits for an answer. Ids count up from 1.
    pub(super) fn add(
        self: &Arc<Self>,
        notice: Notice,
        dismiss: Dismiss,
    ) -> (Guard, oneshot::Receiver<TerminalAnswer>) {
        let (tx, rx) = oneshot::channel();
        let mut s = self.lock();
        s.0 += 1;
        let id = s.0;
        s.1.insert(
            id,
            Entry {
                notice,
                since: Instant::now(),
                answer: tx,
                dismiss,
            },
        );
        let guard = Guard {
            pending: self.clone(),
            id,
        };
        (guard, rx)
    }

    /// Removes the request `id`: the first answer takes it. `None` when it
    /// is unknown or already answered.
    pub(super) fn take(&self, id: u64) -> Option<Taken> {
        self.lock().1.remove(&id).map(|e| Taken {
            answer: e.answer,
            dismiss: e.dismiss,
        })
    }

    /// The subject of request `id`, while it waits.
    pub(super) fn subject(&self, id: u64) -> Option<Subject> {
        self.lock().1.get(&id).map(|e| e.notice.subject.clone())
    }

    /// The waiting requests, oldest first.
    pub(super) fn list(&self) -> Vec<PendingInfo> {
        self.lock()
            .1
            .iter()
            .map(|(id, e)| {
                let (host, unverified) = e.notice.host.parts();
                let (kind, url, ports) = match &e.notice.subject {
                    Subject::Url(u) => (RequestKind::Open, Some(u.to_string()), vec![]),
                    Subject::Forward(p) => (RequestKind::Forward, None, p.clone()),
                    Subject::NeedsLogin
                    | Subject::VersionMismatch { .. }
                    | Subject::Updated { .. } => unreachable!(
                        "a Link notice never waits for an answer, so it is never pending"
                    ),
                };
                PendingInfo {
                    id: *id,
                    kind,
                    host: host.to_string(),
                    unverified,
                    url,
                    ports,
                    target: e.notice.target.as_ref().map(Target::to_string),
                    age_secs: e.since.elapsed().as_secs(),
                }
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, BTreeMap<u64, Entry>)> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}
