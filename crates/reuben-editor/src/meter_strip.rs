//! The master meter strip: a bar per device output channel, and per input channel when the
//! engine opened an input stream, read from the engine's `GetMeters` over the structure channel —
//! the same way whether the editor started the engine or attached to one.
//!
//! The engine does the ballistics: its peak falls back at a fixed rate and its RMS is averaged, so
//! a poll at any rate reads the same values. The strip adds what only a display needs: a held peak
//! marker, and a clip lamp that stays lit until clicked.
//!
//! The exchange runs on a thread of its own, so an attached engine that stops answering holds that
//! thread for a read budget, never the window.

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
/// moving smoothly, and no faster than the window would draw.
const EVERY: Duration = Duration::from_millis(33);
/// How often it is asked while no output frames move, as on an engine with no audio device:
/// nothing changes until they do, so this only bounds how late the strip notices them start.
const IDLE_EVERY: Duration = Duration::from_millis(500);

const BAR: Vec2 = Vec2::new(140.0, 10.0);
const LAMP: Vec2 = Vec2::new(10.0, 10.0);

/// What the last exchange found.
#[derive(Debug, Clone, PartialEq)]
enum Reading {
    /// No engine is running, or the attached one stopped answering its probe.
    NoEngine,
    /// The engine did not answer `GetMeters`, and why.
    Unanswered(String),
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
        if level.peak_dbfs >= self.held || self.held_until.is_none_or(|until| now >= until) {
            self.held = level.peak_dbfs;
            self.held_until = Some(now + HOLD);
        }
        self.clipped |= level.peak_dbfs >= CLIP_DBFS;
    }
}

/// The strip's state, and the thread that asks the engine for its levels.
pub struct MeterStrip {
    asks: Sender<Channel>,
    replies: Receiver<Result<MetersReport, String>>,
    asked: Option<Instant>,
    in_flight: bool,
    reading: Reading,
    frames: Option<u64>,
    moving: bool,
    outputs: Vec<Bar>,
    inputs: Vec<Bar>,
}

impl MeterStrip {
    /// A strip with nothing read yet. `wake` is called from the asking thread on each reply.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (asks, asked) = mpsc::channel::<Channel>();
        let (replied, replies) = mpsc::channel();
        // Not joined: it ends once the strip is dropped and its exchange in hand returns.
        std::thread::spawn(move || {
            for channel in asked {
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
            moving: false,
            outputs: Vec::new(),
            inputs: Vec::new(),
        }
    }

    /// Take in any reply, and ask the engine behind `channel` again when it is time. Returns how
    /// soon to look again; `None` while there is no engine, or while a reply is awaited, which
    /// wakes the window itself. Never waits.
    pub fn poll(&mut self, now: Instant, channel: Option<Channel>) -> Option<Duration> {
        for reply in self.replies.try_iter().collect::<Vec<_>>() {
            self.in_flight = false;
            match reply {
                Ok(report) => self.take(report, now),
                Err(why) => self.reading = Reading::Unanswered(why),
            }
        }
        let Some(channel) = channel else {
            self.reading = Reading::NoEngine;
            return None;
        };
        if self.in_flight {
            return None;
        }
        let every = if self.moving { EVERY } else { IDLE_EVERY };
        let due = self.asked.map_or(now, |asked| asked + every);
        if now < due {
            return Some(due - now);
        }
        if self.asks.send(channel).is_err() {
            self.reading = Reading::Unanswered("the meter thread stopped".to_string());
            return None;
        }
        self.in_flight = true;
        self.asked = Some(now);
        None
    }

    fn take(&mut self, report: MetersReport, now: Instant) {
        // The first reply has nothing to compare with: ask again at the running rate to find out.
        self.moving = self.frames.is_none_or(|frames| frames != report.frames);
        self.frames = Some(report.frames);
        self.reading = Reading::Levels;
        for (bars, levels) in [
            (&mut self.outputs, report.outputs),
            (&mut self.inputs, report.inputs),
        ] {
            // A restart onto another device changes the count; what was held belonged to the old.
            if bars.len() != levels.len() {
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

/// Draw the strip in one row.
pub fn show(ui: &mut egui::Ui, strip: &mut MeterStrip) {
    ui.horizontal_wrapped(|ui| match &strip.reading {
        Reading::NoEngine => {
            ui.weak("Meters: no engine running");
        }
        Reading::Unanswered(why) => {
            ui.weak("Meters: the engine did not answer")
                .on_hover_text(why);
        }
        Reading::Levels if strip.outputs.is_empty() && strip.inputs.is_empty() => {
            ui.weak("Meters: the engine has no audio device");
        }
        Reading::Levels => {
            for (i, bar) in strip.outputs.iter_mut().enumerate() {
                channel(ui, &format!("Out {}", i + 1), bar);
            }
            if !strip.inputs.is_empty() {
                ui.separator();
            }
            for (i, bar) in strip.inputs.iter_mut().enumerate() {
                channel(ui, &format!("In {}", i + 1), bar);
            }
        }
    });
}

/// One channel: its name, its bar, and its clip lamp, which a click puts out.
fn channel(ui: &mut egui::Ui, name: &str, bar: &mut Bar) {
    ui.label(name);
    let described = format!(
        "peak {}, RMS {}",
        db(bar.level.peak_dbfs),
        db(bar.level.rms_dbfs)
    );
    let (rect, response) = ui.allocate_exact_size(BAR, Sense::hover());
    response.widget_info(|| WidgetInfo {
        current_text_value: Some(described.clone()),
        ..WidgetInfo::labeled(WidgetType::ProgressIndicator, true, format!("{name} level"))
    });
    response.on_hover_text(&described);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let up_to = |dbfs: f32| {
            let mut filled = rect;
            filled.set_right(rect.left() + rect.width() * fraction(dbfs));
            filled
        };
        painter.rect_filled(
            up_to(bar.level.peak_dbfs),
            0.0,
            theme::METER.gamma_multiply(0.4),
        );
        painter.rect_filled(up_to(bar.level.rms_dbfs), 0.0, theme::METER);
        if bar.held > SCALE_FLOOR_DBFS {
            let x = rect.left() + rect.width() * fraction(bar.held);
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(2.0, ui.visuals().strong_text_color()),
            );
        }
        painter.rect_stroke(
            rect,
            0.0,
            ui.visuals().widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }

    let (lamp, clicked) = ui.allocate_exact_size(LAMP, Sense::click());
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
    ui.painter().rect_filled(lamp, 1.0, fill);
    ui.painter().rect_stroke(
        lamp,
        1.0,
        ui.visuals().widgets.noninteractive.fg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
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
        bar.take(level(-20.0, -23.0), start + HOLD / 2 + HOLD);
        assert_eq!(bar.held, -20.0, "the hold ran out");
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
    #[derive(Debug, Clone)]
    struct Canned(Arc<Mutex<MetersReport>>);

    impl Canned {
        fn new(outputs: Vec<ChannelLevel>, inputs: Vec<ChannelLevel>) -> Self {
            let canned = Self(Arc::new(Mutex::new(MetersReport::default())));
            canned.set(outputs, inputs);
            canned
        }

        /// Answer with these levels from now on, as an engine whose output keeps running.
        fn set(&self, outputs: Vec<ChannelLevel>, inputs: Vec<ChannelLevel>) {
            let mut report = self.0.lock().expect("unpoisoned");
            report.outputs = outputs;
            report.inputs = inputs;
        }

        fn channel(&self) -> Channel {
            Channel::new(self.clone())
        }
    }

    impl Transport for Canned {
        fn round_trip(&self, _line: &str, _read_timeout: Duration) -> std::io::Result<String> {
            let mut report = self.0.lock().expect("unpoisoned");
            report.frames += 256;
            Ok(Response::Meters(report.clone()).to_ndjson())
        }

        fn endpoint(&self) -> &str {
            "canned"
        }
    }

    fn harness(engine: Option<Canned>) -> Harness<'static, MeterStrip> {
        Harness::new_ui_state(
            move |ui, strip: &mut MeterStrip| {
                strip.poll(Instant::now(), engine.as_ref().map(Canned::channel));
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
    fn with_no_engine_the_strip_says_so() {
        let mut harness = harness(None);
        harness.step();
        harness.get_by_label("Meters: no engine running");
    }
}
