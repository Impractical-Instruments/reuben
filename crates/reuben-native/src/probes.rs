//! The nodes a client has asked the engine to meter, held on a lease.
//!
//! A client names node paths with `SetProbes`; the host switches those nodes' probes on in the
//! installed Plan's [`ProbeSlots`] and answers `GetMeters` from them. Each `set` renews a lease: a
//! client that goes away without clearing its choice would otherwise leave Render measuring for
//! ever, so a set that is not renewed within [`LEASE`] is switched off the next time it is read.
//!
//! Only structure-channel threads touch this, behind a `Mutex` in the host. Render reads the
//! flags through the slots and never takes that lock.

use std::sync::Arc;
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

#[derive(Default)]
pub(crate) struct Probes {
    slots: Option<Arc<ProbeSlots>>,
    /// The paths that are on, in request order: all of them resolve in `slots`.
    held: Vec<Vec<String>>,
    leased_at: Option<Instant>,
}

impl Probes {
    /// Replace the set with `paths`, renew the lease, and return the paths that resolved, in
    /// request order. Every node the old set had on that the new one does not is turned off.
    pub(crate) fn set(&mut self, paths: Vec<Vec<String>>, now: Instant) -> Vec<Vec<String>> {
        let resolved: Vec<Vec<String>> = match &self.slots {
            Some(slots) => paths
                .into_iter()
                .filter(|p| resolve(slots, p).is_some())
                .collect(),
            None => Vec::new(),
        };
        if let Some(slots) = &self.slots {
            for old in self.held.iter().filter(|p| !resolved.contains(p)) {
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
        self.held = resolved.clone();
        self.leased_at = Some(now);
        resolved
    }

    /// Carry the held paths over to a new Plan's `slots`: those still resolving are turned on, the
    /// rest dropped. The old slots go with the old Plan, so nothing is turned off in them.
    pub(crate) fn install(&mut self, slots: Arc<ProbeSlots>) {
        self.held.retain(|p| match resolve(&slots, p) {
            Some(node) => {
                node.set_on(true);
                true
            }
            None => false,
        });
        self.slots = Some(slots);
    }

    /// The held nodes' levels, after expiring the set if `now` is a [`LEASE`] past the last `set`.
    pub(crate) fn report(&mut self, now: Instant) -> Vec<ProbeLevels> {
        if self
            .leased_at
            .is_some_and(|at| now.saturating_duration_since(at) >= LEASE)
        {
            self.expire();
        }
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
                        let (peak, mean_square) = port.level.read();
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
        p.install(slots.clone());
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
    fn re_enabling_a_probe_starts_from_silence() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        let osc = slots.find("/osc").unwrap();
        // Render's part: fold a loud block and commit it.
        osc.ports[0].level.commit(
            reuben_api::meter::Block {
                peak: 1.0,
                sum_squares: BLOCK as f32,
            },
            BLOCK,
            &reuben_api::meter::Ballistics::new(48_000.0),
        );
        assert!(osc.ports[0].level.read().0 > 0.0);

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
        p.install(second.clone());
        assert_eq!(reported_paths(&mut p), vec![names(&["/out"])]);
        assert_eq!(on_addresses(&second), vec!["/out"]);

        // Dropped, not parked: a later Plan with `/osc` again does not bring it back.
        let third = hosting(true);
        p.install(third.clone());
        assert_eq!(reported_paths(&mut p), vec![names(&["/out"])]);
        assert_eq!(on_addresses(&third), vec!["/out"]);
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
    fn a_set_renews_the_lease() {
        let slots = hosting(true);
        let mut p = probes_on(&slots);
        let now = Instant::now();
        p.set(vec![names(&["/osc"])], now);
        p.set(vec![names(&["/osc"])], now + Duration::from_secs(4));
        assert_eq!(p.report(now + Duration::from_secs(8)).len(), 1);
    }
}
