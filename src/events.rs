//! The in-memory event bus (spec §11.6).
//!
//! The durable source of truth for events is the `events` table (the cursor); this bus
//! adds two things a table alone can't give cheaply: **wakeups** (so a subscriber thread
//! sleeps instead of polling until new events land) and **retention bookkeeping** (the
//! lowest still-replayable `seq`, used to answer `cursor_expired`). The server updates the
//! bus whenever it appends or trims events; subscriber threads read the actual rows back
//! from the store.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Shared handle to the event bus.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<BusState>,
    cvar: Condvar,
}

#[derive(Debug, Clone, Copy)]
struct BusState {
    /// Highest event seq written so far (0 = none).
    latest_seq: i64,
    /// Lowest seq still guaranteed replayable; anything below has been trimmed.
    oldest_retained_seq: i64,
    /// Set once the server begins graceful shutdown — wakes all waiters.
    shutting_down: bool,
}

/// The outcome of [`EventBus::wait_for`].
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// `latest_seq` reached the requested value — new events are available.
    Ready,
    /// The server is shutting down; the subscriber should send `server_stopping` and stop.
    ShuttingDown,
    /// The wait timed out with no new events (used to re-check liveness periodically).
    TimedOut,
}

impl EventBus {
    /// Create a bus seeded with the store's current `latest_seq` and `oldest_retained_seq`.
    pub fn new(latest_seq: i64, oldest_retained_seq: i64) -> EventBus {
        EventBus {
            inner: Arc::new(Inner {
                state: Mutex::new(BusState {
                    latest_seq,
                    oldest_retained_seq: oldest_retained_seq.max(1),
                    shutting_down: false,
                }),
                cvar: Condvar::new(),
            }),
        }
    }

    /// Record that events up to `new_latest` are now durably written; wake subscribers.
    pub fn published(&self, new_latest: i64) {
        let mut st = self.inner.state.lock().unwrap();
        if new_latest > st.latest_seq {
            st.latest_seq = new_latest;
        }
        self.inner.cvar.notify_all();
    }

    /// Record the new lowest replayable seq after a retention trim.
    pub fn set_oldest_retained(&self, seq: i64) {
        let mut st = self.inner.state.lock().unwrap();
        st.oldest_retained_seq = seq.max(1);
    }

    /// `(latest_seq, oldest_retained_seq)`.
    pub fn cursor(&self) -> (i64, i64) {
        let st = self.inner.state.lock().unwrap();
        (st.latest_seq, st.oldest_retained_seq)
    }

    /// The highest seq written so far.
    pub fn latest_seq(&self) -> i64 {
        self.inner.state.lock().unwrap().latest_seq
    }

    /// Whether a subscription resuming from `since_seq` (it has consumed through
    /// `since_seq`, wants `since_seq+1`..) has fallen off the retained window.
    pub fn is_expired(&self, since_seq: i64) -> bool {
        let st = self.inner.state.lock().unwrap();
        since_seq + 1 < st.oldest_retained_seq
    }

    /// Begin graceful shutdown: wake every waiter so they can finish.
    pub fn shutdown(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.shutting_down = true;
        self.inner.cvar.notify_all();
    }

    /// True once [`shutdown`](Self::shutdown) has been called.
    pub fn is_shutting_down(&self) -> bool {
        self.inner.state.lock().unwrap().shutting_down
    }

    /// Block until `latest_seq >= want_seq`, the bus shuts down, or `timeout` elapses.
    pub fn wait_for(&self, want_seq: i64, timeout: Duration) -> WaitOutcome {
        let deadline = Instant::now() + timeout;
        let mut st = self.inner.state.lock().unwrap();
        loop {
            if st.shutting_down {
                return WaitOutcome::ShuttingDown;
            }
            if st.latest_seq >= want_seq {
                return WaitOutcome::Ready;
            }
            let now = Instant::now();
            if now >= deadline {
                return WaitOutcome::TimedOut;
            }
            let (guard, res) = self.inner.cvar.wait_timeout(st, deadline - now).unwrap();
            st = guard;
            if res.timed_out() {
                if st.shutting_down {
                    return WaitOutcome::ShuttingDown;
                }
                if st.latest_seq >= want_seq {
                    return WaitOutcome::Ready;
                }
                return WaitOutcome::TimedOut;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn wait_returns_ready_when_published() {
        let bus = EventBus::new(0, 1);
        let b2 = bus.clone();
        let h = thread::spawn(move || b2.wait_for(1, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(50));
        bus.published(1);
        assert_eq!(h.join().unwrap(), WaitOutcome::Ready);
    }

    #[test]
    fn wait_returns_ready_immediately_if_already_ahead() {
        let bus = EventBus::new(5, 1);
        assert_eq!(
            bus.wait_for(3, Duration::from_millis(10)),
            WaitOutcome::Ready
        );
    }

    #[test]
    fn wait_times_out() {
        let bus = EventBus::new(0, 1);
        assert_eq!(
            bus.wait_for(1, Duration::from_millis(30)),
            WaitOutcome::TimedOut
        );
    }

    #[test]
    fn shutdown_wakes_waiters() {
        let bus = EventBus::new(0, 1);
        let b2 = bus.clone();
        let h = thread::spawn(move || b2.wait_for(99, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(50));
        bus.shutdown();
        assert_eq!(h.join().unwrap(), WaitOutcome::ShuttingDown);
    }

    #[test]
    fn expiry_detection() {
        let bus = EventBus::new(10, 8);
        assert!(bus.is_expired(5)); // wants 6.., but 6 < 8
        assert!(bus.is_expired(6)); // wants 7.., 7 < 8
        assert!(!bus.is_expired(7)); // wants 8.., 8 present
        assert!(!bus.is_expired(8)); // wants 9..
        assert!(!bus.is_expired(20)); // ahead
    }
}
