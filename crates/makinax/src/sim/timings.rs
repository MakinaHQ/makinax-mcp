//! Phase-level wall-clock for a multi-phase call (simulate, execute, …).
//!
//! The first latency profile put an execute call at 11.3s with no account of
//! where it went. That is not a performance problem yet — it is a MEASUREMENT
//! problem, and the two are routinely confused: without a decomposition, the
//! only way to answer "why 11 seconds" is to reach for a stopwatch and a
//! hypothesis, which is how a 7-second default poll interval sat unnoticed
//! behind a plausible story about chain latency.
//!
//! So a call carries its own numbers: a latency claim should be readable
//! off the response, not reconstructed. Every run decomposes for free, including the
//! runs nobody was watching.
//!
//! `rpc_count` is per phase where counting is cheap — it separates "this phase
//! is slow because the chain is slow" from "this phase makes 40 calls".

use std::time::Instant;

use serde::Serialize;

/// The phase boundary most recently crossed, readable from OUTSIDE the future
/// that is running.
///
/// A `Timings` object only becomes readable when its function returns. That is
/// fine for measuring and useless for a stall, which is precisely the case
/// where the function does not return. This is the one bit of state a deadline
/// can consult after giving up: not how long each phase took, but **which
/// phase was open when the clock ran out**.
///
/// Cheap by construction — one short lock per phase boundary, never held across
/// anything, and phase boundaries are a handful per call.
#[derive(Clone, Debug)]
pub struct StallMarker(std::sync::Arc<std::sync::Mutex<&'static str>>);

impl Default for StallMarker {
    fn default() -> Self {
        Self::new()
    }
}

impl StallMarker {
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new("start")))
    }

    /// The last phase that COMPLETED. The phase that overran is the one after
    /// it — stated that way in the refusal, because claiming to name the open
    /// phase would be claiming knowledge of a boundary never reached.
    pub fn last_completed(&self) -> &'static str {
        *self.0.lock().unwrap()
    }

    fn mark(&self, phase: &'static str) {
        *self.0.lock().unwrap() = phase;
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Phase {
    pub name: &'static str,
    pub ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_count: Option<u32>,
}

/// Accumulates phases in the order they ran, with a total that is measured
/// independently rather than summed — so a gap between `total` and the sum of
/// the parts is VISIBLE instead of definitionally zero. Unaccounted time is
/// exactly what a first profile needs to find.
#[derive(Debug)]
pub struct Timings {
    started: Instant,
    mark: Instant,
    phases: Vec<Phase>,
    /// Externally readable phase cursor, so a deadline that fires while this
    /// call is still running can say WHERE it was.
    stall: Option<StallMarker>,
}

impl Default for Timings {
    fn default() -> Self {
        Self::new()
    }
}

impl Timings {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            mark: now,
            phases: Vec::new(),
            stall: None,
        }
    }

    /// A `Timings` whose phase boundaries are also published to `marker`.
    pub fn marked(marker: StallMarker) -> Self {
        Self {
            stall: Some(marker),
            ..Self::new()
        }
    }

    /// Close the current phase and open the next.
    pub fn phase(&mut self, name: &'static str) {
        self.phase_with_rpc(name, None)
    }

    pub fn phase_with_rpc(&mut self, name: &'static str, rpc_count: Option<u32>) {
        if let Some(m) = &self.stall {
            m.mark(name);
        }
        let now = Instant::now();
        self.phases.push(Phase {
            name,
            ms: now.duration_since(self.mark).as_millis(),
            rpc_count,
        });
        self.mark = now;
    }

    /// Discard time since the last mark without recording a phase — for
    /// stretches that belong to no phase (e.g. an early return path).
    pub fn skip(&mut self) {
        self.mark = Instant::now();
    }

    pub fn total_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    /// `{phases: [...], total_ms, unaccounted_ms}`.
    ///
    /// `unaccounted_ms` is deliberately surfaced: if it is large, the phase
    /// boundaries are in the wrong places, and that is a finding rather than a
    /// rounding artifact.
    pub fn json(&self) -> serde_json::Value {
        let total = self.total_ms();
        let summed: u128 = self.phases.iter().map(|p| p.ms).sum();
        serde_json::json!({
            "phases": self.phases,
            "total_ms": total,
            "unaccounted_ms": total.saturating_sub(summed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_ordered_and_total_is_measured_not_summed() {
        let mut t = Timings::new();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.phase("first");
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.phase_with_rpc("second", Some(3));

        let j = t.json();
        let phases = j["phases"].as_array().unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0]["name"], "first");
        assert_eq!(phases[1]["name"], "second");
        assert_eq!(phases[1]["rpc_count"], 3);
        assert!(
            phases[0]["rpc_count"].is_null(),
            "a phase with nothing cheap to count omits the field rather than claiming 0"
        );

        // The acceptance property: phases sum to approximately the total.
        let total = j["total_ms"].as_u64().unwrap();
        let summed: u64 = phases.iter().map(|p| p["ms"].as_u64().unwrap()).sum();
        assert!(
            total >= summed,
            "total is measured independently: {total} < {summed}"
        );
        assert!(
            total - summed < 50,
            "phases must account for essentially all of the total (unaccounted {}ms)",
            total - summed
        );
    }

    /// Unaccounted time is REPORTED, not hidden — a large gap means the phase
    /// boundaries are wrong, which is a finding.
    #[test]
    fn skipped_time_shows_up_as_unaccounted() {
        let mut t = Timings::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        t.skip();
        t.phase("only");
        let j = t.json();
        assert!(
            j["unaccounted_ms"].as_u64().unwrap() >= 9,
            "deliberately skipped time must surface: {j}"
        );
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;

    /// The marker reports the last phase that COMPLETED, which is what a
    /// deadline can honestly say: the phase that overran is the one after it.
    #[test]
    fn the_marker_names_where_the_work_got_to() {
        let marker = StallMarker::new();
        assert_eq!(
            marker.last_completed(),
            "start",
            "before any boundary, the only honest answer is that nothing finished"
        );

        let mut t = Timings::marked(marker.clone());
        t.phase("resolution");
        assert_eq!(marker.last_completed(), "resolution");
        t.phase("simulation");
        assert_eq!(
            marker.last_completed(),
            "simulation",
            "a deadline firing here would report the phase AFTER simulation as the one that overran"
        );
    }

    /// **A synthetic stall discloses.** The future never completes; the
    /// deadline fires; the marker still says where it got to — which is the
    /// whole point, because a `Timings` only becomes readable when its function
    /// returns, and a stalled function does not.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_deadline_that_fires_can_still_say_where_it_stopped() {
        let marker = StallMarker::new();
        let m = marker.clone();

        let work = async move {
            let mut t = Timings::marked(m);
            t.phase("resolution");
            t.phase("simulation");
            // …and then never returns, the way an RPC that never answers does.
            std::future::pending::<()>().await;
        };

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(120), work).await;
        assert!(
            outcome.is_err(),
            "the deadline must fire on a stalled future"
        );
        assert_eq!(
            marker.last_completed(),
            "simulation",
            "the refusal can name where the work got to even though the future never returned"
        );
    }
}
