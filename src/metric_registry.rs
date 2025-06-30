use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use dashmap::DashMap;

/// Keep at most this many seconds of samples in each deque.
const MAX_WINDOW: Duration = Duration::from_secs(60);

/// Rolling-window metric registry (thread-safe, lock-free reads).
#[derive(Clone, Default)]
pub struct MetricRegistry {
    /// container-id → deque of (timestamp, CPU fraction 0.0–1.0)
    pub cpu: Arc<DashMap<String, VecDeque<(Instant, f64)>>>,
    /// container-id → deque of (timestamp, memory bytes as f64)
    pub mem: Arc<DashMap<String, VecDeque<(Instant, f64)>>>,
}

impl MetricRegistry {
    /* ───────────── public API ───────────── */

    pub fn record_cpu(&self, id: &str, usage: f64) {
        let mut guard = self.cpu.entry(id.to_owned()).or_default();
        Self::insert_sample(guard.value_mut(), usage);
    }

    pub fn record_mem(&self, id: &str, bytes: u64) {
        let mut guard = self.mem.entry(id.to_owned()).or_default();
        Self::insert_sample(guard.value_mut(), bytes as f64);
    }

    pub fn cpu_avg(&self, id: &str, window: Duration) -> Option<f64> {
        self.avg(&self.cpu, id, window)
    }

    pub fn mem_max(&self, id: &str, window: Duration) -> Option<u64> {
        self.max(&self.mem, id, window).map(|v| v as u64)
    }

    /* ──────────── internals ──────────── */

    fn insert_sample(q: &mut VecDeque<(Instant, f64)>, value: f64) {
        let now = Instant::now();
        q.push_back((now, value));
        while let Some((ts, _)) = q.front() {
            if now.duration_since(*ts) > MAX_WINDOW {
                q.pop_front();
            } else {
                break;
            }
        }
    }

    fn avg(
        &self,
        map: &DashMap<String, VecDeque<(Instant, f64)>>,
        id: &str,
        window: Duration,
    ) -> Option<f64> {
        map.get(id).and_then(|q| {
            let now = Instant::now();
            let mut sum = 0.0;
            let mut n = 0;
            for &(ts, v) in q.iter().rev() {
                if now.duration_since(ts) > window {
                    break;
                }
                sum += v;
                n += 1;
            }
            (n > 0).then(|| sum / n as f64)
        })
    }

    fn max(
        &self,
        map: &DashMap<String, VecDeque<(Instant, f64)>>,
        id: &str,
        window: Duration,
    ) -> Option<f64> {
        map.get(id).and_then(|q| {
            let now = Instant::now();
            q.iter()
                .rev()
                .take_while(|&&(ts, _)| now.duration_since(ts) <= window)
                .map(|&(_, v)| v)
                .filter(|v| !v.is_nan())
                .max_by(|a, b| a.partial_cmp(b).unwrap())
        })
    }
}
