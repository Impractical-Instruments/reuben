//! The audio output choice: which device the embedded engine plays on, and the sample rate and
//! buffer size it asks that device for. Stored per user in `audio.json` under the
//! [`ConfigDir`], and applied to the [`DeviceProfile`] the engine starts from.
//!
//! An `--io-map` on the command line decides the whole profile for that run: the stored choice is
//! neither applied nor rewritten until the user picks something in the window.
//!
//! A stored device is named by its whole name, which `reuben-native` selects ahead of any device
//! whose name merely contains it. One that is not connected at launch is not handed to the
//! engine at all — a name that selects nothing fails the start, and one that is contained in
//! another device's name would silently select that other device — so the engine plays on the
//! default device, with a warning, and the choice stays stored for when the device is back.

use std::path::PathBuf;

use eframe::egui;
use reuben_native::audio::AudioError;
use reuben_native::cli::{EngineFlags, FlagsError};
use reuben_native::devices::{self, OutputDevice};
use reuben_native::engine::{self, EngineConfig, Instrument, RunningEngine, StartError};
use reuben_native::profile::DeviceProfile;
use serde::{Deserialize, Serialize};

use crate::config::ConfigDir;
use crate::engine::Engine;

const FILE: &str = "audio.json";

/// The rates offered, where the device grants them. A device's supported rates are ranges, often
/// 1 Hz to several hundred kHz, so the menu is the rates audio hardware actually runs at.
const SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// The buffer sizes offered, where the device grants them: the powers of two a device period is
/// conventionally set to.
const BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

type ListDevices = fn() -> Result<Vec<OutputDevice>, AudioError>;

/// What the user chose. Every field `None` is the default device at its own rate and buffer size.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioChoice {
    /// The whole name of an output device, as [`devices::output_devices`] listed it.
    pub output_device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
}

/// The stored [`AudioChoice`], the devices it can be made from, and the bar that edits it.
pub struct AudioSettings {
    dir: ConfigDir,
    list: ListDevices,
    choice: AudioChoice,
    devices: Vec<OutputDevice>,
    /// The `--io-map` file deciding this run's profile, until the user picks in the window.
    io_map: Option<PathBuf>,
    /// The stored device was not connected when the choice was last applied, so the engine is on
    /// the default device instead.
    fell_back: bool,
    warnings: Vec<String>,
}

impl AudioSettings {
    /// Read the stored choice from `dir` and list the output devices. Neither can fail: an
    /// unreadable file is the default choice and a failed listing is no devices, each with a
    /// warning the bar shows.
    pub fn load(dir: ConfigDir) -> Self {
        Self::load_with(dir, devices::output_devices)
    }

    /// [`load`](Self::load) listing through `list`, the seam a test names its own devices with.
    pub(crate) fn load_with(dir: ConfigDir, list: ListDevices) -> Self {
        let (stored, warning) = dir.load::<AudioChoice>(FILE);
        let mut settings = Self {
            dir,
            list,
            // A zero is not a rate or a size the engine can ask for; `DeviceProfile::load`
            // rejects one in an `--io-map` file for the same reason.
            choice: AudioChoice {
                sample_rate: stored.sample_rate.filter(|&hz| hz > 0),
                buffer_size: stored.buffer_size.filter(|&frames| frames > 0),
                ..stored
            },
            devices: Vec::new(),
            io_map: None,
            fell_back: false,
            warnings: Vec::new(),
        };
        if let Some(warning) = warning {
            settings.warn(warning);
        }
        settings.refresh();
        settings
    }

    /// The config `flags` describe, on the stored choice — unless `flags` name an `--io-map`,
    /// whose profile then stands as it is.
    pub fn engine_config(
        &mut self,
        flags: &EngineFlags,
        instrument: Instrument,
        instrument_root: Option<PathBuf>,
    ) -> Result<EngineConfig, FlagsError> {
        let mut config = flags.config(instrument, instrument_root)?;
        match &flags.io_map {
            Some(path) => self.io_map = Some(path.clone()),
            None => self.apply(&mut config.profile),
        }
        Ok(config)
    }

    /// Store `choice` and restart `engine` on it. With no engine to restart the choice is still
    /// stored, for the next launch.
    pub fn select(&mut self, choice: AudioChoice, engine: &mut Engine) {
        self.select_with(choice, engine, engine::start);
    }

    /// [`select`](Self::select) restarting through `start`, the seam a test puts the headless
    /// engine in.
    pub(crate) fn select_with(
        &mut self,
        choice: AudioChoice,
        engine: &mut Engine,
        start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
    ) {
        self.warnings.clear();
        self.io_map = None;
        self.choice = choice;
        if let Err(e) = self.dir.save(FILE, &self.choice) {
            let place = self.dir.path(FILE).unwrap_or_else(|| PathBuf::from(FILE));
            self.warn(format!(
                "the audio choice was not saved to {}: {e}",
                place.display()
            ));
        }
        let Some(mut profile) = engine.profile().cloned() else {
            return;
        };
        self.apply(&mut profile);
        engine.restart_with(profile, start);
    }

    /// Write the stored choice over `profile`'s output device, sample rate and buffer size,
    /// leaving its channel maps and input side alone.
    fn apply(&mut self, profile: &mut DeviceProfile) {
        let connected = match &self.choice.output_device {
            Some(name) => self.devices.iter().any(|d| &d.name == name),
            None => true,
        };
        self.fell_back = !connected;
        if connected {
            profile.output.device = self.choice.output_device.clone();
            profile.sample_rate = self.choice.sample_rate;
            profile.buffer_size = self.choice.buffer_size;
        } else {
            // The stored rate and buffer size were chosen for the device that is gone.
            profile.output.device = None;
            profile.sample_rate = None;
            profile.buffer_size = None;
            let name = self.choice.output_device.clone().unwrap_or_default();
            self.warn(format!(
                "output device \"{name}\" is not connected; playing on the default device"
            ));
        }
    }

    fn refresh(&mut self) {
        match (self.list)() {
            Ok(devices) => self.devices = devices,
            Err(e) => {
                self.devices.clear();
                self.warn(format!("the output devices could not be listed: {e}"));
            }
        }
    }

    /// List the devices again, and move `engine` onto the stored device if it was missing when
    /// the choice was last applied and is listed now.
    fn refresh_with(
        &mut self,
        engine: &mut Engine,
        start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
    ) {
        self.refresh();
        let back = self.fell_back
            && self.io_map.is_none()
            && self
                .choice
                .output_device
                .as_ref()
                .is_some_and(|name| self.devices.iter().any(|d| &d.name == name));
        if back {
            self.select_with(self.choice.clone(), engine, start);
        }
    }

    fn warn(&mut self, text: String) {
        eprintln!("warning: {text}");
        self.warnings.push(text);
    }

    /// What the menus show as chosen: the stored choice, or the `--io-map` profile's own fields
    /// while it decides this run.
    fn shown(&self, engine: &Engine) -> AudioChoice {
        match (&self.io_map, engine.profile()) {
            (Some(_), Some(profile)) => AudioChoice {
                // The whole name of the device the profile's string selects, which is what a
                // choice stores: the string itself may be any part of that name, in any case.
                output_device: profile.output.device.as_ref().map(|name| {
                    devices::selected(&self.devices, name)
                        .map_or_else(|| name.clone(), |device| device.name.clone())
                }),
                sample_rate: profile.sample_rate,
                buffer_size: profile.buffer_size,
            },
            _ => self.choice.clone(),
        }
    }

    /// The listed device `name` is, where `None` is the default device.
    fn device(&self, name: Option<&str>) -> Option<&OutputDevice> {
        self.devices.iter().find(|d| match name {
            Some(name) => d.name == name,
            None => d.is_default,
        })
    }

    /// The bar: one menu each for the device, the sample rate and the buffer size. Picking in any
    /// of them stores the choice and restarts `engine` on it.
    pub fn ui(&mut self, ui: &mut egui::Ui, engine: &mut Engine) {
        let shown = self.shown(engine);
        let mut choice = shown.clone();
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Output");
            egui::ComboBox::from_id_salt("audio_output_device")
                .selected_text(self.device_label(shown.output_device.as_deref()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice.output_device, None, self.device_label(None));
                    for device in &self.devices {
                        ui.selectable_value(
                            &mut choice.output_device,
                            Some(device.name.clone()),
                            device.name.as_str(),
                        );
                    }
                });
            if choice.output_device != shown.output_device {
                let device = self.device(choice.output_device.as_deref());
                choice.sample_rate = choice
                    .sample_rate
                    .filter(|&hz| device.is_none_or(|d| d.supports_sample_rate(hz)));
                choice.buffer_size = choice
                    .buffer_size
                    .filter(|&frames| device.is_none_or(|d| d.supports_buffer_size(frames)));
            }
            let device = self.device(choice.output_device.as_deref());

            ui.label("Sample rate");
            let rates = offered(&SAMPLE_RATES, shown.sample_rate, |hz| {
                device.is_none_or(|d| d.supports_sample_rate(hz))
            });
            let default_rate = match device.and_then(|d| d.default_sample_rate) {
                Some(hz) => format!("Device default ({hz} Hz)"),
                None => "Device default".to_string(),
            };
            preference_menu(
                ui,
                "audio_sample_rate",
                &mut choice.sample_rate,
                &rates,
                &default_rate,
                |hz| format!("{hz} Hz"),
            );

            ui.label("Buffer size");
            let sizes = offered(&BUFFER_SIZES, shown.buffer_size, |frames| {
                device.is_none_or(|d| d.supports_buffer_size(frames))
            });
            preference_menu(
                ui,
                "audio_buffer_size",
                &mut choice.buffer_size,
                &sizes,
                "Device default",
                |frames| format!("{frames} frames"),
            );

            refresh = ui
                .button("Refresh")
                .on_hover_text("List the output devices again")
                .clicked();
            if let Some(path) = &self.io_map {
                ui.label(format!(
                    "--io-map {} decides this run; choosing here replaces it",
                    path.display()
                ));
            }
        });
        for warning in &self.warnings {
            ui.colored_label(ui.visuals().warn_fg_color, format!("warning: {warning}"));
        }
        if refresh {
            self.refresh_with(engine, engine::start);
        }
        if choice != shown {
            self.select(choice, engine);
        }
    }

    fn device_label(&self, name: Option<&str>) -> String {
        match name {
            None => match self.device(None) {
                Some(default) => format!("Default device ({})", default.name),
                None => "Default device".to_string(),
            },
            Some(name) if self.device(Some(name)).is_some() || self.io_map.is_some() => {
                name.to_string()
            }
            Some(name) => format!("{name} (not connected)"),
        }
    }
}

/// The `standard` values the device `supports`, plus `chosen` when it is not among them, so the
/// menu always has an entry for what it shows as selected.
fn offered(standard: &[u32], chosen: Option<u32>, supports: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut values: Vec<u32> = standard.iter().copied().filter(|&v| supports(v)).collect();
    if let Some(chosen) = chosen {
        if !values.contains(&chosen) {
            values.push(chosen);
            values.sort_unstable();
        }
    }
    values
}

fn preference_menu(
    ui: &mut egui::Ui,
    id: &str,
    chosen: &mut Option<u32>,
    values: &[u32],
    default: &str,
    label: impl Fn(u32) -> String,
) {
    let selected = match *chosen {
        Some(value) => label(value),
        None => default.to_string(),
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(chosen, None, default);
            for &value in values {
                ui.selectable_value(chosen, Some(value), label(value));
            }
        });
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::UdpSocket;

    use reuben_api::engine::{get_current_instrument, DocSource};
    use reuben_api::FsResolver;
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::config::test_support::TempConfig;
    use crate::engine::test_support::{config, library, HANG_SECS};
    use crate::engine::Level;

    fn device(name: &str, is_default: bool) -> OutputDevice {
        OutputDevice {
            name: name.to_string(),
            is_default,
            default_sample_rate: Some(48_000),
            sample_rates: std::iter::once(44_100..=96_000).collect(),
            buffer_sizes: Some(64..=2048),
        }
    }

    /// "USB Audio" is contained in the name listed before it, so only whole-name selection
    /// tells the two apart.
    pub(crate) fn two_devices() -> Result<Vec<OutputDevice>, AudioError> {
        Ok(vec![
            device("Built-in", true),
            device("USB Audio Pro", false),
            device("USB Audio", false),
        ])
    }

    fn usb_audio_unplugged() -> Result<Vec<OutputDevice>, AudioError> {
        Ok(vec![
            device("Built-in", true),
            device("USB Audio Pro", false),
        ])
    }

    fn no_listing() -> Result<Vec<OutputDevice>, AudioError> {
        Err(AudioError::NoDevice)
    }

    fn usb_audio() -> AudioChoice {
        AudioChoice {
            output_device: Some("USB Audio".to_string()),
            sample_rate: Some(96_000),
            buffer_size: Some(128),
        }
    }

    fn launch_profile(settings: &mut AudioSettings, flags: &EngineFlags) -> DeviceProfile {
        settings
            .engine_config(flags, Instrument::Default, None)
            .expect("a config")
            .profile
    }

    /// The content hash and source of what `engine` is playing.
    fn playing(engine: &Engine) -> (String, Option<String>) {
        let channel = engine.running().expect("running").channel();
        let store = |_: Option<&str>| FsResolver::new(library(""));
        let now = get_current_instrument(&channel, store).expect("the engine answers");
        (now.output.content_hash, now.output.source)
    }

    #[test]
    fn nothing_stored_is_the_default_profile() {
        let temp = TempConfig::new();
        let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(
            launch_profile(&mut settings, &EngineFlags::default()),
            DeviceProfile::default()
        );
        assert!(settings.warnings.is_empty(), "{:?}", settings.warnings);
    }

    #[test]
    fn a_choice_is_stored_and_used_on_the_next_launch() {
        let temp = TempConfig::new();
        let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
        settings.select(usb_audio(), &mut Engine::idle());
        assert!(settings.warnings.is_empty(), "{:?}", settings.warnings);

        let mut next = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(next.choice, usb_audio());
        let profile = launch_profile(&mut next, &EngineFlags::default());
        assert_eq!(profile.output.device.as_deref(), Some("USB Audio"));
        assert_eq!(profile.sample_rate, Some(96_000));
        assert_eq!(profile.buffer_size, Some(128));
        assert!(next.warnings.is_empty(), "{:?}", next.warnings);
    }

    #[test]
    fn a_corrupt_choice_file_is_the_default_profile_and_a_warning() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).expect("mkdir");
        std::fs::write(temp.path.join(FILE), "{ not json").expect("seed");
        let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(
            launch_profile(&mut settings, &EngineFlags::default()),
            DeviceProfile::default()
        );
        assert_eq!(settings.warnings.len(), 1, "{:?}", settings.warnings);
        assert!(
            settings.warnings[0].contains(FILE),
            "{:?}",
            settings.warnings
        );
    }

    #[test]
    fn a_stored_zero_rate_or_buffer_size_is_no_preference() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).expect("mkdir");
        std::fs::write(
            temp.path.join(FILE),
            r#"{"output_device":"USB Audio","sample_rate":0,"buffer_size":0}"#,
        )
        .expect("seed");
        let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
        let profile = launch_profile(&mut settings, &EngineFlags::default());
        assert_eq!(profile.output.device.as_deref(), Some("USB Audio"));
        assert_eq!(profile.sample_rate, None);
        assert_eq!(profile.buffer_size, None);
    }

    #[test]
    fn an_io_map_overrides_the_stored_choice_for_that_run_only() {
        let temp = TempConfig::new();
        AudioSettings::load_with(temp.dir(), two_devices).select(usb_audio(), &mut Engine::idle());
        let io_map = temp.path.join("rig.io-map.json");
        std::fs::write(
            &io_map,
            r#"{"output":{"device":"Built-in","map":{"0":1}},"sample_rate":44100}"#,
        )
        .expect("seed");
        let flags = EngineFlags {
            io_map: Some(io_map.clone()),
            ..EngineFlags::default()
        };

        let mut overridden = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(
            launch_profile(&mut overridden, &flags),
            DeviceProfile::load(&io_map).expect("the profile loads")
        );

        let mut next = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(
            launch_profile(&mut next, &EngineFlags::default())
                .output
                .device
                .as_deref(),
            Some("USB Audio")
        );
    }

    #[test]
    fn a_stored_device_that_is_gone_plays_on_the_default_device_with_a_warning() {
        let temp = TempConfig::new();
        AudioSettings::load_with(temp.dir(), two_devices).select(usb_audio(), &mut Engine::idle());

        for list in [usb_audio_unplugged as ListDevices, no_listing] {
            let mut settings = AudioSettings::load_with(temp.dir(), list);
            assert_eq!(
                launch_profile(&mut settings, &EngineFlags::default()),
                DeviceProfile::default()
            );
            let warning = settings.warnings.last().expect("a warning");
            assert!(warning.contains("\"USB Audio\""), "{warning}");
            assert!(warning.contains("default device"), "{warning}");
        }

        let mut replugged = AudioSettings::load_with(temp.dir(), two_devices);
        assert_eq!(
            launch_profile(&mut replugged, &EngineFlags::default())
                .output
                .device
                .as_deref(),
            Some("USB Audio")
        );
    }

    #[test]
    fn choosing_in_the_window_replaces_the_io_map_device_and_keeps_its_channel_map() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let io_map = temp.path.join("rig.io-map.json");
            std::fs::create_dir_all(&temp.path).expect("mkdir");
            std::fs::write(&io_map, r#"{"output":{"device":"Built-in","map":{"0":1}}}"#)
                .expect("seed");
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let flags = EngineFlags {
                io_map: Some(io_map),
                ..EngineFlags::default()
            };
            let from_flags = settings
                .engine_config(&flags, Instrument::Path(library("acid-techno.json")), None)
                .expect("a config");
            let mut engine = Engine::start_with(
                EngineConfig {
                    profile: from_flags.profile,
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            assert_eq!(
                settings.shown(&engine).output_device.as_deref(),
                Some("Built-in")
            );

            settings.select_with(usb_audio(), &mut engine, start_headless);
            let profile = engine.profile().expect("a profile");
            assert_eq!(profile.output.device.as_deref(), Some("USB Audio"));
            assert_eq!(profile.output.map.get(&0), Some(&1));
            assert_eq!(settings.shown(&engine), usb_audio());
            engine.shutdown();
        });
    }

    #[test]
    fn a_switch_restarts_the_engine_on_the_same_document_with_the_new_profile() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            assert_eq!(engine.profile(), Some(&DeviceProfile::default()));
            let before = playing(&engine);

            let mut started_with = None;
            settings.select_with(usb_audio(), &mut engine, |config| {
                started_with = Some(config.profile.clone());
                start_headless(config)
            });

            let mut expected = DeviceProfile {
                sample_rate: Some(96_000),
                buffer_size: Some(128),
                ..DeviceProfile::default()
            };
            expected.output.device = Some("USB Audio".to_string());
            assert_eq!(started_with.as_ref(), Some(&expected));
            assert_eq!(engine.profile(), Some(&expected));
            assert_eq!(playing(&engine), before);
            assert!(
                engine.report().iter().all(|l| l.level != Level::Error),
                "{:?}",
                engine.report()
            );
            assert_eq!(
                AudioSettings::load_with(temp.dir(), two_devices).choice,
                usb_audio()
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_switch_whose_restart_cannot_rebind_a_port_runs_without_it_says_why_and_the_next_recovers()
    {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let probe = UdpSocket::bind("127.0.0.1:0").expect("find a free port");
            let addr = probe.local_addr().expect("bound").to_string();
            drop(probe);
            let mut engine = Engine::start_with(
                EngineConfig {
                    osc_in: Some(addr.clone()),
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            let before = playing(&engine);

            // The seam runs after the old engine has freed the port, so this is another
            // process taking it in the gap.
            let mut squatter = None;
            settings.select_with(usb_audio(), &mut engine, |config| {
                squatter.get_or_insert_with(|| {
                    UdpSocket::bind(addr.as_str()).expect("the port was freed")
                });
                start_headless(config)
            });
            let running = engine.running().expect("the server runs without OSC-in");
            assert_eq!(running.osc_in_addr(), None);
            assert_eq!(playing(&engine), before);
            let errors: Vec<_> = engine
                .report()
                .iter()
                .filter(|l| l.level == Level::Error)
                .collect();
            assert_eq!(errors.len(), 1, "{:?}", engine.report());
            assert!(errors[0].text.contains("OSC-in"), "{}", errors[0].text);
            assert!(errors[0].text.contains(&addr), "{}", errors[0].text);

            drop(squatter);
            settings.select_with(AudioChoice::default(), &mut engine, start_headless);
            assert_eq!(engine.profile(), Some(&DeviceProfile::default()));
            assert_eq!(playing(&engine), before);
            assert_eq!(
                (engine.running()).and_then(|running| running.osc_in_addr()),
                Some(addr.parse().unwrap()),
                "OSC-in is asked for again"
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_pick_under_an_io_map_naming_part_of_a_device_keeps_that_device() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let io_map = temp.path.join("rig.io-map.json");
            std::fs::create_dir_all(&temp.path).expect("mkdir");
            std::fs::write(&io_map, r#"{"output":{"device":"pro"}}"#).expect("seed");
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let flags = EngineFlags {
                io_map: Some(io_map),
                ..EngineFlags::default()
            };
            let from_flags = settings
                .engine_config(&flags, Instrument::Path(library("acid-techno.json")), None)
                .expect("a config");
            let mut engine = Engine::start_with(
                EngineConfig {
                    profile: from_flags.profile,
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            let shown = settings.shown(&engine);
            assert_eq!(shown.output_device.as_deref(), Some("USB Audio Pro"));

            // What the bar does when only the sample rate menu changes.
            let picked = AudioChoice {
                sample_rate: Some(48_000),
                ..shown
            };
            settings.select_with(picked.clone(), &mut engine, start_headless);

            let profile = engine.profile().expect("a profile");
            assert_eq!(profile.output.device.as_deref(), Some("USB Audio Pro"));
            assert_eq!(profile.sample_rate, Some(48_000));
            assert!(settings.warnings.is_empty(), "{:?}", settings.warnings);
            assert_eq!(
                AudioSettings::load_with(temp.dir(), two_devices).choice,
                picked
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_switch_leaves_the_engine_running_when_its_instrument_file_is_gone() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            std::fs::create_dir_all(&temp.path).expect("mkdir");
            let instrument = temp.path.join("breath-harp.json");
            std::fs::copy(library("breath-harp.json"), &instrument).expect("copy");
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let mut engine = Engine::start_with(config(instrument.clone()), start_headless);
            let before = playing(&engine);
            std::fs::remove_file(&instrument).expect("delete");

            settings.select_with(usb_audio(), &mut engine, start_headless);

            assert_eq!(playing(&engine), before);
            assert_eq!(engine.profile(), Some(&DeviceProfile::default()));
            let last = engine.report().last().expect("a line");
            assert_eq!(last.level, Level::Error);
            assert!(last.text.contains("not changed"), "{}", last.text);
            engine.shutdown();
        });
    }

    #[test]
    fn a_switch_while_playing_a_document_with_no_file_says_what_it_went_back_to() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut settings = AudioSettings::load_with(temp.dir(), two_devices);
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            let launched = playing(&engine);
            let by_value = serde_json::from_str(
                &std::fs::read_to_string(library("breath-harp.json")).expect("read"),
            )
            .expect("a document");
            engine
                .running()
                .expect("running")
                .channel()
                .swap(DocSource::Document(by_value), None)
                .expect("the engine answers");
            assert_ne!(playing(&engine), launched);

            settings.select_with(usb_audio(), &mut engine, start_headless);

            assert_eq!(playing(&engine), launched);
            let warnings: Vec<_> = engine
                .report()
                .iter()
                .filter(|l| l.level == Level::Warning)
                .collect();
            assert!(
                warnings.iter().any(|l| l.text.contains("without a file")),
                "{:?}",
                engine.report()
            );
            engine.shutdown();
        });
    }

    #[test]
    fn refreshing_after_a_missing_device_comes_back_moves_the_engine_onto_it() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            AudioSettings::load_with(temp.dir(), two_devices)
                .select(usb_audio(), &mut Engine::idle());
            let mut settings = AudioSettings::load_with(temp.dir(), usb_audio_unplugged);
            let launch = settings
                .engine_config(
                    &EngineFlags::default(),
                    Instrument::Path(library("acid-techno.json")),
                    None,
                )
                .expect("a config");
            let mut engine = Engine::start_with(
                EngineConfig {
                    profile: launch.profile,
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            assert_eq!(engine.profile(), Some(&DeviceProfile::default()));
            assert!(!settings.warnings.is_empty());

            settings.list = two_devices;
            settings.refresh_with(&mut engine, start_headless);

            assert_eq!(
                engine
                    .profile()
                    .expect("a profile")
                    .output
                    .device
                    .as_deref(),
                Some("USB Audio")
            );
            assert!(settings.warnings.is_empty(), "{:?}", settings.warnings);
            engine.shutdown();
        });
    }
}
