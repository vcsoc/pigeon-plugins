//! Bounded worker pools. Every request from the host runs on one of these threads, never on the protocol
//! threads, so a slow server can only occupy workers. A full queue is reported to the caller at once
//! (`busy`), so a flood never grows memory without limit.

use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

pub type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Pool {
    tx: SyncSender<Job>,
}

impl Pool {
    pub fn new(name: &str, threads: usize, capacity: usize) -> Pool {
        let (tx, rx) = sync_channel::<Job>(capacity);
        let rx = Arc::new(Mutex::new(rx));
        for index in 0..threads {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("{name}-{index}"))
                .spawn(move || loop {
                    let job = {
                        let Ok(queue) = rx.lock() else { break };
                        queue.recv()
                    };
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                })
                .expect("start plugin worker");
        }
        Pool { tx }
    }

    /// Queues a job, or returns it back when the pool is saturated.
    pub fn submit(&self, job: Job) -> Result<(), Job> {
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => Err(job),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn saturated_pool_refuses_instead_of_growing() {
        let pool = Pool::new("test", 1, 1);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // One job runs and blocks the only thread; one job fills the queue; the third is refused.
        let first = pool.submit(Box::new(move || {
            let _ = gate_rx.recv_timeout(Duration::from_secs(5));
        }));
        assert!(first.is_ok());
        std::thread::sleep(Duration::from_millis(20));
        assert!(pool.submit(Box::new(|| {})).is_ok());
        assert!(pool.submit(Box::new(|| {})).is_err());
        let _ = gate_tx.send(());
    }

    #[test]
    fn jobs_run_on_pool_threads_not_the_caller() {
        let pool = Pool::new("test", 2, 8);
        let (tx, rx) = mpsc::channel();
        let caller = std::thread::current().id();
        let queued = pool.submit(Box::new(move || {
            let _ = tx.send(std::thread::current().id());
        }));
        assert!(queued.is_ok());
        let worker = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(worker, caller);
    }
}
