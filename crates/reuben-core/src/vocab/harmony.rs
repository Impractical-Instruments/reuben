//! Tonal context — the latched harmony value followers resolve against.
//!
//! A [`Harmony`] is the current key/scale/chord, a small **`Copy`** value so the engine can
//! snapshot it onto the Message wire allocation-free: the slicing model *forces*
//! the `Copy` shape. It owns the resolver — `hz` (degree → Hz), `snap` (arbitrary pitch →
//! nearest in-scale degree), `chord_tone` — so the Scale∘Tuning composition lives in one
//! correct place and followers stay dumb (`io.read(IN_HARMONY).hz(p)`).
//!
//! Representation: a **Scale** is ordered **step**-offsets within the tuning's
//! period (12-EDO major = `[0,2,4,5,7,9,11]`) plus a root; `degree d → root + scale[d mod
//! len] + octave*period`. A **Chord** is a tagged union — scale-relative (re-spells with the
//! key) or absolute (frozen). This v1.1 slice is **12-TET only** (period 12); Scala/EDO
//! tunings ride the same step-space seam (the shared vocab registry) and land with the
//! "Format & library" thread.

use crate::vocab::pitch::Pitch;

/// Max scale degrees in a `Harmony` (within a 12-TET period). The registry-side full tuning
/// ladder (large MOS / Scala) is a separate, deferred axis.
pub const SCALE_CAP: usize = 12;
/// Max chord tones in a `Harmony`.
pub const CHORD_CAP: usize = 8;

/// Steps per period — 12-TET only for v1.1. Scale lives in step-space so a tuning
/// swap moves Hz without re-spelling; that swap is the deferred piece.
const PERIOD: i32 = 12;
const REF_MIDI: f32 = 69.0;
const REF_HZ: f32 = 440.0;
/// Float-compare slop for snap distance/direction ties (well below a cent).
const EPS: f32 = 1e-4;

/// MIDI (12-TET coordinate / absolute step) → frequency in Hz.
fn midi_to_hz(midi: f32) -> f32 {
    REF_HZ * 2.0_f32.powf((midi - REF_MIDI) / 12.0)
}

/// An ordered set of within-period **step** offsets plus a length — the Scale field of a
/// [`Harmony`]. Inline + `Copy` (no heap) so a context snapshot is a memcpy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleField {
    offsets: [i16; SCALE_CAP],
    len: u8,
}

impl ScaleField {
    /// Build from a step-offset slice (truncated to [`SCALE_CAP`]; min length 1). An **empty**
    /// slice floors to a 1-degree `[0]` scale — `len` clamps up to 1 while the copy takes only the
    /// offsets actually supplied, so there is no out-of-range read.
    pub fn new(offs: &[i16]) -> Self {
        let mut offsets = [0i16; SCALE_CAP];
        let take = offs.len().min(SCALE_CAP);
        offsets[..take].copy_from_slice(&offs[..take]);
        let len = offs.len().clamp(1, SCALE_CAP);
        Self {
            offsets,
            len: len as u8,
        }
    }

    /// The 12-EDO major scale `[0,2,4,5,7,9,11]` — the default. A `const` so
    /// [`Harmony::DEFAULT`] can be a `const` (the typed-handle default).
    pub const MAJOR: Self = Self {
        offsets: [0, 2, 4, 5, 7, 9, 11, 0, 0, 0, 0, 0],
        len: 7,
    };

    /// The 12-EDO major scale `[0,2,4,5,7,9,11]` — the default.
    pub fn major() -> Self {
        Self::MAJOR
    }

    /// Number of degrees in the scale (≥ 1).
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Always non-empty (`len ≥ 1`); provided to satisfy clippy and read intent.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn offset(&self, idx: usize) -> i32 {
        self.offsets[idx] as i32
    }
}

/// How a chord tracks the key: the **tag** makes "follows key" vs "frozen" an
/// explicit call-site choice, defusing the silent re-spell footgun.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChordTag {
    /// No chord set.
    #[default]
    None,
    /// Offsets are **scale degrees** — re-spell diatonically as the scale changes.
    ScaleRelative,
    /// Offsets are **raw step-offsets from root** — frozen against scale changes.
    Absolute,
}

/// A chord: a tagged set of offsets (degrees if scale-relative, steps if absolute). `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub tag: ChordTag,
    offsets: [i16; CHORD_CAP],
    len: u8,
}

impl Chord {
    /// The empty chord (no chord tones). `const` so [`Harmony::DEFAULT`] can be one too.
    pub const fn empty() -> Self {
        Self {
            tag: ChordTag::None,
            offsets: [0; CHORD_CAP],
            len: 0,
        }
    }

    /// Build a chord from a tag and offset slice (truncated to [`CHORD_CAP`]).
    pub fn new(tag: ChordTag, offs: &[i16]) -> Self {
        let mut offsets = [0i16; CHORD_CAP];
        let len = offs.len().min(CHORD_CAP);
        offsets[..len].copy_from_slice(&offs[..len]);
        Self {
            tag,
            offsets,
            len: len as u8,
        }
    }

    fn len(&self) -> usize {
        if self.tag == ChordTag::None {
            0
        } else {
            self.len as usize
        }
    }
}

/// Which set [`Harmony::snap`] quantizes to. A shared *vocab* enum:
/// rides the central `Arg` as `Arg::Enum`, read by the `snap` operator as a held choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum SnapTarget {
    /// Any scale tone survives.
    #[default]
    Scale,
    /// Strict: only chord tones survive.
    Chord,
    /// Permissive: any scale tone survives, chord tones only win ties.
    ChordThenScale,
}

/// Snap direction: `Nearest` with a deterministic **down** tie-break (no coin-flip), or a
/// forced `Up`/`Down`. A shared *vocab* enum (`Arg::Enum`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum SnapDir {
    #[default]
    Nearest,
    Up,
    Down,
}

/// Snap policy — a caller argument, not baked into the context: auto-tune wants
/// `Scale/Nearest`, an arp wants `Chord`, a melody wants `ChordThenScale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SnapPolicy {
    pub target: SnapTarget,
    pub direction: SnapDir,
}

/// The latched tonal context: tuning (12-TET for v1.1) + root + scale + chord. A small
/// `Copy` value carrying the resolver; a shared *vocab* type riding the central
/// `Arg` as `Arg::Harmony`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, reuben_macros::ArgValue)]
pub struct Harmony {
    /// Tonic **step** (absolute MIDI; spans octaves). Default 60 (C4).
    pub root: i32,
    pub scale: ScaleField,
    pub chord: Chord,
}

impl Harmony {
    /// C major, 12-TET, no chord — the unwired-default tonal frame. A `const` (like every vocab
    /// enum's derive-generated `DEFAULT`) so a typed input handle can carry it as its declared
    /// default; [`Default::default`] returns exactly this value.
    pub const DEFAULT: Harmony = Harmony {
        root: 60,
        scale: ScaleField::MAJOR,
        chord: Chord::empty(),
    };
}

impl Default for Harmony {
    /// C major, 12-TET, no chord — so a rig with no context node resolves degrees exactly
    /// like the prior 12-TET default (existing rigs sound identical). One source:
    /// [`Harmony::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Running-best candidate during a [`Harmony::snap`] search (allocation-free).
#[derive(Clone, Copy)]
struct Cand {
    dist: f32,
    step: i32,
    degree: Option<i32>,
    is_chord: bool,
}

impl Harmony {
    /// Resolve a scale degree to an absolute **step** (MIDI). `degree d → root + scale[d mod
    /// len] + octave*period`; negative degrees wrap downward (Euclidean).
    pub fn degree_to_step(&self, degree: i32) -> i32 {
        let len = self.scale.len() as i32;
        let oct = degree.div_euclid(len);
        let idx = degree.rem_euclid(len) as usize;
        self.root + self.scale.offset(idx) + oct * PERIOD
    }

    /// Resolve a [`Pitch`] to Hz. A degree pitch resolves through scale + tuning (so it
    /// re-spells live); an absolute pitch uses its MIDI coordinate directly.
    pub fn hz(&self, pitch: Pitch) -> f32 {
        match pitch {
            Pitch::Degree(d) => midi_to_hz(self.degree_to_step(d) as f32),
            Pitch::Absolute(midi) => midi_to_hz(midi),
        }
    }

    /// The `n`th chord tone as a [`Pitch`] (wrapping by octave). Scale-relative tones return
    /// a *degree* (so they re-spell); absolute tones return a MIDI pitch (frozen).
    pub fn chord_tone(&self, n: i32) -> Pitch {
        let clen = self.chord.len() as i32;
        if clen == 0 {
            return Pitch::from_degree(0);
        }
        let oct = n.div_euclid(clen);
        let idx = n.rem_euclid(clen) as usize;
        let off = self.chord.offsets[idx] as i32;
        match self.chord.tag {
            ChordTag::ScaleRelative => {
                let len = self.scale.len() as i32;
                Pitch::from_degree(off + oct * len)
            }
            ChordTag::Absolute => Pitch::from_midi((self.root + off + oct * PERIOD) as f32),
            ChordTag::None => Pitch::from_degree(0),
        }
    }

    /// Visit each chord tone's step (and its degree, if scale-relative) across the octave
    /// window centred on `base_oct`. Allocation-free.
    fn for_each_chord_step(&self, base_oct: i32, mut f: impl FnMut(i32, Option<i32>)) {
        let clen = self.chord.len();
        if clen == 0 {
            return;
        }
        let len = self.scale.len() as i32;
        for oct in (base_oct - 1)..=(base_oct + 1) {
            for k in 0..clen {
                let off = self.chord.offsets[k] as i32;
                match self.chord.tag {
                    ChordTag::ScaleRelative => {
                        let degree = oct * len + off;
                        f(self.degree_to_step(degree), Some(degree));
                    }
                    ChordTag::Absolute => f(self.root + off + oct * PERIOD, None),
                    ChordTag::None => {}
                }
            }
        }
    }

    /// Is `step` a chord tone (within the search window)?
    fn step_is_chord(&self, step: i32, base_oct: i32) -> bool {
        let mut hit = false;
        self.for_each_chord_step(base_oct, |s, _| hit |= s == step);
        hit
    }

    /// Quantize an arbitrary float-MIDI gesture to the nearest in-target degree, per `policy`
    /// Distance is measured in MIDI/semitone space (≡ cents in 12-TET);
    /// microtonal cents-correct distance rides the same path when non-12-TET tunings land.
    /// Returns a symbolic [`Pitch`] (a degree where possible) so it re-resolves if the tuning
    /// swaps. Allocation-free.
    pub fn snap(&self, midi: f32, policy: SnapPolicy) -> Pitch {
        let len = self.scale.len() as i32;
        let base_oct = ((midi - self.root as f32) / PERIOD as f32).round() as i32;
        let mut best: Option<Cand> = None;

        let mut consider = |step: i32, degree: Option<i32>, is_chord: bool| {
            // Direction filter.
            match policy.direction {
                SnapDir::Up if (step as f32) < midi - EPS => return,
                SnapDir::Down if (step as f32) > midi + EPS => return,
                _ => {}
            }
            let dist = (midi - step as f32).abs();
            let cand = Cand {
                dist,
                step,
                degree,
                is_chord,
            };
            best = Some(match best {
                None => cand,
                Some(b) => pick(b, cand, policy.target),
            });
        };

        match policy.target {
            SnapTarget::Scale => {
                for oct in (base_oct - 1)..=(base_oct + 1) {
                    for idx in 0..len {
                        let degree = oct * len + idx;
                        consider(self.degree_to_step(degree), Some(degree), false);
                    }
                }
            }
            SnapTarget::ChordThenScale => {
                for oct in (base_oct - 1)..=(base_oct + 1) {
                    for idx in 0..len {
                        let degree = oct * len + idx;
                        let step = self.degree_to_step(degree);
                        let is_chord = self.step_is_chord(step, base_oct);
                        consider(step, Some(degree), is_chord);
                    }
                }
            }
            SnapTarget::Chord => {
                self.for_each_chord_step(base_oct, |step, degree| {
                    // Direction filter + best-update, inlined (can't call `consider`: two
                    // closures would both borrow `best`).
                    match policy.direction {
                        SnapDir::Up if (step as f32) < midi - EPS => return,
                        SnapDir::Down if (step as f32) > midi + EPS => return,
                        _ => {}
                    }
                    let cand = Cand {
                        dist: (midi - step as f32).abs(),
                        step,
                        degree,
                        is_chord: true,
                    };
                    best = Some(match best {
                        None => cand,
                        Some(b) => pick(b, cand, SnapTarget::Chord),
                    });
                });
            }
        }

        match best {
            Some(c) => match c.degree {
                Some(d) => Pitch::from_degree(d),
                None => Pitch::from_midi(c.step as f32),
            },
            None => Pitch::from_midi(midi), // nothing to snap to → unchanged
        }
    }
}

/// Choose the better of two snap candidates: lower distance wins; on a tie, `ChordThenScale`
/// prefers a chord tone, then the **lower step** (deterministic down tie-break).
fn pick(a: Cand, b: Cand, target: SnapTarget) -> Cand {
    if (a.dist - b.dist).abs() > EPS {
        return if a.dist < b.dist { a } else { b };
    }
    if target == SnapTarget::ChordThenScale && a.is_chord != b.is_chord {
        return if a.is_chord { a } else { b };
    }
    if a.step <= b.step {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c_major() -> Harmony {
        Harmony::default()
    }

    // `ScaleField::new` documents "min length 1": an empty offset slice floors to a 1-degree
    // `[0]` scale rather than panicking on the `&offs[..len]` copy. Pins that contract directly
    // (harmony's `degrees` floor keeps its own reads off this edge, but the type must hold it).
    #[test]
    fn scale_field_from_empty_floors_to_one_degree() {
        let s = ScaleField::new(&[]);
        assert_eq!(s, ScaleField::new(&[0]));
    }

    // The other end: an over-long slice truncates to `SCALE_CAP`, no out-of-range read.
    #[test]
    fn scale_field_truncates_past_the_cap() {
        let long: Vec<i16> = (0..(SCALE_CAP as i16 + 4)).collect();
        let s = ScaleField::new(&long);
        assert_eq!(s, ScaleField::new(&long[..SCALE_CAP]));
    }

    fn approx(a: f32, b: f32) {
        approx::assert_relative_eq!(a, b, epsilon = 0.01);
    }

    // §1 — resolution chain (degree → step → Hz), C major / 12-TET.
    #[test]
    fn resolution_chain() {
        let c = c_major();
        assert_eq!(c.degree_to_step(0), 60);
        assert_eq!(c.degree_to_step(2), 64);
        assert_eq!(c.degree_to_step(4), 67);
        assert_eq!(c.degree_to_step(7), 72); // wraps: 7 mod 7 = 0, octave 1
        assert_eq!(c.degree_to_step(-1), 59); // wraps downward to the leading tone
        approx(c.hz(Pitch::from_degree(0)), 261.63);
        approx(c.hz(Pitch::from_degree(2)), 329.63);
        approx(c.hz(Pitch::from_degree(4)), 392.00);
        approx(c.hz(Pitch::from_degree(7)), 523.25);
    }

    // §2 — octave wrap off the heptatonic path. `degree_to_step` wraps by the tuning PERIOD
    // (12) while a scale-relative `chord_tone` wraps by the *scale length* — the two constants
    // the resolver rule `degree d → root + scale[d mod len] + octave*period` keeps distinct. Every
    // other resolver test uses a 7-note scale, where a "unify the two wraps" refactor can pass
    // the whole suite; a 5-note pentatonic separates len (5) from PERIOD (12) and pins both.
    #[test]
    fn pentatonic_wrap_uses_period_for_steps_and_len_for_chord_tones() {
        let c = Harmony {
            root: 60,
            scale: ScaleField::new(&[0, 3, 5, 7, 10]), // C minor pentatonic (len 5)
            chord: Chord::new(ChordTag::ScaleRelative, &[0, 2, 4]),
        };
        // Degree 5 wraps to idx 0, one octave up: 60 + 0 + 1*PERIOD = 72 — not scale-len drift.
        assert_eq!(c.degree_to_step(5), 72);
        // Euclidean down-wrap: degree -1 → idx 4 (offset 10), octave -1: 60 + 10 - 12 = 58.
        assert_eq!(c.degree_to_step(-1), 58);
        // And two-part up: degree 9 → idx 4, octave 1: 60 + 10 + 12 = 82.
        assert_eq!(c.degree_to_step(9), 82);
        // Scale-relative chord tones wrap by scale *len* (5), not 7 and not PERIOD:
        // chord_tone(3) → offset 0 + 1*len = degree 5.
        assert_eq!(c.chord_tone(3), Pitch::from_degree(5));
        // Negative chord-tone indices wrap down the same way: offset 4 + (-1 * len) = -1.
        assert_eq!(c.chord_tone(-1), Pitch::from_degree(-1));
        // Chain the two wraps through one resolution: chord_tone(3)'s degree (len-wrapped)
        // resolves via PERIOD to exactly the octave above the root.
        assert_eq!(c.degree_to_step(c.chord_tone(3).degree().unwrap()), 72);
    }

    #[test]
    fn absolute_pitch_ignores_scale() {
        // An absolute pitch resolves by MIDI regardless of root/scale.
        let c = Harmony {
            root: 62,
            scale: ScaleField::new(&[0, 2, 3, 5, 7, 8, 10]), // D minor-ish
            chord: Chord::empty(),
        };
        approx(c.hz(Pitch::from_midi(69.0)), 440.0);
    }

    // §3 — diatonic chord motion: shifting the degree set walks the chords.
    #[test]
    fn chord_tones_walk_diatonically() {
        let c = Harmony {
            chord: Chord::new(ChordTag::ScaleRelative, &[0, 2, 4]),
            ..Harmony::default()
        };
        // I = C E G
        assert_eq!(c.chord_tone(0), Pitch::from_degree(0));
        assert_eq!(c.degree_to_step(c.chord_tone(1).degree().unwrap()), 64); // E
        assert_eq!(c.degree_to_step(c.chord_tone(2).degree().unwrap()), 67); // G
                                                                             // Wrap: chord_tone(3) → degree 7 → C5 (octave up).
        assert_eq!(c.chord_tone(3), Pitch::from_degree(7));
        assert_eq!(c.degree_to_step(7), 72);
    }

    // §4 — the re-spell footgun: scale-relative follows the key, absolute is frozen.
    #[test]
    fn scale_relative_respells_absolute_freezes() {
        let major = Harmony {
            chord: Chord::new(ChordTag::ScaleRelative, &[0, 2, 4]),
            ..Harmony::default()
        };
        // Scale-relative {0,2,4} in C major → C E G.
        assert_eq!(
            major.degree_to_step(major.chord_tone(1).degree().unwrap()),
            64
        ); // E

        let minor = Harmony {
            scale: ScaleField::new(&[0, 2, 3, 5, 7, 8, 10]), // C minor
            ..major
        };
        // Same scale-relative chord now → C E♭ G (the 3rd re-spells to 63).
        assert_eq!(
            minor.degree_to_step(minor.chord_tone(1).degree().unwrap()),
            63
        ); // E♭

        // Absolute [0,4,7] is frozen: still C E G under C minor.
        let frozen = Harmony {
            chord: Chord::new(ChordTag::Absolute, &[0, 4, 7]),
            ..minor
        };
        let e = frozen.chord_tone(1);
        approx(frozen.hz(e), midi_to_hz(64.0)); // still E natural
    }

    // §5 — snap direction and ties (Scale target).
    #[test]
    fn snap_scale_direction_and_ties() {
        let c = c_major();
        let near = SnapPolicy::default();
        let up = SnapPolicy {
            direction: SnapDir::Up,
            ..Default::default()
        };
        let down = SnapPolicy {
            direction: SnapDir::Down,
            ..Default::default()
        };
        let step = |p: Pitch| c.degree_to_step(p.degree().unwrap());
        assert_eq!(step(c.snap(64.3, near)), 64); // E
        assert_eq!(step(c.snap(64.8, near)), 65); // F
        assert_eq!(step(c.snap(66.0, near)), 65); // F♯ midpoint → tie down → F
        assert_eq!(step(c.snap(66.0, up)), 67); // forced up → G
        assert_eq!(step(c.snap(66.0, down)), 65); // forced down → F
        assert_eq!(step(c.snap(62.0, near)), 62); // already in scale → D
    }

    // §6 — snap target: Chord (strict) vs ChordThenScale (permissive).
    #[test]
    fn snap_chord_targets() {
        let c = Harmony {
            chord: Chord::new(ChordTag::ScaleRelative, &[0, 2, 4]), // C E G
            ..Harmony::default()
        };
        let strict = SnapPolicy {
            target: SnapTarget::Chord,
            ..Default::default()
        };
        let permissive = SnapPolicy {
            target: SnapTarget::ChordThenScale,
            ..Default::default()
        };
        let step = |p: Pitch| match p.degree() {
            Some(d) => c.degree_to_step(d),
            None => p.midi().unwrap() as i32,
        };
        assert_eq!(step(c.snap(62.0, strict)), 60); // D → tie C,E → down → C
        assert_eq!(step(c.snap(62.0, permissive)), 62); // D is a scale tone → kept
        assert_eq!(step(c.snap(63.0, permissive)), 64); // tie D/E → E is chord tone → E
        assert_eq!(step(c.snap(65.0, strict)), 64); // F → nearest chord tone E
        assert_eq!(step(c.snap(65.0, permissive)), 65); // F is in scale → kept
    }

    #[test]
    fn snap_with_no_target_returns_input() {
        // Chord target but no chord set → nothing to snap to → unchanged.
        let c = c_major();
        let strict = SnapPolicy {
            target: SnapTarget::Chord,
            ..Default::default()
        };
        let p = c.snap(63.4, strict);
        approx(p.midi().unwrap(), 63.4);
    }

    #[test]
    fn context_is_copy_and_small() {
        // The slicing model forces a `Copy`, heap-free struct: assert it stays so.
        fn assert_copy<T: Copy>() {}
        assert_copy::<Harmony>();
        // Guard against an accidental Box/Vec creeping in and ballooning the snapshot.
        assert!(std::mem::size_of::<Harmony>() <= 64);
    }
}
