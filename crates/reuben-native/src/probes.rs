//! The nodes a client has asked the engine to meter, held on a lease.
//!
//! A client names node paths with `SetProbes`; the host switches those nodes' probes on in the
//! installed Plan's [`ProbeSlots`] and answers `GetMeters` from them. Each `set` renews a lease: a
//! client that goes away without clearing its choice would otherwise leave Render measuring for
//! ever, so a set that is not renewed within [`LEASE`] is switched off. A thread from
//! [`spawn_lease_thread`] checks once a second, so this holds when no client is left to poll.
//!
//! Only structure-channel threads and the lease thread touch this, behind a `Mutex` in the host.
//! Render reads the flags through the slots and never takes that lock.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use reuben_api::engine::{ChannelLevel, PortLevel, ProbeLevels};
use reuben_api::probe::{NodeProbe, ProbeSlots};

/// How long a probe set stays on without a renewing `set`.
pub(crate) const LEASE: Duration = Duration::from_secs(5);

/// The node at `path`: `path[0]` in `slots`, each further name in the previous node's hosted slots.
fn resolve<'a>(slots: &'a ProbeSlots, path: &[String]) -> Option<&'a NodeProbe> {
    let (first, rest) = path.split_first()?;
    let mut node = slots.find(first)?;
    for name in rest {
        node = node.hosted.as_deref()?.find(name)?;
    }
    Some(node)
}

/// How often the lease thread checks for a lapsed lease.
pub(crate) const LEASE_CHECK: Duration = Duration::from_secs(1);

pub(crate) struct Probes {
    lease: Duration,
    slots: Option<Arc<ProbeSlots>>,
    /// The paths last requested, in request order. Those that do not resolve in `slots` stay held,
    /// so a Plan that has them again turns them on.
    held: Vec<Vec<String>>,
    leased_at: Option<Instant>,
}

impl Default for Probes {
    fn default() -> Self {
        Self::with_lease(LEASE)
    }
}

impl Probes {
    pub(crate) fn with_lease(lease: Duration) -> Self {
        Self {
            lease,
            slots: None,
            held: Vec::new(),
            leased_at: None,
        }
    }

    /// Replace the set with `paths`, renew the lease, and return the paths that resolved, in
    /// request order. Every node the old set had on that the new one does not is turned off.
    /// The count is capped at the structure server, before the host is asked.
    pub(crate) fn set(&mut self, paths: Vec<Vec<String>>, now: Instant) -> Vec<Vec<String>> {
        let mut unique: Vec<Vec<String>> = Vec::with_capacity(paths.len());
        for path in paths {
            if !unique.contains(&path) {
                unique.push(path);
            }
        }
        let paths = unique;
        let mut resolved = Vec::new();
        if let Some(slots) = &self.slots {
            resolved = paths
                .iter()
                .filter(|p| resolve(slots, p).is_some())
                .cloned()
                .collect();
            for old in self.held.iter().filter(|p| !paths.contains(p)) {
                if let Some(node) = resolve(slots, old) {
                    node.set_on(false);
                }
            }
            // Turning on a node that is already on changes nothing, so a renewal keeps its levels.
            for path in &resolved {
                if let Some(node) = resolve(slots, path) {
                    node.set_on(true);
                }
            }
        }
        self.held = paths;
        self.leased_at = Some(now);
        resolved
    }

    /// Map the held paths onto a new Plan's `slots`: those resolving are turned on, the rest stay
    /// held but unreported. The old slots go with the old Plan, so nothing is turned off in them.
    /// A lease that lapsed before `now` is expired first, so a swap after it turns nothing on.
    pub(crate) fn install(&mut self, slots: Arc<ProbeSlots>, now: Instant) {
        self.expire_if_due(now);
        for node in self.held.iter().filter_map(|p| resolve(&slots, p)) {
            node.set_on(true);
        }
        self.slots = Some(slots);
    }

    /// Turn the set off if `now` is a lease past the last `set`.
    pub(crate) fn expire_if_due(&mut self, now: Instant) {
        if self
            .leased_at
            .is_some_and(|at| now.saturating_duration_since(at) >= self.lease)
        {
            self.expire();
        }
    }

    /// The held nodes' levels, after [`expire_if_due`](Self::expire_if_due).
    pub(crate) fn report(&mut self, now: Instant) -> Vec<ProbeLevels> {
        self.expire_if_due(now);
        let Some(slots) = &self.slots else {
            return Vec::new();
        };
        self.held
            .iter()
            .filter_map(|path| {
                let ports = resolve(slots, path)?
                    .ports
                    .iter()
                    .map(|port| {
                        let (peak, mean_square) = port.read();
                        PortLevel {
                            port: port.name.to_string(),
                            level: ChannelLevel::from_linear(peak, mean_square),
                        }
                    })
                    .collect();
                Some(ProbeLevels {
                    path: path.clone(),
                    ports,
                })
            })
            .collect()
    }

    fn expire(&mut self) {
        if let Some(slots) = &self.slots {
            for path in &self.held {
                if let Some(node) = resolve(slots, path) {
                    node.set_on(false);
                }
            }
        }
        self.held.clear();
        self.leased_at = None;
    }
}

/// Expire `probes` every `period` until its last strong owner is dropped. Detached: it holds only
/// a `Weak`, so it ends within one period of the host going away.
pub(crate) fn spawn_lease_thread(probes: Weak<Mutex<Probes>>, period: Duration) {
    std::thread::spawn(move || loop {
        std::thread::sleep(period);
        let Some(probes) = probes.upgrade() else {
            return;
        };
        probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .expire_if_due(Instant::now());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use reuben_api::render::operators::{oscillator, output, Oscillator, Output};
    use reuben_api::render::{AudioConfig, Graph};

    const BLOCK: usize = 128;

    fn names(path: &[&str]) -> Vec<String> {
        path.iter().map(|s| s.to_string()).collect()
    }

    /// `/out`, and `/osc` into it unless `with_osc` is false.
    fn graph(with_osc: bool) -> Graph {
        let mut g = Graph::new();
        let out = g.add("/out", Output::new());
        if with_osc {
            let osc = g.add("/osc", Oscillator::new());
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
        }
        g
    }

    fn slots_of(g: &Graph) -> Arc<ProbeSlots> {
        ProbeSlots::for_graph(g, &AudioConfig::new(48_000.0, BLOCK))
    }

    /// Outer `/osc` and `/out`, and a Voicer `/v` hosting its own `/osc`.
    fn hosting(with_osc: bool) -> Arc<ProbeSlots> {
        let mut outer = graph(with_osc);
        outer.add("/v", Oscillator::new());
        let mut outer = slots_of(&outer);
        let voice = slots_of(&graph(true));
        let v = Arc::get_mut(&mut outer)
            .expect("sole owner")
            .nodes
            .iter_mut()
            .find(|n| &*n.address == "/v")
            .expect("/v has a slot");
        v.hosted = Some(voice);
        outer
    }

    fn probes_on(slots: &Arc<ProbeSlots>) -> Probes {
        let mut p = Probes::default();
        p.install(slots.clone(), Instant::now());
        p
    }

    fn on_addresses(slots: &ProbeSlots) -> Vec<String> {
        slots
            .nodes
            .iter()
            .filter(|n| n.is_on())
            .map(|n| n.address.to_string())
            .collect()
    }

    fn reported_paths(p: &mut Probes) -> Vec<Vec<String>> {
        p.report(Instant::now())
            .into_iter()
            .map(|l| l.path)
            .collect()
    }

    #[test]
    fn a_path_resolves_through_a_hosting_node() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let resolved = p.set(vec![names(&["/v", "/osc"])], Instant::now());
        assert_eq!(resolved, vec![names(&["/v", "/osc"])]);
        assert!(on_addresses(&slots).is_empty());
        let voice = slots.find("/v").unwrap().hosted.as_ref().unwrap();
        assert_eq!(on_addresses(voice), vec!["/osc"]);
    }

    #[test]
    fn an_unknown_path_is_left_out_of_the_answer() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let resolved = p.set(
            vec![
                names(&["/nope"]),
                names(&["/osc"]),
                names(&["/out", "/osc"]),
                vec![],
            ],
            Instant::now(),
        );
        assert_eq!(resolved, vec![names(&["/osc"])]);
        assert_eq!(on_addresses(&slots), vec!["/osc"]);
    }

    #[test]
    fn a_set_turns_off_what_it_no_longer_names() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"]), names(&["/v", "/osc"])], now);
        p.set(vec![names(&["/out"])], now);
        assert_eq!(on_addresses(&slots), vec!["/out"]);
        let voice = slots.find("/v").unwrap().hosted.as_ref().unwrap();
        assert!(on_addresses(voice).is_empty());
    }

    #[test]
    // Only Render writes a level, so clearing a loud level on re-enable is pinned in reuben-core's
    // probe tests; this pins that the host's off-then-on still reports the node, at the floor.
    fn re_enabling_a_probe_starts_from_silence() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        p.set(vec![], now);
        p.set(vec![names(&["/osc"])], now);
        // The next block Render commits applies the reset the re-enable asked for.
        slots.commit(BLOCK);
        let report = p.report(now);
        assert_eq!(report.len(), 1);
        assert_eq!(
            report[0].ports[0].level,
            ChannelLevel::from_linear(0.0, 0.0)
        );
    }

    #[test]
    fn a_swap_drops_a_renamed_probe_and_keeps_the_rest() {
        let first = hosting(true);
        let mut p = probes_on(&first);
        p.set(vec![names(&["/osc"]), names(&["/out"])], Instant::now());

        let second = hosting(false);
        p.install(second.clone(), Instant::now());
        assert_eq!(reported_paths(&mut p), vec![names(&["/out"])]);
        assert_eq!(on_addresses(&second), vec!["/out"]);

        // Held while absent: a later Plan with `/osc` again brings it back.
        let third = hosting(true);
        p.install(third.clone(), Instant::now());
        assert_eq!(
            reported_paths(&mut p),
            vec![names(&["/osc"]), names(&["/out"])]
        );
        assert_eq!(on_addresses(&third), vec!["/osc", "/out"]);
    }

    #[test]
    fn an_unrenewed_probe_set_expires() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        assert_eq!(p.report(now + Duration::from_secs(4)).len(), 1);
        assert!(p.report(now + LEASE).is_empty());
        assert!(!slots.any_on());
    }

    #[test]
    fn a_set_turns_off_a_held_node_when_an_unresolved_path_sits_beside_it() {
        let first = hosting(false);
        let mut p = probes_on(&first);
        let now = Instant::now();
        p.set(vec![names(&["/osc"]), names(&["/out"])], now);
        assert_eq!(on_addresses(&first), vec!["/out"]);
        p.set(vec![names(&["/osc"])], now);
        assert!(on_addresses(&first).is_empty());
    }

    #[test]
    fn a_set_names_each_path_once() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let resolved = p.set(
            vec![names(&["/out"]), names(&["/osc"]), names(&["/out"])],
            Instant::now(),
        );
        assert_eq!(resolved, vec![names(&["/out"]), names(&["/osc"])]);
    }

    #[test]
    fn a_swap_after_the_lease_lapsed_turns_nothing_on() {
        let first = hosting(true);
        let mut p = probes_on(&first);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        let second = hosting(true);
        p.install(second.clone(), now + LEASE);
        assert!(!first.any_on());
        assert!(!second.any_on());
        assert!(p.report(now + LEASE).is_empty());
    }

    #[test]
    fn a_set_before_any_install_answers_empty_and_the_install_turns_it_on() {
        let mut p = Probes::default();
        let now = Instant::now();
        assert!(p.set(vec![names(&["/osc"])], now).is_empty());
        let slots = hosting(true);
        p.install(slots.clone(), now);
        assert_eq!(on_addresses(&slots), vec!["/osc"]);
    }

    #[test]
    fn the_lease_thread_expires_a_set_nobody_reads() {
        let slots = hosting(true);
        let probes = Arc::new(Mutex::new(Probes::with_lease(Duration::from_millis(30))));
        probes
            .lock()
            .unwrap()
            .install(slots.clone(), Instant::now());
        probes
            .lock()
            .unwrap()
            .set(vec![names(&["/osc"])], Instant::now());
        assert!(slots.any_on());
        spawn_lease_thread(Arc::downgrade(&probes), Duration::from_millis(5));
        let deadline = Instant::now() + Duration::from_secs(2);
        while slots.any_on() {
            assert!(
                Instant::now() < deadline,
                "the lease thread never expired it"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_set_renews_the_lease() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        p.set(vec![names(&["/osc"])], now + Duration::from_secs(4));
        assert_eq!(p.report(now + Duration::from_secs(8)).len(), 1);
    }
}
