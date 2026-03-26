use std::sync::{Arc, Weak};

use tokio::sync::Mutex;

use super::state::ServerState;

impl ServerState {
    pub(crate) fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub(crate) struct SessionLockLease {
    state: ServerState,
    session_id: String,
    lock: Weak<Mutex<()>>,
}

impl SessionLockLease {
    pub(crate) fn new(state: ServerState, session_id: String, lock: Weak<Mutex<()>>) -> Self {
        Self {
            state,
            session_id,
            lock,
        }
    }
}

impl Drop for SessionLockLease {
    fn drop(&mut self) {
        let mut locks = self
            .state
            .session_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(current) = locks.get(&self.session_id)
            && Arc::as_ptr(current) == self.lock.as_ptr()
            && Arc::strong_count(current) == 2
        {
            locks.remove(&self.session_id);
        }
    }
}
