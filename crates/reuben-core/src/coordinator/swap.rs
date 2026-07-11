//! The passive, OS-free [`Coordinator`] (ADR-0046 §7): the single writer of graph structure.
//!
//! It owns the [`Registry`] handle, the resolver, the installed-Plan [`Manifest`], the canonical
//! [`NormalizedDoc`], and the Coordinator-side mailbox endpoint. [`Coordinator::swap_document`]
//! validates and builds a **whole new Engine off-thread** ([`Engine::from_document`]'s chain),
//! computes the migration table by diffing the installed manifest against the new one, fills the
//! install mailbox with the [`InstallBundle`] (new Engine + table), and returns a real
//! [`SwapReport`]. [`Coordinator::reclaim`] drops the retired bundle off-thread.
//!
//! "Off-thread build" is a property of the *caller*, not this code: everything here is a plain
//! function with no clock, threads, or I/O, so the native shell runs [`swap_document`] on a worker
//! thread and the render side never blocks. Single-writer discipline is enforced by `&mut self`.
//! The Coordinator **never touches devices** (ADR-0046 §6).

use crate::config::AudioConfig;
use crate::contract::{content_hash, Diag, Report, SwapReport};
use crate::engine::{Engine, FromDocumentError};
use crate::format::{load_instrument_doc, LoadWarning, NormalizedDoc};
use crate::plan::Plan;
use crate::registry::Registry;
use crate::resources::ResourceResolver;

use super::mailbox::{swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox};
use super::manifest::{build_manifest, Manifest, MigrationTable};

/// What crosses the install mailbox (ADR-0046 §1, §4): a complete [`Engine`] — the Plan's runtime
/// vessel, so the callback allocates nothing post-install — plus the precomputed
/// [`MigrationTable`] the render side transplants by. This is the payload type ticket #321's RT
/// install slot drains and applies; the retiree posted back is the same type (its `migration` is
/// then irrelevant — a reclaimed Engine has nothing to migrate).
pub struct InstallBundle {
    /// The freshly built Engine to install at the next callback top.
    pub engine: Engine,
    /// The `(old index, new index)` survivor pairs to transplant into `engine` from the retiring
    /// Engine before it goes live (ADR-0046 §4).
    pub migration: MigrationTable,
}

/// The render side's half of a fresh Coordinator: the **initial** Engine (installed directly into
/// the callback, not through the mailbox) and the [`RenderMailbox`] the callback drains. Ticket
/// #321 builds the production RT slot that owns these; [`Coordinator::install_initial`] hands them
/// out so the shell can wire its audio callback.
pub struct RenderSide {
    pub engine: Engine,
    pub mailbox: RenderMailbox<InstallBundle>,
}

/// The passive Coordinator (ADR-0046 §7). Single-writer by `&mut self`; OS-free.
pub struct Coordinator {
    registry: Registry,
    resolver: Box<dyn ResourceResolver + Send>,
    config: AudioConfig,
    /// The canonical installed document (ADR-0046 §7): the source of truth for what is playing,
    /// and the basis of the content hash a swap's `expect` guard compares (§9).
    doc: NormalizedDoc,
    /// The installed Plan's survivor identities (ADR-0046 §5), diffed against each swap's new Plan.
    manifest: Manifest,
    /// The Coordinator-side install/retire mailbox endpoint (ADR-0046 §2).
    mailbox: CoordinatorMailbox<InstallBundle>,
    /// The currently-installed engine's logical output channel count ([`Engine::channels`]). The
    /// engine itself has crossed into the mailbox, but the native shell still needs this logical
    /// geometry to rebuild the device output map off-thread against the retained device channel
    /// count (ADR-0046 §6). A *logical* count — not the device's — so recording it here keeps the
    /// Coordinator device-free (ADR-0046 §7).
    installed_channels: usize,
    /// The currently-installed engine's logical input channel count ([`Engine::input_channels`]).
    /// The native shell reads it to decide the input dark-degrade warning (ADR-0038 §7): an engine
    /// that binds input channels no open stream provides degrades to silence with a loud warning.
    installed_input_channels: usize,
}

impl Coordinator {
    /// Build the initial Engine from `doc_json`, take its manifest, and return the Coordinator
    /// alongside the [`RenderSide`] (initial Engine + render mailbox) for the shell's callback and
    /// the load warnings (resource problems are non-fatal, ADR-0016, but the shell must surface
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
        // Record the initial engine's logical geometry before it moves into the RenderSide (ADR-0046
        // §6/§7): the native shell reads it to build the first device output map.
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

    /// The content hash of the currently installed document (ADR-0046 §9) — the token a later
    /// swap's `expect` guard compares, and what every report names as "what is playing".
    pub fn installed_hash(&self) -> String {
        content_hash(&self.doc)
    }

    /// The canonical installed document (ADR-0046 §7): a fresh conversation reads what is playing
    /// from here.
    pub fn document(&self) -> &NormalizedDoc {
        &self.doc
    }

    /// The currently-installed engine's logical output channel count ([`Engine::channels`]). The
    /// native shell reads it to rebuild the device output map off-thread against the retained device
    /// channel count after a swap (ADR-0046 §6) — a *logical* count, so the Coordinator stays
    /// device-free (§7).
    pub fn installed_channels(&self) -> usize {
        self.installed_channels
    }

    /// The currently-installed engine's logical input channel count ([`Engine::input_channels`]).
    /// The native shell reads it to raise the input dark-degrade warning (ADR-0038 §7/§9) when a
    /// swapped-in engine binds input channels no open stream provides.
    pub fn installed_input_channels(&self) -> usize {
        self.installed_input_channels
    }

    /// Validate + build a whole new Engine off-thread, precompute the migration table, fill the
    /// install mailbox, and return a real [`SwapReport`] (ADR-0046 §§5,7,9).
    ///
    /// `source` is the document JSON (by-path resolution is a shell concern — the resolver seam —
    /// kept out of this OS-free core). `expect`, when `Some`, is the installed content hash the
    /// client believes is live: a mismatch is the optimistic-concurrency guard (§9) — the swap is
    /// rejected, nothing installs, and the report names the *actual* installed hash so the client
    /// re-reads and reconciles. A load/instantiate error likewise installs nothing. On success the
    /// report carries the real survivor/reset [`DiffSummary`] and the now-installed hash, and the
    /// canonical document + manifest advance (last-write-wins).
    pub fn swap_document(&mut self, source: &str, expect: Option<&str>) -> SwapReport {
        // Opportunistically clear a previous swap whose retiree has come home, so a caller that
        // drove the render side between swaps can install the next one without a separate reclaim.
        self.mailbox.try_reclaim();

        // Parse + normalize (the loader is the single validation authority, ADR-0045 §3).
        let new_doc = match NormalizedDoc::from_json(source, &self.registry, Some(&*self.resolver))
        {
            Ok(d) => d,
            Err(e) => return self.reject(vec![Diag::from_load(&e)]),
        };

        // Optimistic-concurrency guard (ADR-0046 §9): honor `expect` before building anything.
        if let Some(expected) = expect {
            let actual = self.installed_hash();
            if expected != actual {
                return self.reject(vec![Diag {
                    node: None,
                    port: None,
                    message: format!(
                        "swap rejected: expected installed document {expected:?}, but {actual:?} \
                         is installed — a concurrent edit won; re-read and reconcile"
                    ),
                }]);
            }
        }

        // Build the whole new Engine + its manifest off-thread.
        let (engine, new_manifest, warnings) =
            match build_engine(&new_doc, &self.registry, &*self.resolver, self.config) {
                Ok(built) => built,
                Err(FromDocumentError::Load(e)) => return self.reject(vec![Diag::from_load(&e)]),
                Err(FromDocumentError::Plan(e)) => {
                    return self.reject(vec![Diag {
                        node: None,
                        port: None,
                        message: format!("instantiate plan: {e:?}"),
                    }])
                }
            };

        // The new engine's logical geometry, captured before it is boxed into the mailbox — the
        // native shell reads it back (via the installed-geometry accessors) to rebuild the device
        // output map off-thread (ADR-0046 §6) and to compute the input dark-degrade warning (§7).
        let new_channels = engine.channels();
        let new_input_channels = engine.input_channels();

        // Precompute the migration table + diff (the edit always wins, ADR-0046 §5).
        let (migration, diff) = self.manifest.diff(&new_manifest);

        // Fill the install mailbox. One swap in flight (§2): if the render side has not yet drained
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
        let content_hash = content_hash(&new_doc);
        self.doc = new_doc;
        self.manifest = new_manifest;
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

    /// Non-blocking reclaim of the retired [`InstallBundle`] (ADR-0009 deferred free): take it back
    /// so the caller can drop it off the audio thread, and open the slot for the next swap. `None`
    /// if the render side has not posted the retiree yet.
    pub fn try_reclaim(&mut self) -> Option<Box<InstallBundle>> {
        self.mailbox.try_reclaim()
    }

    /// Blocking reclaim (ADR-0046 §2): poll until the retiree returns or `timed_out` fires. The
    /// caller supplies the clock (core is OS-free) — see [`CoordinatorMailbox::reclaim`]. Dropping
    /// the returned bundle is the off-thread free; it opens the slot for the next swap.
    pub fn reclaim(
        &mut self,
        timed_out: impl FnMut() -> bool,
    ) -> Result<Box<InstallBundle>, ReclaimError> {
        self.mailbox.reclaim(timed_out)
    }

    /// A rejected swap installs nothing (ADR-0046 §9): `ok: false`, the given diagnostics, no
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
    use crate::message::Arg;
    use crate::resources::{MemoryResolver, SampleBuffer};
    use std::sync::{Arc, Mutex};

    const DEFAULT_VOICE_JSON: &str =
        include_str!("../../../../instruments/voices/default-voice.json");

    fn cfg() -> AudioConfig {
        AudioConfig::new(48_000.0, 128)
    }

    /// A **test-only** render slot standing in for ticket #321's production RT install slot. It
    /// owns the live Engine + the render-side mailbox and, at each `poll_install`, drains a pending
    /// swap and applies the **same migration-table semantics** the RT slot will: box-transplant the
    /// survivors, install the new Engine, post the retiree. It applies them *synchronously* (no
    /// callback, no atomics timing) purely so a Coordinator-driven swap's survivor-vs-reset
    /// behavior can be OBSERVED in rendered audio. It is NOT the RT path — #321 owns that.
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

        /// The callback-top install step (ADR-0046 §§3,4), synchronous test form.
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
        // The behavioral heart of ADR-0046 §5. Warm the envelope to its sustain level, then swap.
        // Swapping to the identical document keeps `/env` a survivor (address + type + fingerprint
        // all match): the transplanted box carries its held level, so the first post-swap block is
        // still ringing at sustain. Swapping to a document that *renames* the node makes it a
        // remove+add (ADR-0045 §2): the fresh box restarts its attack from zero.
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
            let report = coord.swap_document(&base, None);
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
            let report = coord.swap_document(&envelope_doc("/eg"), None);
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
        // The load-bearing survivor half of the asymmetry (ADR-0045 §2 / ADR-0046 §5): a runtime
        // `inputs` param is NOT part of the survivor key, so editing one leaves the node a survivor
        // — the box (with its warmed state) transplants and the new Plan's latch supplies the new
        // value. Warm the envelope to sustain, then swap to a document that differs ONLY in `attack`
        // (a pure runtime param, inert while the gate is held): the transplanted box keeps its held
        // level, so it is still ringing at sustain on the first post-swap block. The counterpart to
        // `a_survivor_keeps_state_a_reset_starts_fresh` — here the edited node must NOT reset.
        let (mut coord, side, _w) = Coordinator::install_initial(
            &envelope_doc_attack(0.5),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let mut rig = RenderRig::new(side);
        rig.render_peak(48_000); // ~1s: past attack+decay, sitting at sustain

        let report = coord.swap_document(&envelope_doc_attack(0.05), None);
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
        let report = coord.swap_document(&voicer_doc(swap_to), None);
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        rig.poll_install();
        coord.try_reclaim();
        rig.render_peak(2_048)
    }

    #[test]
    fn bumping_voices_resets_the_voicer_unchanged_survives() {
        // ADR-0046 §5: the voicer's `voices` pool size is an instantiate-time Constant. Swapping to
        // an identical `voices` keeps the voicer a survivor — its held note keeps ringing across
        // the swap. Bumping `voices` 4→8 is a different instantiation (the box carries a 4-voice
        // pool): the voicer resets to a fresh, silent 8-voice pool and the note is gone.
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
    /// re-upload-same-path flow (ADR-0046 §5 / MCP/F). Interior-mutable so a test can change the
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
        let report = coord.swap_document(SAMPLE_DOC, None);
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        rig.poll_install();
        coord.try_reclaim();
        rig.render_peak(2_048)
    }

    #[test]
    fn reresolving_a_sample_to_different_bytes_resets_the_player() {
        // ADR-0046 §5 / ADR-0016: a sample's identity is its decoded bytes, not its path. Swapping
        // the identical document with the *same* bytes keeps the player a survivor — the one-shot
        // keeps playing across the swap. Re-uploading different bytes at the same path is a
        // different instantiation: the player resets to a fresh, un-triggered box and falls silent.
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

    // ---- Concurrency guard + hash, and reclaim ----

    #[test]
    fn expect_guard_rejects_a_stale_swap_and_names_the_installed_hash() {
        // ADR-0046 §9: a swap carrying an `expect` that does not match the installed document is
        // rejected — nothing installs — and the report names the actually-installed hash so the
        // client re-reads and reconciles. `None` is last-write-wins and always installs.
        let (mut coord, _side, _w) = Coordinator::install_initial(
            &envelope_doc("/env"),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let installed = coord.installed_hash();

        let stale = coord.swap_document(&envelope_doc("/eg"), Some("deadbeefdeadbeef"));
        assert!(!stale.report.ok, "a stale expect is rejected");
        assert!(stale.diff.is_none(), "a rejected swap installs nothing");
        assert_eq!(
            stale.content_hash, installed,
            "the report names what keeps playing"
        );
        assert_eq!(
            coord.installed_hash(),
            installed,
            "the installed document is unchanged"
        );

        // The matching guard (and LWW `None`) go through.
        let ok = coord.swap_document(&envelope_doc("/eg"), Some(&installed));
        assert!(ok.report.ok, "a matching expect installs: {:?}", ok.report);
        assert_ne!(
            coord.installed_hash(),
            installed,
            "the swap advanced the doc"
        );
    }

    /// A minimal instrument that binds logical input channel 0 (ADR-0038 §3): `input_channels`
    /// becomes 1. No resources, so a bare [`MemoryResolver`] loads it. Used to prove the
    /// installed-geometry accessors advance across a swap.
    const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
        "interface": {
            "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "out": { "from": "/mic" } } },
        "nodes": [] }"#;

    #[test]
    fn installed_channels_reflect_the_installed_engine_geometry() {
        // The native shell reads these to build the device output map off-thread (ADR-0046 §6) and
        // to compute the input dark-degrade warning (ADR-0038 §7): both need the *currently
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
        let report = coord.swap_document(MIC_PASSTHRU, None);
        assert!(report.report.ok, "swap should succeed: {:?}", report.report);
        assert_eq!(
            coord.installed_input_channels(),
            1,
            "the swapped-in rig binds logical input channel 0"
        );

        // A rejected swap leaves the geometry unchanged (it names what keeps playing).
        let rejected = coord.swap_document("{ not json", None);
        assert!(!rejected.report.ok);
        assert_eq!(
            coord.installed_input_channels(),
            1,
            "a rejected swap does not advance the installed geometry"
        );
    }

    #[test]
    fn reclaim_returns_the_retiree_for_off_thread_drop() {
        // ADR-0009/§2: after the render side applies a swap it posts the retired Engine back; the
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
        let first = coord.swap_document(&envelope_doc("/env"), None);
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
        let second = coord.swap_document(&envelope_doc("/env"), None);
        assert!(
            second.report.ok,
            "second swap installs: {:?}",
            second.report
        );
    }
}
