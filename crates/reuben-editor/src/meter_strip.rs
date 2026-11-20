//! The master meter strip: a bar per device output channel, and per input channel when the
//! engine opened an input stream, read from the engine's `GetMeters` over the structure channel —
//! the same way whether the editor started the engine or attached to one.
//!
//! The engine does the ballistics: its peak falls back at a fixed rate and its RMS is averaged, so
//! a poll at any rate reads the same values. The strip adds what only a display needs: a held peak
//! marker, and a clip lamp that stays lit until clicked. The lamp sees the peak only when it polls,
//! and the engine's peak starts falling at once, so an over smaller than the fall between two polls
//! can go unseen.
//!
//! The exchange runs on a thread of its own, so an attached engine that stops answering holds that
//! thread for a read budget, never the window. The thread waits out the interval before it asks,
//! and its reply wakes the window, so each poll costs the window one frame.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Sense, Stroke, Vec2, WidgetInfo, WidgetType};
use reuben_api::engine::{Channel, ChannelLevel, MetersReport, METER_FLOOR_DBFS};

use crate::theme;

/// The bottom of the drawn scale. The engine reports down to its own floor, far below anything a
/// bar this size can show apart from silence.
pub const SCALE_FLOOR_DBFS: f32 = -60.0;
/// A peak at or above full scale lights the clip lamp.
const CLIP_DBFS: f32 = 0.0;
/// How long the peak marker stays at a peak before it follows the level down.
const HOLD: Duration = Duration::from_millis(1500);
/// How often the engine is asked while its output is running: about the rate a level reads as
/// moving smoothly. An attached engine's exchange dials afresh each time, which can stretch it.
const EVERY: Duration = Duration::from_millis(33);
/// How often it is asked while no output frames move, as on an engine with no audio device:
/// nothing changes until they do, so this only bounds how late the strip notices them start.
const IDLE_EVERY: Duration = Duration::from_millis(500);
/// How long the output frame count must stand still before the output counts as stopped. A device
/// runs the callback once a period, which can be longer than [`EVERY`], so two polls in one period
/// read the same count from an output that is running; this is longer than any period a device
/// plays at.
const STALL: Duration = Duration::from_millis(500);

const BAR_WIDTH: f32 = 12.0;
/// The shortest a bar is drawn, however little room its panel leaves.
const MIN_BAR_HEIGHT: f32 = 60.0;
const LAMP: Vec2 = Vec2::new(12.0, 8.0);
const SCALE_WIDTH: f32 = 24.0;
const SPACING: f32 = 2.0;
/// Wide enough that a message beside the controls wraps instead of widening the meters' panel.
const MESSAGE_WIDTH: f32 = 140.0;
/// Where the scale is marked, and each bar ticked.
const MARKS: [f32; 6] = [0.0, -12.0, -24.0, -36.0, -48.0, -60.0];

/// What the strip knows of the engine.
#[derive(Debug, Clone, PartialEq)]
enum Reading {
    /// There is no engine to read: none runs, or the attached one stopped answering its probe.
    NoEngine,
    /// There is an engine, and its first reply has not come yet.
    Waiting,
    /// The engine's `GetMeters` failed, and why.
    Failed(String),
    /// The engine answered.
    Levels,
}

/// One channel as drawn.
#[derive(Debug, Clone, PartialEq)]
struct Bar {
    level: ChannelLevel,
    held: f32,
    held_until: Option<Instant>,
    clipped: bool,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            level: ChannelLevel {
                peak_dbfs: METER_FLOOR_DBFS,
                rms_dbfs: METER_FLOOR_DBFS,
            },
            held: METER_FLOOR_DBFS,
            held_until: None,
            clipped: false,
        }
    }
}

impl Bar {
    fn take(&mut self, level: ChannelLevel, now: Instant) {
        self.level = level;
        if level.peak_dbfs >= self.held {
            self.held = level.peak_dbfs;
            self.held_until = Some(now + HOLD);
        } else if self.held_until.is_none_or(|until| now >= until) {
            // The hold stays expired, so the marker rides the level down until the next new peak.
            self.held = level.peak_dbfs;
        }
        self.clipped |= level.peak_dbfs >= CLIP_DBFS;
    }
}

/// The strip's state, and the thread that asks the engine for its levels.
pub struct MeterStrip {
    /// A channel to ask, and the moment to ask it at.
    asks: Sender<(Channel, Instant)>,
    replies: Receiver<Result<MetersReport, String>>,
    asked: Option<Instant>,
    in_flight: bool,
    reading: Reading,
    frames: Option<u64>,
    /// When a reply last found the output frame count changed, the first reply included.
    moved: Option<Instant>,
    /// Whether the count has stood still for [`STALL`], as of the last reply.
    stopped: bool,
    outputs: Vec<Bar>,
    inputs: Vec<Bar>,
}

impl MeterStrip {
    /// A strip with nothing read yet. `wake` is called from the asking thread on each reply.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (asks, asked) = mpsc::channel::<(Channel, Instant)>();
        let (replied, replies) = mpsc::channel();
        // Not joined: it ends once the strip is dropped and its exchange in hand returns.
        std::thread::spawn(move || {
            for (channel, at) in asked {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
                let reply = channel.get_meters().map_err(|e| e.to_string());
                if replied.send(reply).is_err() {
                    return;
                }
                wake();
            }
        });
        Self {
            asks,
            replies,
            asked: None,
            in_flight: false,
            reading: Reading::NoEngine,
            frames: None,
            moved: None,
            stopped: false,
            outputs: Vec::new(),
            inputs: Vec::new(),
        }
    }

    /// Take in any reply, and queue the next ask of the engine behind `channel` when none is
    /// queued. Never waits; the reply wakes the window.
    pub fn poll(&mut self, now: Instant, channel: Option<Channel>) {
        for reply in self.replies.try_iter().collect::<Vec<_>>() {
            self.in_flight = false;
            match reply {
                Ok(report) => self.take(report, now),
                Err(why) => self.reading = Reading::Failed(why),
            }
        }
        let Some(channel) = channel else {
            self.reading = Reading::NoEngine;
            return;
        };
        if self.reading == Reading::NoEngine {
            self.reading = Reading::Waiting;
        }
        if self.in_flight {
            return;
        }
        let every = if self.stopped { IDLE_EVERY } else { EVERY };
        let at = self.asked.map_or(now, |asked| (asked + every).max(now));
        if self.asks.send((channel, at)).is_err() {
            self.reading = Reading::Failed("the meter thread stopped".to_string());
            return;
        }
        self.in_flight = true;
        self.asked = Some(at);
    }

    fn take(&mut self, report: MetersReport, now: Instant) {
        // A count that went back is a restarted engine: what was held and lit belonged to the
        // last run, whatever its device.
        let restarted = self.frames.is_some_and(|frames| report.frames < frames);
        if self.frames != Some(report.frames) {
            self.moved = Some(now);
        }
        self.stopped = self.moved.is_some_and(|moved| now - moved >= STALL);
        self.frames = Some(report.frames);
        self.reading = Reading::Levels;
        for (bars, levels) in [
            (&mut self.outputs, report.outputs),
            (&mut self.inputs, report.inputs),
        ] {
            if restarted || bars.len() != levels.len() {
                *bars = vec![Bar::default(); levels.len()];
            }
            for (bar, level) in bars.iter_mut().zip(levels) {
                bar.take(level, now);
            }
        }
    }
}

/// Where `dbfs` falls on the drawn scale, 0 at its floor and 1 at full scale.
fn fraction(dbfs: f32) -> f32 {
    ((dbfs - SCALE_FLOOR_DBFS) / -SCALE_FLOOR_DBFS).clamp(0.0, 1.0)
}

fn db(dbfs: f32) -> String {
    if dbfs <= SCALE_FLOOR_DBFS {
        format!("below {SCALE_FLOOR_DBFS} dBFS")
    } else {
        format!("{dbfs:.1} dBFS")
    }
}

/// Draw the strip: a vertical bar per channel, side by side, outputs then inputs, filling the
/// height of `ui`.
pub fn show(ui: &mut egui::Ui, strip: &mut MeterStrip) {
    let message = match &strip.reading {
        Reading::NoEngine => Some(("Meters: no engine to read", None)),
        Reading::Waiting => Some(("Meters: reading the engine…", None)),
        Reading::Failed(why) => Some(("Meters: unavailable", Some(why.as_str()))),
        Reading::Levels if strip.outputs.is_empty() && strip.inputs.is_empty() => {
            Some(("Meters: the engine has no audio device", None))
        }
        Reading::Levels => None,
    };
    if let Some((text, why)) = message {
        ui.set_max_width(MESSAGE_WIDTH);
        let shown = ui.weak(text);
        if let Some(why) = why {
            shown.on_hover_text(why);
        }
        return;
    }
    let stopped = strip.stopped;
    let color = if stopped {
        ui.visuals().weak_text_color()
    } else {
        theme::METER
    };
    // What a column draws above and below its bar: the clip lamp, and the channel's number.
    let chrome = LAMP.y + ui.text_style_height(&egui::TextStyle::Small) + 3.0 * SPACING;
    // The group titles above, and the stopped note below.
    let reserved = 2.0 * (ui.text_style_height(&egui::TextStyle::Body) + SPACING);
    let height = (ui.available_height() - chrome - reserved).max(MIN_BAR_HEIGHT);
    ui.horizontal_top(|ui| {
        scale(ui, height);
        group(ui, "Out", &mut strip.outputs, height, color);
        if !strip.inputs.is_empty() {
            ui.separator();
            group(ui, "In", &mut strip.inputs, height, color);
        }
    });
    if stopped {
        ui.weak("Output stopped");
    }
}

/// The dB marks beside the bars, lined up with them.
fn scale(ui: &mut egui::Ui, height: f32) {
    ui.vertical(|ui| {
        ui.label("");
        ui.add_space(LAMP.y + SPACING);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(SCALE_WIDTH, height), Sense::hover());
        let font = egui::TextStyle::Small.resolve(ui.style());
        let color = ui.visuals().weak_text_color();
        for dbfs in MARKS {
            let y = rect.bottom() - rect.height() * fraction(dbfs);
            ui.painter().text(
                egui::pos2(rect.right(), y),
                egui::Align2::RIGHT_CENTER,
                format!("{dbfs}"),
                font.clone(),
                color,
            );
        }
    });
}

/// A titled run of channels, side by side.
fn group(ui: &mut egui::Ui, title: &str, bars: &mut [Bar], height: f32, color: Color32) {
    ui.vertical(|ui| {
        ui.label(title);
        ui.horizontal_top(|ui| {
            for (i, bar) in bars.iter_mut().enumerate() {
                channel(ui, title, i + 1, bar, height, color);
            }
        });
    });
}

/// One channel: its clip lamp, which a click puts out, its bar, and its number.
fn channel(
    ui: &mut egui::Ui,
    title: &str,
    number: usize,
    bar: &mut Bar,
    height: f32,
    color: Color32,
) {
    let name = format!("{title} {number}");
    ui.vertical_centered(|ui| {
        ui.set_width(BAR_WIDTH + 2.0 * SPACING);
        lamp(ui, &name, bar);
        let described = format!(
            "peak {}, RMS {}",
            db(bar.level.peak_dbfs),
            db(bar.level.rms_dbfs)
        );
        let (rect, response) = ui.allocate_exact_size(Vec2::new(BAR_WIDTH, height), Sense::hover());
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(described.clone()),
            ..WidgetInfo::labeled(WidgetType::ProgressIndicator, true, format!("{name} level"))
        });
        response.on_hover_text(format!("{name}: {described}"));
        if ui.is_rect_visible(rect) {
            paint(ui, rect, bar, color);
        }
        ui.small(number.to_string());
    });
}

fn paint(ui: &egui::Ui, rect: egui::Rect, bar: &Bar, color: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let up_to = |dbfs: f32| {
        let mut filled = rect;
        filled.set_top(rect.bottom() - rect.height() * fraction(dbfs));
        filled
    };
    painter.rect_filled(up_to(bar.level.peak_dbfs), 0.0, color.gamma_multiply(0.4));
    painter.rect_filled(up_to(bar.level.rms_dbfs), 0.0, color);
    let tick = ui.visuals().widgets.noninteractive.bg_stroke;
    for dbfs in MARKS {
        let y = rect.bottom() - rect.height() * fraction(dbfs);
        painter.hline(rect.x_range(), y, tick);
    }
    if bar.held > SCALE_FLOOR_DBFS {
        let y = rect.bottom() - rect.height() * fraction(bar.held);
        painter.hline(
            rect.x_range(),
            y,
            Stroke::new(2.0, ui.visuals().strong_text_color()),
        );
    }
    painter.rect_stroke(rect, 0.0, tick, egui::StrokeKind::Inside);
}

fn lamp(ui: &mut egui::Ui, name: &str, bar: &mut Bar) {
    let (rect, clicked) = ui.allocate_exact_size(LAMP, Sense::click());
    let clipped = bar.clipped;
    clicked.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, true, clipped, format!("{name} clip"))
    });
    let clicked = clicked.on_hover_text(if clipped {
        "Clipped: click to clear"
    } else {
        "Lights when a peak reaches full scale"
    });
    if clicked.clicked() {
        bar.clipped = false;
    }
    let fill = if bar.clipped {
        theme::CLIP
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 1.0, fill);
    ui.painter().rect_stroke(
        rect,
        1.0,
        ui.visuals().widgets.noninteractive.fg_stroke,
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use reuben_api::engine::{Response, Transport};

    use super::*;

    fn level(peak_dbfs: f32, rms_dbfs: f32) -> ChannelLevel {
        ChannelLevel {
            peak_dbfs,
            rms_dbfs,
        }
    }

    const SILENT: ChannelLevel = ChannelLevel {
        peak_dbfs: METER_FLOOR_DBFS,
        rms_dbfs: METER_FLOOR_DBFS,
    };

    #[test]
    fn the_scale_runs_from_its_floor_to_full_scale() {
        assert_eq!(fraction(METER_FLOOR_DBFS), 0.0);
        assert_eq!(fraction(SCALE_FLOOR_DBFS), 0.0);
        assert_eq!(fraction(-30.0), 0.5);
        assert_eq!(fraction(0.0), 1.0);
        assert_eq!(fraction(6.0), 1.0);
    }

    #[test]
    fn the_peak_marker_holds_then_follows_the_level_down() {
        let start = Instant::now();
        let mut bar = Bar::default();
        bar.take(level(-6.0, -9.0), start);
        bar.take(level(-12.0, -15.0), start + HOLD / 2);
        assert_eq!(bar.held, -6.0, "held within the hold time");
        bar.take(level(-3.0, -6.0), start + HOLD / 2);
        assert_eq!(bar.held, -3.0, "a higher peak takes over at once");
        let expired = start + HOLD / 2 + HOLD;
        bar.take(level(-20.0, -23.0), expired);
        assert_eq!(bar.held, -20.0, "the hold ran out");
        bar.take(level(-21.0, -24.0), expired + EVERY);
        assert_eq!(bar.held, -21.0, "and stays out, following the level down");
        bar.take(level(-9.0, -12.0), expired + EVERY * 2);
        bar.take(level(-15.0, -18.0), expired + EVERY * 3);
        assert_eq!(bar.held, -9.0, "a new peak is held afresh");
    }

    #[test]
    fn the_clip_lamp_stays_lit_after_the_peak_falls() {
        let start = Instant::now();
        let mut bar = Bar::default();
        bar.take(level(-0.5, -3.5), start);
        assert!(!bar.clipped, "just under full scale");
        bar.take(level(0.0, -3.0), start);
        bar.take(level(-24.0, -27.0), start + HOLD * 4);
        assert!(bar.clipped);
    }

    /// An engine answering every `GetMeters` with whatever the test last set.
    #[derive(Debug, Clone, Default)]
    struct Canned {
        report: Arc<Mutex<MetersReport>>,
        /// Whether the editor would have a channel to it.
        down: Arc<AtomicBool>,
        /// Whether its output frames stand still.
        stopped: Arc<AtomicBool>,
    }

    impl Canned {
        fn new(outputs: Vec<ChannelLevel>, inputs: Vec<ChannelLevel>) -> Self {
            let canned = Self::default();
            canned.set(outputs, inputs);
            canned
        }

        /// Answer with these levels from now on.
        fn set(&self, outputs: Vec<ChannelLevel>, inputs: Vec<ChannelLevel>) {
            let mut report = self.report.lock().expect("unpoisoned");
            report.outputs = outputs;
            report.inputs = inputs;
        }

        fn up(&self) -> bool {
            !self.down.load(Ordering::SeqCst)
        }

        fn channel(&self) -> Channel {
            Channel::new(self.clone())
        }
    }

    impl Transport for Canned {
        fn round_trip(&self, _line: &str, _read_timeout: Duration) -> std::io::Result<String> {
            let mut report = self.report.lock().expect("unpoisoned");
            if !self.stopped.load(Ordering::SeqCst) {
                report.frames += 256;
            }
            Ok(Response::Meters(report.clone()).to_ndjson())
        }

        fn endpoint(&self) -> &str {
            "canned"
        }
    }

    fn harness(engine: Option<Canned>) -> Harness<'static, MeterStrip> {
        Harness::new_ui_state(
            move |ui, strip: &mut MeterStrip| {
                let channel = engine.as_ref().filter(|e| e.up()).map(Canned::channel);
                strip.poll(Instant::now(), channel);
                show(ui, strip);
            },
            MeterStrip::new(|| {}),
        )
    }

    /// Step until the node labelled `label` reads `want`, failing after a few seconds.
    fn until_value(harness: &mut Harness<'_, MeterStrip>, label: &str, want: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.step();
            let value = harness.query_by_label(label).and_then(|node| node.value());
            if value.as_deref() == Some(want) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{label} never read {want:?}: {value:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_playing_channel_shows_its_level_and_a_silent_one_the_floor() {
        let engine = Canned::new(vec![level(-6.0, -9.0), SILENT], vec![level(-18.0, -24.0)]);
        let mut harness = harness(Some(engine));
        until_value(&mut harness, "Out 1 level", "peak -6.0 dBFS, RMS -9.0 dBFS");
        let floor = "peak below -60 dBFS, RMS below -60 dBFS";
        assert_eq!(
            harness.get_by_label("Out 2 level").value().as_deref(),
            Some(floor)
        );
        assert_eq!(
            harness.get_by_label("In 1 level").value().as_deref(),
            Some("peak -18.0 dBFS, RMS -24.0 dBFS")
        );

        // Upright, and side by side: outputs first, then inputs.
        let out_1 = harness.get_by_label("Out 1 level").rect();
        let out_2 = harness.get_by_label("Out 2 level").rect();
        let in_1 = harness.get_by_label("In 1 level").rect();
        assert!(out_1.height() > 4.0 * out_1.width(), "{out_1:?}");
        assert_eq!(out_1.top(), out_2.top());
        assert_eq!(out_1.top(), in_1.top());
        assert!(out_1.right() < out_2.left(), "{out_1:?} {out_2:?}");
        assert!(out_2.right() < in_1.left(), "{out_2:?} {in_1:?}");
    }

    #[test]
    fn a_clip_stays_lit_until_clicked() {
        let engine = Canned::new(vec![level(1.5, -2.0)], Vec::new());
        let mut harness = harness(Some(engine.clone()));
        until_value(&mut harness, "Out 1 level", "peak 1.5 dBFS, RMS -2.0 dBFS");
        engine.set(vec![level(-12.0, -15.0)], Vec::new());
        until_value(
            &mut harness,
            "Out 1 level",
            "peak -12.0 dBFS, RMS -15.0 dBFS",
        );
        assert!(
            harness.state().outputs[0].clipped,
            "lit after the peak fell"
        );
        harness.get_by_label("Out 1 clip").click();
        harness.step();
        assert!(!harness.state().outputs[0].clipped, "the click put it out");
    }

    #[test]
    fn an_output_counts_as_stopped_only_once_its_frames_stand_still_for_the_stall() {
        let start = Instant::now();
        let report = |frames| MetersReport {
            outputs: vec![level(-6.0, -9.0)],
            inputs: Vec::new(),
            frames,
        };
        let mut strip = MeterStrip::new(|| {});
        strip.take(report(256), start);
        // Two polls inside one long device period read the same count.
        strip.take(report(256), start + EVERY);
        assert!(!strip.stopped, "one period is not a stop");
        strip.take(report(4096), start + EVERY * 2);
        strip.take(report(4096), start + EVERY * 2 + STALL / 2);
        assert!(!strip.stopped);
        strip.take(report(4096), start + EVERY * 2 + STALL);
        assert!(strip.stopped, "still for the whole stall");
        strip.take(report(4352), start + EVERY * 3 + STALL);
        assert!(!strip.stopped, "moving again");
    }

    /// Step until the strip shows the text `label`, failing after a few seconds.
    fn until_shown(harness: &mut Harness<'_, MeterStrip>, label: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while harness.query_by_label(label).is_none() {
            assert!(Instant::now() < deadline, "never showed {label:?}");
            harness.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_engine_that_goes_away_leaves_no_levels_behind() {
        let engine = Canned::new(vec![level(-6.0, -9.0)], Vec::new());
        let mut harness = harness(Some(engine.clone()));
        until_value(&mut harness, "Out 1 level", "peak -6.0 dBFS, RMS -9.0 dBFS");
        engine.down.store(true, Ordering::SeqCst);
        until_shown(&mut harness, "Meters: no engine to read");
        assert!(harness.query_by_label("Out 1 level").is_none());
        engine.down.store(false, Ordering::SeqCst);
        until_value(&mut harness, "Out 1 level", "peak -6.0 dBFS, RMS -9.0 dBFS");
    }

    #[test]
    fn an_output_that_stops_says_so_and_one_that_restarts_reads_live_again() {
        let engine = Canned::new(vec![level(-6.0, -9.0)], Vec::new());
        let mut harness = harness(Some(engine.clone()));
        until_value(&mut harness, "Out 1 level", "peak -6.0 dBFS, RMS -9.0 dBFS");
        assert!(harness.query_by_label("Output stopped").is_none());
        engine.stopped.store(true, Ordering::SeqCst);
        until_shown(&mut harness, "Output stopped");
        engine.stopped.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while harness.query_by_label("Output stopped").is_some() {
            assert!(Instant::now() < deadline, "never read live again");
            harness.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn with_no_engine_the_strip_says_so() {
        let mut harness = harness(None);
        harness.step();
        harness.get_by_label("Meters: no engine to read");
    }
}
