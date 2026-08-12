//! The passive, OS-free [`Coordinator`]: the single writer of graph structure.
//!
//! It owns the [`Registry`] handle, the resolver, the installed-Plan [`Manifest`], the canonical
//! [`NormalizedDoc`], and the Coordinator-side mailbox endpoint. [`Coordinator::swap_document`]
//! validates and builds a whole new Engine off-thread, diffs the migration table, fills the install
//! mailbox with an [`InstallBundle`], and returns a [`SwapReport`]; the same swap splits at the
//! build for a caller that must inspect what was built before carrying it
//! ([`Coordinator::prepare_document`] → [`PreparedSwap`] → [`Coordinator::commit_swap`]).
//! [`Coordinator::reclaim`] drops
//! the retired bundle off-thread. "Off-thread" is a property of the *caller* — this module is a
//! plain OS-free function with no clock, threads, or I/O — and single-writer discipline is enforced
//! by `&mut self`.
//!
//! see rules: execution-runtime

use crate::contract::{content_hash, Diag, Report, SwapReport};
use crate::engine::FromDocumentError;
use crate::format::{load_instrument_doc, LoadWarning, NormalizedDoc};
use crate::resources::ResourceResolver;
use reuben_core::config::AudioConfig;
use reuben_core::coordinator::{InstallBundle, RenderSide};
use reuben_core::plan::Plan;
use reuben_core::registry::Registry;
use reuben_core::Engine;

use super::manifest::{build_manifest, Manifest};
use reuben_core::coordinator::mailbox::{swap_pair, CoordinatorMailbox, ReclaimError};

/// A validated, fully built swap that has **not** been handed to the render side: everything
/// [`Coordinator::swap_document`] does *except* filling the install mailbox, stopped one step short
/// so a caller can read what was built and refuse it.
///
/// The case this exists for is a door whose render buffers are a fixed size. Its geometry check can
/// only run against the built Engine, and a single-call swap returns with that Engine already in
/// the mailbox — the door is handed a fait accompli it cannot carry, and its only remaining move is
/// to make its render path total and report the engine unplayable. Reading
/// [`channels`](Self::channels) / [`input_channels`](Self::input_channels) here happens before
/// anything crosses the RT boundary.
///
/// [`Coordinator::commit_swap`] posts a prepared swap; **dropping it declines it**, an ordinary
/// off-thread free like every other Coordinator-side drop. Nothing was published, so there is
/// nothing to reclaim and the install slot never closed.
///
/// It deliberately carries no [`MigrationTable`](reuben_core::coordinator::MigrationTable): that table pairs Plan indices against the Engine
/// this swap will displace, which is known at commit and not before. see rules: execution-runtime
///
/// **RT-safety requirement (drop off-thread), same as the [`InstallBundle`] it becomes.** Dropping
/// one frees a whole Engine — a heap free the audio thread may never do. Nothing in this API can
/// carry it there ([`Coordinator::commit_swap`] takes `&mut Coordinator`, which the render side
/// does not have), so this is a property of the type rather than a hazard on any path here; it is
/// stated because `PreparedSwap` is `Send` and a host is free to move one between its own threads.
pub struct PreparedSwap {
    /// The document that becomes canonical on commit.
    doc: NormalizedDoc,
    /// The built Engine's survivor identities, diffed against the installed manifest at commit.
    manifest: Manifest,
    /// The freshly built Engine — the same vessel the mailbox would carry.
    engine: Engine,
    /// The load's non-fatal resource problems, promoted into the commit report's warnings.
    warnings: Vec<LoadWarning>,
    /// Computed at prepare so the caller can read it before deciding; the commit report carries
    /// this same string.
    content_hash: String,
}

impl PreparedSwap {
    /// The built Engine's logical output channel count ([`Engine::channels`]) — the geometry a door
    /// with fixed-size render buffers weighs before committing.
    pub fn channels(&self) -> usize {
        self.engine.channels()
    }

    /// The built Engine's logical input channel count ([`Engine::input_channels`]) — the dual, for
    /// a door that must also supply the input master.
    pub fn input_channels(&self) -> usize {
        self.engine.input_channels()
    }

    /// The core block size the built Engine renders in.
    pub fn block_size(&self) -> usize {
        self.engine.block_size()
    }

    /// The sample rate the built Engine's Plan was instantiated for.
    pub fn sample_rate(&self) -> f32 {
        self.engine.sample_rate()
    }

    /// The content hash the installed document advances to if this swap commits — the same token
    /// the commit report carries, readable before the decision.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// The load's non-fatal resource problems. Advisory (they never make a swap unbuildable), but a
    /// door that surfaces them may want to weigh them in its decision, not only report them after.
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }
}

/// The passive Coordinator. Single-writer by `&mut self`; OS-free.
pub struct Coordinator {
    registry: Registry,
    resolver: Box<dyn ResourceResolver + Send>,
    config: AudioConfig,
    /// The canonical installed document: the source of truth for what is playing,
    /// and the basis of the content hash a swap's `expect` guard compares.
    doc: NormalizedDoc,
    /// The installed Plan's survivor identities, diffed against each swap's new Plan.
    manifest: Manifest,
    /// The Coordinator-side install/retire mailbox endpoint.
    mailbox: CoordinatorMailbox<InstallBundle>,
    /// The currently-installed engine's logical output channel count ([`Engine::channels`]). The
    /// engine itself has crossed into the mailbox, but the native shell still needs this logical
    /// geometry to rebuild the device output map off-thread against the retained device channel
    /// count. A *logical* count — not the device's — so recording it here keeps the
    /// Coordinator device-free.
    installed_channels: usize,
    /// The currently-installed engine's logical input channel count ([`Engine::input_channels`]).
    /// The native shell reads it to decide the input dark-degrade warning: an engine
    /// that binds input channels no open stream provides degrades to silence with a loud warning.
    installed_input_channels: usize,
}

impl Coordinator {
    /// Build the initial Engine from `doc_json`, take its manifest, and return the Coordinator
    /// alongside the [`RenderSide`] (initial Engine + render mailbox) for the shell's callback and
    /// the load warnings (resource problems are non-fatal, but the shell must surface
    /// them). The initial Engine is installed *directly* into the callback — it does not cross the
    /// mailbox — so the first swap is what first fills the install slot.
    pub fn install_initial(
        doc_json: &str,
        registry: Registry,
        resolver: Box<dyn ResourceResolver + Send>,
        config: AudioConfig,
    ) -> Result<(Coordinator, RenderSide, Vec<LoadWarning>), FromDocumentError> {
        let doc = NormalizedDoc::from_json(doc_json, &registry, Some(&*resolver))
            .map_err(FromDocumentError::Load)?;
        let (engine, manifest, warnings) = build_engine(&doc, &registry, &*resolver, config)?;
        // Record the initial engine's logical geometry before it moves into the RenderSide:
        // the native shell reads it to build the first device output map.
        let installed_channels = engine.channels();
        let installed_input_channels = engine.input_channels();
        let (mailbox, render_mailbox) = swap_pair::<InstallBundle>();
        let coordinator = Coordinator {
            registry,
            resolver,
            config,
            doc,
            manifest,
            mailbox,
            installed_channels,
            installed_input_channels,
        };
        let render_side = RenderSide {
            engine,
            mailbox: render_mailbox,
        };
        Ok((coordinator, render_side, warnings))
    }

    /// The content hash of the currently installed document — the token a later
    /// swap's `expect` guard compares, and what every report names as "what is playing".
    pub fn installed_hash(&self) -> String {
        content_hash(&self.doc)
    }

    /// The canonical installed document: a fresh conversation reads what is playing
    /// from here.
    pub fn document(&self) -> &NormalizedDoc {
        &self.doc
    }

    /// The currently-installed engine's logical output channel count ([`Engine::channels`]). The
    /// native shell reads it to rebuild the device output map off-thread against the retained device
    /// channel count after a swap — a *logical* count, so the Coordinator stays
    /// device-free.
    pub fn installed_channels(&self) -> usize {
        self.installed_channels
    }

    /// The currently-installed engine's logical input channel count ([`Engine::input_channels`]).
    /// The native shell reads it to raise the input dark-degrade warning when a
    /// swapped-in engine binds input channels no open stream provides.
    pub fn installed_input_channels(&self) -> usize {
        self.installed_input_channels
    }

    /// Validate + build a whole new Engine off-thread, precompute the migration table, fill the
    /// install mailbox, and return a real [`SwapReport`]: the whole swap in one call, for a caller
    /// that will carry whatever builds.
    ///
    /// A caller that might *not* — a door whose render buffers are a fixed size — takes the two
    /// halves instead: [`prepare_document`](Self::prepare_document) builds and stops, and
    /// [`commit_swap`](Self::commit_swap) posts what it built. This form is exactly those two,
    /// back to back.
    ///
    /// `source` is the document JSON (by-path resolution is a shell concern — the resolver seam —
    /// kept out of this OS-free core). A load/instantiate error installs nothing. On success the
    /// report carries the real survivor/reset [`DiffSummary`](crate::DiffSummary) and the now-installed hash, and the
    /// canonical document + manifest advance.
    ///
    /// **Arbitration here is last-write-wins, and this signature takes no `expect` guard** — the
    /// optimistic-concurrency guard is a *door* concern (see rules: agent-mcp). A door with
    /// concurrent clients compares the hash its client holds against
    /// [`installed_hash`](Coordinator::installed_hash) and answers in its own shape before calling
    /// in. That compare is the whole guard — not logic worth centralizing — and the doors that
    /// need it do not all arrive through here (the web lane runs a restart-swap with no
    /// Coordinator at all). Do not re-add the parameter: it would be a second implementation of
    /// one decision, reachable only from its own test.
    pub fn swap_document(&mut self, source: &str) -> SwapReport {
        // Reclaim BEFORE the build, not only inside the commit. `commit_swap` reclaims too, but by
        // the time it runs the freshly built Engine is already alive — so reclaiming only there
        // would raise this call's peak to live + retiree + new, where it has always been live +
        // new. That extra Engine is a real cost to a wasm door under a memory cap. Doing it here
        // keeps the one-call form byte-for-byte the swap it was; the second call is then a
        // no-op load on an empty slot.
        self.mailbox.try_reclaim();
        match self.prepare_document(source) {
            Ok(prepared) => self.commit_swap(prepared),
            Err(rejected) => rejected,
        }
    }

    /// Validate + build a whole new Engine off-thread and **stop there**: the first half of
    /// [`swap_document`](Self::swap_document), returning a [`PreparedSwap`] the caller inspects and
    /// then either commits ([`commit_swap`](Self::commit_swap)) or drops. Nothing crosses the RT
    /// boundary, the install slot does not close, and the installed document does not move — so a
    /// door that cannot carry what was built is never handed it.
    ///
    /// `&self`, not `&mut self`: preparing changes nothing here. Single-writer discipline still
    /// binds where it matters — only the commit mutates.
    ///
    /// `Err` is the same rejection report the single-call form returns for a document that will not
    /// load or will not plan: `ok: false`, the diagnostics, and the hash of what keeps playing.
    // `result_large_err`: the lint's remedy buys nothing here, because it measures the wrong
    // variant. Sizes as built: `SwapReport` 160 B, `PreparedSwap` 888 B, and the `Result` 888 B —
    // the `Ok` variant sets it, so boxing the `Err` leaves the returned Result at 888 B exactly.
    // What boxing would add is an allocation and a deref on every rejection, which is the ordinary
    // outcome of an agent's first draft, for a report the caller already handles by value
    // everywhere else (`swap_document` returns this same type unboxed).
    #[allow(clippy::result_large_err)]
    pub fn prepare_document(&self, source: &str) -> Result<PreparedSwap, SwapReport> {
        // Parse + normalize (the loader is the single validation authority).
        let doc = match NormalizedDoc::from_json(source, &self.registry, Some(&*self.resolver)) {
            Ok(d) => d,
            Err(e) => return Err(self.reject(vec![Diag::from_load(&e)])),
        };

        // Build the whole new Engine + its manifest off-thread.
        let (engine, manifest, warnings) =
            match build_engine(&doc, &self.registry, &*self.resolver, self.config) {
                Ok(built) => built,
                Err(FromDocumentError::Load(e)) => {
                    return Err(self.reject(vec![Diag::from_load(&e)]))
                }
                Err(FromDocumentError::Plan(e)) => {
                    return Err(self.reject(vec![Diag {
                        node: None,
                        port: None,
                        message: format!("instantiate plan: {e:?}"),
                    }]))
                }
            };

        let content_hash = content_hash(&doc);
        Ok(PreparedSwap {
            doc,
            manifest,
            engine,
            warnings,
            content_hash,
        })
    }

    /// Commit a [`PreparedSwap`]: diff it against what is *actually* installed, fill the install
    /// mailbox, advance the canonical document + manifest, and return the real [`SwapReport`] — the
    /// second half of [`swap_document`](Self::swap_document).
    ///
    /// Refused, with nothing installed, in two cases: while a previous swap is still in flight
    /// (exactly as the single-call form is), and when `prepared` was built against a different
    /// [`AudioConfig`] than this Coordinator runs. The prepared swap is consumed either way, so a
    /// caller for whom the build was expensive calls [`try_reclaim`](Self::try_reclaim) first —
    /// that is what opens the slot, and with it open the in-flight refusal is unreachable.
    ///
    /// Committing a swap prepared by a *different* Coordinator at the *same* config is sound and
    /// deliberately allowed: the Engine is self-consistent by then, and the migration table is
    /// diffed here against whatever this Coordinator has installed.
    pub fn commit_swap(&mut self, prepared: PreparedSwap) -> SwapReport {
        // A PreparedSwap is an Engine already instantiated against ONE AudioConfig, and the config
        // is not recoverable from the document — so committing one into a Coordinator that runs a
        // different rate or block size installs a Plan rendering at the wrong speed (pitch and
        // tempo), under a declick ramp the slot sized for a rate the Engine does not run at. The
        // single-call form could not express that, because it always built with `self.config`; the
        // split can, so the split has to refuse it. Checked before anything is reclaimed or
        // installed: a refusal must leave the mailbox exactly as it found it.
        if prepared.sample_rate() != self.config.sample_rate
            || prepared.block_size() != self.config.block_size
        {
            return self.reject(vec![Diag {
                node: None,
                port: None,
                message: format!(
                    "prepared against {} Hz / {}-frame blocks, but this Coordinator runs \
                     {} Hz / {}-frame blocks — prepare and commit through Coordinators that \
                     share an audio configuration",
                    prepared.sample_rate(),
                    prepared.block_size(),
                    self.config.sample_rate,
                    self.config.block_size,
                ),
            }]);
        }

        // Opportunistically clear a previous swap whose retiree has come home, so a caller that
        // drove the render side between swaps can install the next one without a separate reclaim.
        self.mailbox.try_reclaim();

        let PreparedSwap {
            doc,
            manifest,
            engine,
            warnings,
            content_hash,
        } = prepared;

        // The new engine's logical geometry, captured before it is boxed into the mailbox — the
        // native shell reads it back (via the installed-geometry accessors) to rebuild the device
        // output map off-thread and to compute the input dark-degrade warning.
        let new_channels = engine.channels();
        let new_input_channels = engine.input_channels();

        // Precompute the migration table + diff (the edit always wins). This is the commit's work,
        // not the build's: the table pairs Plan indices against the Engine this swap displaces, so
        // a prepared swap held across another install still transplants against what it retires.
        let (migration, diff) = self.manifest.diff(&manifest);

        // Fill the install mailbox. One swap in flight: if the render side has not yet drained
        // the previous swap, the retiree is not home, `install` is refused, and nothing changes —
        // report it honestly, still-playing hash intact, so the caller retries after reclaim.
        let bundle = Box::new(InstallBundle { engine, migration });
        if self.mailbox.install(bundle).is_err() {
            return self.reject(vec![Diag {
                node: None,
                port: None,
                message: "previous swap is still in flight — the render side has not drained it; \
                          reclaim its retiree and retry"
                    .to_string(),
            }]);
        }

        // Committed (last-write-wins): the canonical document + manifest are now the new ones. The
        // render side is eventually-consistent — it applies the transplant at its next callback
        // top — but the Coordinator is the authority for "what is installed" from here on.
        self.doc = doc;
        self.manifest = manifest;
        self.installed_channels = new_channels;
        self.installed_input_channels = new_input_channels;

        SwapReport {
            report: Report {
                ok: true,
                errors: Vec::new(),
                warnings: warnings.iter().map(Diag::from_warning).collect(),
            },
            content_hash,
            diff: Some(diff),
        }
    }

    /// Non-blocking reclaim of the retired [`InstallBundle`] (deferred free): take it back
    /// so the caller can drop it off the audio thread, and open the slot for the next swap. `None`
    /// if the render side has not posted the retiree yet.
    pub fn try_reclaim(&mut self) -> Option<Box<InstallBundle>> {
        self.mailbox.try_reclaim()
    }

    /// Blocking reclaim: poll until the retiree returns or `timed_out` fires. The
    /// caller supplies the clock (core is OS-free) — see [`CoordinatorMailbox::reclaim`]. Dropping
    /// the returned bundle is the off-thread free; it opens the slot for the next swap.
    pub fn reclaim(
        &mut self,
        timed_out: impl FnMut() -> bool,
    ) -> Result<Box<InstallBundle>, ReclaimError> {
        self.mailbox.reclaim(timed_out)
    }

    /// A rejected swap installs nothing: `ok: false`, the given diagnostics, no
    /// diff, and the still-installed document's hash — the report names what keeps playing.
    fn reject(&self, errors: Vec<Diag>) -> SwapReport {
        SwapReport {
            report: Report {
                ok: false,
                errors,
                warnings: Vec::new(),
            },
            content_hash: self.installed_hash(),
            diff: None,
        }
    }
}

/// Build an Engine and its manifest from a normalized document — the off-thread Instantiate shared
/// by initial install and every swap. Re-uses the already-minted [`NormalizedDoc`] (no re-parse),
/// then instantiates and takes the manifest before wrapping the Plan in an Engine.
fn build_engine(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    config: AudioConfig,
) -> Result<(Engine, Manifest, Vec<LoadWarning>), FromDocumentError> {
    let loaded = load_instrument_doc(doc, registry, resolver).map_err(FromDocumentError::Load)?;
    let plan = Plan::instantiate(loaded.graph, config).map_err(FromDocumentError::Plan)?;
    let manifest = build_manifest(doc, &plan, registry, resolver);
    Ok((Engine::new(plan), manifest, loaded.warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::MemoryResolver;
    use reuben_core::coordinator::{MigrationTable, RenderMailbox};
    use reuben_core::message::Arg;
    use reuben_core::resources::SampleBuffer;
    use std::sync::{Arc, Mutex};

    const DEFAULT_VOICE_JSON: &str =
        include_str!("../../../../instruments/voices/default-voice.json");

    fn cfg() -> AudioConfig {
        AudioConfig::new(48_000.0, 128)
    }

    /// A **test-only** render slot standing in for the production RT install slot
    /// ([`super::reuben_core::coordinator::slot::RenderSlot`]). It owns the live Engine + the render-side mailbox and,
    /// at each `poll_install`, drains a pending swap and applies the **same migration-table
    /// semantics** the RT slot will: box-transplant the survivors, install the new Engine, post the
    /// retiree. It applies them *synchronously* (no callback, no atomics timing) purely so a
    /// Coordinator-driven swap's survivor-vs-reset behavior can be OBSERVED in rendered audio. It is
    /// NOT the RT path.
    struct RenderRig {
        engine: Engine,
        mailbox: RenderMailbox<InstallBundle>,
    }

    impl RenderRig {
        fn new(side: RenderSide) -> Self {
            Self {
                engine: side.engine,
                mailbox: side.mailbox,
            }
        }

        /// Whether anything is waiting in the install slot — the render side's own view of
        /// "have I been handed a new Engine yet", which a declined swap must never move.
        fn has_install(&self) -> bool {
            self.mailbox.has_install()
        }

        /// The callback-top install step, synchronous test form.
        fn poll_install(&mut self) {
            if let Some(bundle) = self.mailbox.take_install() {
                let mut bundle = bundle;
                bundle
                    .engine
                    .transplant_survivors(&mut self.engine, bundle.migration.survivors());
                let retiring = std::mem::replace(&mut self.engine, bundle.engine);
                let _ = self.mailbox.post_retiree(Box::new(InstallBundle {
                    engine: retiring,
                    migration: MigrationTable::empty(),
                }));
            }
        }

        fn queue_osc(&mut self, address: &str, args: &[Arg]) {
            self.engine.queue_osc(address, args);
        }

        /// Render `frames` and return the peak absolute sample — the rendered behavior the survivor
        /// tests observe.
        fn render_peak(&mut self, frames: usize) -> f32 {
            let ch = self.engine.channels();
            let mut buf = vec![0.0f32; frames * ch];
            self.engine.fill(&mut buf);
            buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
        }
    }

    /// An envelope (gate held, slow attack) whose CV is the master output — so the rendered peak
    /// *is* the envelope level. `env_addr` lets a test rename the node to force a reset.
    fn envelope_doc(env_addr: &str) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "eg",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "envelope", "address": "{env_addr}",
                      "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                                   "sustain": 0.8, "release": 0.5 }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
        )
    }

    /// The same `/env` envelope as [`envelope_doc`], with only `attack` — a runtime `inputs` param
    /// (never part of the instantiate-time fingerprint) — as what a caller varies.
    fn envelope_doc_attack(attack: f32) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "eg",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "envelope", "address": "/env",
                      "inputs": {{ "gate": 1.0, "attack": {attack}, "decay": 0.01,
                                   "sustain": 0.8, "release": 0.5 }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "/env.cv" }} }} }} ] }}"#
        )
    }

    #[test]
    fn a_survivor_keeps_state_a_reset_starts_fresh() {
        // Survivor identity is address + type + fingerprint (see rules: execution-runtime); a
        // rename is a remove+add, not a survivor. Warm the envelope to sustain, then swap.
        let base = envelope_doc("/env");

        let survived = {
            let (mut coord, side, _w) = Coordinator::install_initial(
                &base,
                Registry::builtin(),
                Box::new(MemoryResolver::new()),
                cfg(),
            )
            .expect("initial install");
            let mut rig = RenderRig::new(side);
            rig.render_peak(48_000); // ~1s: past attack+decay, sitting at sustain
            let report = coord.swap_document(&base);
            assert!(report.report.ok, "swap should succeed: {:?}", report.report);
            assert_eq!(
                report.diff.as_ref().unwrap().survived,
                2,
                "both nodes survive"
            );
            rig.poll_install();
            coord.try_reclaim();
            rig.render_peak(128)
        };

        let reset = {
            let (mut coord, side, _w) = Coordinator::install_initial(
                &base,
                Registry::builtin(),
                Box::new(MemoryResolver::new()),
                cfg(),
            )
            .expect("initial install");
            let mut rig = RenderRig::new(side);
            rig.render_peak(48_000);
            let report = coord.swap_document(&envelope_doc("/eg"));
            assert!(report.report.ok, "swap should succeed: {:?}", report.report);
            // `/env` removed, `/eg` added, `/out` survives.
            let diff = report.diff.as_ref().unwrap();
            assert_eq!(diff.survived, 1, "only /out survives a rename");
            rig.poll_install();
            coord.try_reclaim();
            rig.render_peak(128)
        };

        assert!(
            survived > 0.6,
            "a survivor envelope keeps ringing at sustain: peak {survived}"
        );
        assert!(
            reset < 0.1,
            "a renamed (reset) envelope restarts from zero: peak {reset}"
        );
    }

    #[test]
    fn a_changed_runtime_param_leaves_the_survivor_ringing() {
        // A runtime `inputs` param is never part of the survivor key (see rules:
        // execution-runtime), so editing only `attack` here must leave the node a survivor — the
        // counterpart to `a_survivor_keeps_state_a_reset_starts_fresh`, where it must NOT reset.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc_attack(0.5),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);
        rig.render_peak(48_000); // ~1s: past attack+decay, sitting at sustain

        let report = coord.swap_document(&envelope_doc_attack(0.05));
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        // Only a runtime param moved: both nodes survive (neither fingerprint changed).
        assert_eq!(
            report.diff.as_ref().unwrap().survived,
            2,
            "a runtime param edit resets nothing"
        );
        rig.poll_install();
        coord.try_reclaim();
        let ringing = rig.render_peak(128);

        assert!(
            ringing > 0.6,
            "a survivor keeps ringing at sustain across a runtime param edit: peak {ringing}"
        );
    }

    // ---- Voicer: a changed config constant (`voices`) resets the hosted pool ----

    fn voice_resolver() -> MemoryResolver {
        let mut r = MemoryResolver::new();
        r.insert_text("voices/default-voice.json", DEFAULT_VOICE_JSON);
        r
    }

    fn voicer_doc(voices: u32) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "top",
                 "resources": {{ "dv": "voices/default-voice.json" }},
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "voicer", "address": "/voicer", "config": {{ "voices": {voices} }},
                      "voice": "dv" }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "/voicer.audio" }} }} }} ] }}"#
        )
    }

    /// Warm a held note into a 4-voice voicer, then swap to `swap_to` voices and observe the peak.
    fn voicer_peak_after_swap(swap_to: u32) -> f32 {
        let (mut coord, side, _w) = Coordinator::install_initial(
            &voicer_doc(4),
            Registry::builtin(),
            Box::new(voice_resolver()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);
        // Note-on (midi 69, gate 1), held — a voice rings at sustain.
        rig.queue_osc("/voicer/notes", &[Arg::F32(69.0), Arg::F32(1.0)]);
        rig.render_peak(24_000); // ~0.5s: the voice's envelope reaches sustain
        let report = coord.swap_document(&voicer_doc(swap_to));
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        rig.poll_install();
        coord.try_reclaim();
        rig.render_peak(2_048)
    }

    #[test]
    fn bumping_voices_resets_the_voicer_unchanged_survives() {
        // `voices` is an instantiate-time Constant, part of the survivor fingerprint (see rules:
        // execution-runtime): bumping it 4→8 is a different instantiation, so the voicer resets to
        // a fresh, silent pool rather than surviving.
        let ringing = voicer_peak_after_swap(4);
        let reset = voicer_peak_after_swap(8);
        assert!(
            ringing > 0.02,
            "an unchanged voicer keeps its held note ringing: peak {ringing}"
        );
        assert!(
            reset < ringing * 0.25,
            "bumping voices resets the voicer to a fresh, silent pool: peak {reset} vs {ringing}"
        );
    }

    // ---- Sample player: re-resolving a same-path sample to different bytes resets it ----

    /// A resolver whose one sample's bytes can be flipped between installs, at the same path — the
    /// re-upload-same-path flow (MCP/F). Interior-mutable so a test can change the
    /// resolved bytes without the document changing at all.
    #[derive(Clone)]
    struct FlipResolver {
        buffer: Arc<Mutex<SampleBuffer>>,
    }

    impl ResourceResolver for FlipResolver {
        fn resolve(&self, _source: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
            Ok(self.buffer.lock().unwrap().clone())
        }
    }

    const SAMPLE_DOC: &str = r#"{ "format_version": 3, "instrument": "samp",
        "resources": { "kick": "kick.wav" },
        "interface": { "outputs": { "out": { "from": "/samp.audio" } } },
        "nodes": [
          { "type": "sample", "address": "/samp", "sample": "kick" },
          { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/samp.audio" } } } ] }"#;

    /// A long constant-level mono sample, so a triggered one-shot is still playing across the swap.
    fn constant_sample(level: f32) -> SampleBuffer {
        SampleBuffer::new(vec![vec![level; 48_000]], 48_000.0)
    }

    /// Trigger a one-shot, optionally re-upload different bytes at the same path, swap, observe.
    fn sample_peak_after_swap(reupload_different_bytes: bool) -> f32 {
        let buffer = Arc::new(Mutex::new(constant_sample(0.5)));
        let resolver = FlipResolver {
            buffer: Arc::clone(&buffer),
        };
        let (mut coord, side, _w) = Coordinator::install_initial(
            SAMPLE_DOC,
            Registry::builtin(),
            Box::new(resolver),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);
        // Rising gate edge fires the one-shot; it plays for ~1s (48k frames at rate 1).
        rig.queue_osc("/samp/gate", &[Arg::F32(1.0)]);
        rig.render_peak(4_800); // ~0.1s in: the one-shot is playing
        if reupload_different_bytes {
            // Same path, different content — the document is byte-identical; only the bytes change.
            *buffer.lock().unwrap() = constant_sample(0.25);
        }
        let report = coord.swap_document(SAMPLE_DOC);
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        rig.poll_install();
        coord.try_reclaim();
        rig.render_peak(2_048)
    }

    #[test]
    fn reresolving_a_sample_to_different_bytes_resets_the_player() {
        // A sample's survivor identity is its decoded bytes, not its path (see rules:
        // execution-runtime): re-uploading different bytes at the same path is a different
        // instantiation, so the player resets rather than surviving.
        let same_bytes = sample_peak_after_swap(false);
        let changed_bytes = sample_peak_after_swap(true);
        assert!(
            same_bytes > 0.4,
            "an unchanged sample keeps its one-shot playing: peak {same_bytes}"
        );
        assert!(
            changed_bytes < 0.05,
            "re-uploaded bytes reset the player to silence: peak {changed_bytes}"
        );
    }

    // ---- Installed hash + geometry, and reclaim ----

    #[test]
    fn installed_hash_advances_on_install_and_is_retained_by_a_rejected_swap() {
        // `installed_hash` is the token every door hands its clients, and the one a door with
        // concurrent clients compares its `expect` against (see rules: agent-mcp — the guard is
        // the door's, not this method's). What core owes them: the hash advances when a document
        // installs, and a rejected swap neither advances it nor lies about it — the report names
        // what *keeps playing*, so a client that re-reads gets the doc it can actually hear.
        let (mut coord, _side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let installed = coord.installed_hash();

        // Rejected (the loader refuses it): nothing installs, the hash stays put, and the report
        // names it.
        let rejected = coord.swap_document("{ not json");
        assert!(!rejected.report.ok, "a bad document is rejected");
        assert!(rejected.diff.is_none(), "a rejected swap installs nothing");
        assert_eq!(
            rejected.content_hash, installed,
            "the report names what keeps playing"
        );
        assert_eq!(
            coord.installed_hash(),
            installed,
            "the installed document is unchanged"
        );

        // An outside witness: the target's hash computed through the same public path a client
        // walks (normalize, then hash), never through the Coordinator. Asserting the report
        // against `installed_hash()` alone would compare two spellings of one expression and pass
        // even if the swap committed the *wrong* document; this catches that.
        let target = envelope_doc("/eg");
        let resolver = MemoryResolver::new();
        let witness = content_hash(
            &NormalizedDoc::from_json(&target, &Registry::builtin(), Some(&resolver))
                .expect("the target document normalizes"),
        );

        // Installed: the hash advances to the document that actually went in, and the report and
        // the accessor both name it.
        let ok = coord.swap_document(&target);
        assert!(ok.report.ok, "a valid document installs: {:?}", ok.report);
        assert_ne!(
            coord.installed_hash(),
            installed,
            "the swap advanced the doc"
        );
        assert_eq!(
            ok.content_hash, witness,
            "the report names the document it installed"
        );
        assert_eq!(
            coord.installed_hash(),
            witness,
            "the accessor names the document it installed"
        );
    }

    /// A minimal instrument that binds logical input channel 0: `input_channels`
    /// becomes 1. No resources, so a bare [`MemoryResolver`] loads it. Used to prove the
    /// installed-geometry accessors advance across a swap.
    const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
        "interface": {
            "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "out": { "from": "/mic" } } },
        "nodes": [] }"#;

    #[test]
    fn installed_channels_reflect_the_installed_engine_geometry() {
        // The native shell reads these to build the device output map off-thread and
        // to compute the input dark-degrade warning: both need the *currently
        // installed* engine's logical channel geometry, which the Coordinator holds even though the
        // engine itself has crossed into the mailbox.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        // The accessors match the engine the same install produced (independent source of truth: the
        // engine's own accessors), and the base rig binds no input.
        assert_eq!(coord.installed_channels(), side.engine.channels());
        assert_eq!(
            coord.installed_input_channels(),
            side.engine.input_channels()
        );
        assert_eq!(
            coord.installed_input_channels(),
            0,
            "the base rig binds no input"
        );

        // A successful swap to an input-binding document advances the reported geometry.
        let report = coord.swap_document(MIC_PASSTHRU);
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        assert_eq!(
            coord.installed_input_channels(),
            1,
            "the swapped-in rig binds logical input channel 0"
        );

        // A rejected swap leaves the geometry unchanged (it names what keeps playing).
        let rejected = coord.swap_document("{ not json");
        assert!(!rejected.report.ok);
        assert_eq!(
            coord.installed_input_channels(),
            1,
            "a rejected swap does not advance the installed geometry"
        );
    }

    // ---- Build-then-decide: prepare, inspect, commit or decline ----

    /// [`envelope_doc`]'s rig widened to four logical master channels (the interface taps channel
    /// 3 as well as channel 0). A door with fixed-size render buffers is exactly the caller that
    /// has to see this width *before* the render side is handed the Engine carrying it.
    const WIDE_ENVELOPE: &str = r#"{ "format_version": 3, "instrument": "eg",
        "interface": { "outputs": {
            "front": { "from": "/out.audio", "channel": 0 },
            "rear":  { "from": "/out.audio", "channel": 3 } } },
        "nodes": [
          { "type": "envelope", "address": "/env",
             "inputs": { "gate": 1.0, "attack": 0.5, "decay": 0.01,
                         "sustain": 0.8, "release": 0.5 } },
          { "type": "output", "address": "/out",
             "inputs": { "audio": { "from": "/env.cv" } } } ] }"#;

    #[test]
    fn a_door_reads_the_built_geometry_and_declines_before_anything_crosses() {
        // The property the split exists for: a door with fixed-size render buffers must be able to
        // refuse an Engine it cannot carry, and refusing must leave the render side untouched.
        // `swap_document` cannot offer that — by the time it returns, the mailbox is already full.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let rig = RenderRig::new(side);
        let installed = coord.installed_hash();

        // Build it, then look at what was built — this is the moment that did not exist before.
        let prepared = coord.prepare_document(WIDE_ENVELOPE).expect("it builds");
        assert_eq!(
            prepared.channels(),
            4,
            "the door reads the built Engine's logical width"
        );
        assert_eq!(prepared.input_channels(), 0);
        assert!(
            !rig.has_install(),
            "preparing a swap hands the render side nothing"
        );

        // The door declines: dropping the prepared swap is the whole gesture, and it is an
        // ordinary off-thread free.
        drop(prepared);
        assert!(
            !rig.has_install(),
            "a declined swap never reaches the render side"
        );
        assert!(
            coord.try_reclaim().is_none(),
            "nothing was published, so there is no retiree to reclaim"
        );
        assert_eq!(
            coord.installed_hash(),
            installed,
            "a declined swap does not advance the installed document"
        );
        assert_eq!(coord.installed_channels(), 2, "nor the installed geometry");

        // And the slot is still free: the door can install something it *can* carry.
        let narrow = coord.swap_document(&envelope_doc("/eg"));
        assert!(
            narrow.report.ok,
            "declining did not wedge the swap slot: {:?}",
            narrow.report
        );
        assert!(rig.has_install(), "the accepted swap did cross");
    }

    #[test]
    fn a_committed_swap_installs_exactly_what_it_advertised() {
        // The other half: what `prepare_document` reported is what `commit_swap` installs, and the
        // report it returns is the same shape `swap_document` returns.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let rig = RenderRig::new(side);

        let prepared = coord.prepare_document(WIDE_ENVELOPE).expect("it builds");
        let advertised_hash = prepared.content_hash().to_string();
        let advertised_channels = prepared.channels();

        let report = coord.commit_swap(prepared);
        assert!(report.report.ok, "commit installs: {:?}", report.report);
        assert_eq!(
            report.content_hash, advertised_hash,
            "the committed report names the document prepare advertised"
        );
        assert_eq!(coord.installed_hash(), advertised_hash);
        assert_eq!(coord.installed_channels(), advertised_channels);
        assert!(rig.has_install(), "the committed swap crossed");
    }

    #[test]
    fn a_prepared_swap_diffs_against_what_is_installed_when_it_commits() {
        // A prepared swap carries no migration table: the table pairs Plan indices against the
        // Engine it will displace, which is only known at commit. Prepare against `/env`, install
        // a *rename* underneath it, then commit — the diff must describe the rename it actually
        // displaces (only `/out` survives), not the `/env` document it was prepared against (where
        // both nodes would have survived).
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);

        let prepared = coord
            .prepare_document(&envelope_doc("/env"))
            .expect("builds");

        // Something else installs first, and the render side drains it so the slot re-opens.
        let renamed = coord.swap_document(&envelope_doc("/eg"));
        assert!(renamed.report.ok, "{:?}", renamed.report);
        rig.poll_install();
        coord.try_reclaim();

        let report = coord.commit_swap(prepared);
        assert!(report.report.ok, "commit installs: {:?}", report.report);
        let diff = report
            .diff
            .as_ref()
            .expect("a committed swap carries a diff");
        assert_eq!(
            diff.survived, 1,
            "only /out survives what this swap actually displaces"
        );
        assert_eq!(diff.added, vec!["/env".to_string()]);
        assert_eq!(diff.removed, vec!["/eg".to_string()]);
    }

    #[test]
    fn a_swap_prepared_against_a_different_audio_config_is_refused() {
        // The one thing the split can express that the single call could not get wrong: a
        // `PreparedSwap` is an Engine already instantiated against ONE AudioConfig, and
        // `swap_document` always built with its own. The door this exists for already runs two
        // Coordinators (a discovery context beside the live one), so "prepare there, commit here"
        // is the first thing a caller tries — and committing it would install a Plan rendering at
        // the wrong rate (pitch and tempo) under a declick ramp sized for a rate the Engine does
        // not run at. It must be refused, with nothing installed and nothing crossed.
        let (discovery, _side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            AudioConfig::new(48_000.0, 64),
        )
        .expect("initial install");
        let (mut live, live_side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            AudioConfig::new(96_000.0, 128),
        )
        .expect("initial install");
        let rig = RenderRig::new(live_side);
        let installed = live.installed_hash();

        let prepared = discovery
            .prepare_document(&envelope_doc("/eg"))
            .expect("it builds on the discovery context");
        assert_eq!(prepared.sample_rate(), 48_000.0, "built at the other rate");
        assert_eq!(prepared.block_size(), 64, "and the other block size");

        let refused = live.commit_swap(prepared);
        assert!(!refused.report.ok, "a foreign audio config is refused");
        assert!(
            refused
                .report
                .errors
                .iter()
                .any(|d| d.message.contains("48000") && d.message.contains("96000")),
            "the diagnostic names both configs: {:?}",
            refused.report.errors
        );
        assert!(refused.diff.is_none(), "a refused swap installs nothing");
        assert_eq!(
            refused.content_hash, installed,
            "the report names what keeps playing"
        );
        assert!(!rig.has_install(), "nothing crossed the RT boundary");
        assert_eq!(live.installed_hash(), installed);

        // The refusal did not consume the live Coordinator's install slot either: a well-formed
        // swap still goes in right after.
        let ok = live.swap_document(&envelope_doc("/eg"));
        assert!(
            ok.report.ok,
            "the refusal left the slot open: {:?}",
            ok.report
        );
    }

    #[test]
    fn a_prepared_swap_commits_across_coordinators_that_share_a_config() {
        // The converse, so the guard above is a config check and not an accidental
        // same-Coordinator check: at a shared config the Engine is self-consistent and the
        // migration table is diffed against whatever the *committing* Coordinator has installed.
        let (discovery, _side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let (mut live, live_side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let rig = RenderRig::new(live_side);

        let prepared = discovery
            .prepare_document(&envelope_doc("/eg"))
            .expect("it builds");
        let report = live.commit_swap(prepared);
        assert!(
            report.report.ok,
            "shared config commits: {:?}",
            report.report
        );
        assert_eq!(
            report.diff.as_ref().unwrap().survived,
            1,
            "diffed against the committing Coordinator's installed document"
        );
        assert!(rig.has_install(), "it crossed");
    }

    #[test]
    fn the_one_call_swap_reclaims_before_it_builds() {
        // `swap_document` is prepare + commit, and `commit_swap` reclaims — but reclaiming ONLY
        // there would hold the retiree alive across the build, raising this call's peak from
        // live + new to live + retiree + new. A whole extra Engine is a real cost to a door under
        // a memory cap. The observable proxy: with a retiree waiting, the reclaim has already
        // happened by the time the document is parsed, so a swap whose document is REJECTED — one
        // that never reaches `commit_swap` at all — still comes back with the slot cleared.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);

        let first = coord.swap_document(&envelope_doc("/env"));
        assert!(first.report.ok);
        rig.poll_install(); // the render side drains and posts the retiree home

        // A document that never builds: it returns before `commit_swap` is ever reached.
        let rejected = coord.swap_document("{ not json");
        assert!(!rejected.report.ok, "the document is rejected");
        assert!(
            coord.try_reclaim().is_none(),
            "the rejected swap had already reclaimed the retiree — the reclaim runs before the \
             build, not after it"
        );
    }

    #[test]
    fn prepare_rejects_a_bad_document_and_names_what_keeps_playing() {
        // A document that will not load never becomes a `PreparedSwap`: the error side is the same
        // rejection report `swap_document` returns, and nothing moves.
        let (coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let rig = RenderRig::new(side);
        let installed = coord.installed_hash();

        let rejected = coord
            .prepare_document("{ not json")
            .err()
            .expect("a bad document does not prepare");
        assert!(!rejected.report.ok);
        assert!(!rejected.report.errors.is_empty(), "it says why");
        assert!(rejected.diff.is_none(), "a rejected swap installs nothing");
        assert_eq!(
            rejected.content_hash, installed,
            "the report names what keeps playing"
        );
        assert!(!rig.has_install(), "nothing crossed");
        assert_eq!(coord.installed_hash(), installed);
    }

    #[test]
    fn reclaim_returns_the_retiree_for_off_thread_drop() {
        // After the render side applies a swap it posts the retired Engine back; the
        // Coordinator reclaims it (to drop off-thread) which also opens the slot for the next swap.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);

        // Before the render side drains it, the retiree is not home.
        let first = coord.swap_document(&envelope_doc("/env"));
        assert!(first.report.ok);
        assert!(
            coord.try_reclaim().is_none(),
            "no retiree until the render side drains the install"
        );

        rig.poll_install(); // drains, transplants, posts the retiree
        assert!(
            coord.try_reclaim().is_some(),
            "the retiree comes home after the render side posts it"
        );

        // With the slot clear, a second swap installs cleanly.
        let second = coord.swap_document(&envelope_doc("/env"));
        assert!(
            second.report.ok,
            "second swap installs: {:?}",
            second.report
        );
    }
}
