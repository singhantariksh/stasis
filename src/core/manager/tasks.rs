use tokio::task::JoinHandle;
use crate::log::log_debug_message;

/// Hard cap on concurrent background tasks.
const MAX_SPAWNED_TASKS: usize = 10;

pub struct TaskHandles {
    pub idle: Option<JoinHandle<()>>,
    pub lock: Option<JoinHandle<()>>,
    pub media: Option<JoinHandle<()>>,
    pub input: Option<JoinHandle<()>>,
    pub spawned: Vec<JoinHandle<()>>,
}

impl TaskHandles {
    pub fn new() -> Self {
        Self {
            idle: None,
            lock: None,
            media: None,
            input: None,
            spawned: Vec::new(),
        }
    }

    pub fn abort_all(&mut self) {
        if let Some(h) = self.idle.take() { h.abort(); }
        if let Some(h) = self.lock.take() { h.abort(); }
        if let Some(h) = self.media.take() { h.abort(); }
        if let Some(h) = self.input.take() { h.abort(); }
        for h in self.spawned.drain(..) { h.abort(); }
    }
}

/// Clean up finished tasks from a vector of JoinHandles.
pub fn cleanup_tasks(tasks: &mut Vec<JoinHandle<()>>) {
    tasks.retain(|h| !h.is_finished());
}

/// Spawn a task while respecting the MAX_SPAWNED_TASKS limit.
/// Automatically cleans up completed tasks before spawning.
pub fn spawn_task_limited<F>(tasks: &mut Vec<JoinHandle<()>>, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    cleanup_tasks(tasks);

    if tasks.len() < MAX_SPAWNED_TASKS {
        tasks.push(tokio::spawn(fut));
    } else {
        log_debug_message("Max spawned tasks reached, skipping task spawn");
    }
}
