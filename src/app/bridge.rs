use std::{
    ffi::{CStr, CString, c_char},
    sync::atomic::Ordering::Relaxed,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{App, AppEvent, Settings, Wake, start, stop, with_app};
use crate::{
    Result,
    tray::{self, Event, PlacementTarget},
    wasapi_input::{Endpoint, endpoints},
};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Command {
    Drain,
    Snapshot,
    Settings,
    Apply {
        settings: Settings,
    },
    Devices {
        track: usize,
    },
    ToggleSource {
        track: usize,
    },
    SelectDevice {
        track: usize,
        endpoint: Option<String>,
    },
    SaveLengths,
    Save {
        minutes: Option<u64>,
    },
    Place {
        width: i32,
        height: i32,
        centered: bool,
        new_anchor: bool,
    },
    PlaceTrayMenu {
        width: i32,
        height: i32,
    },
    PointerOverTray {
        require_press: bool,
    },
    WatchOutsideClicks {
        watch: bool,
    },
    FocusTray,
    CanQuit,
    Quit,
}

#[derive(Serialize)]
struct Snapshot {
    #[serde(skip_serializing)]
    status: &'static str,
    warning: Option<String>,
    issue: Option<String>,
    system_on: bool,
    mic_on: bool,
    can_save: bool,
    save_label: String,
}

#[derive(Serialize)]
struct DeviceChoice {
    label: String,
    endpoint: Option<String>,
    selected: bool,
}

#[derive(Serialize)]
struct Preferences {
    settings: Settings,
    devices: [Vec<DeviceChoice>; 2],
    issue: Option<String>,
}

#[derive(Serialize)]
struct SaveLength {
    minutes: u64,
    label: String,
    is_default: bool,
}

#[derive(Serialize)]
struct Reply {
    value: Value,
    error: Option<String>,
}

fn value(data: impl Serialize) -> Result<Value> {
    Ok(serde_json::to_value(data)?)
}

fn reply(result: Result<Value>) -> *mut c_char {
    let reply = match result {
        Ok(value) => Reply { value, error: None },
        Err(err) => Reply {
            value: Value::Null,
            error: Some(err.to_string()),
        },
    };
    // JSON escapes NUL characters, so its UTF-8 representation is a C string.
    CString::new(serde_json::to_vec(&reply).unwrap())
        .unwrap()
        .into_raw()
}

// The host calls start, invoke and stop only on its WinUI dispatcher and retains the
// wake callback until stop has joined the native workers. The callback only queues work.
#[unsafe(no_mangle)]
extern "C" fn hearlier_start(wake: Option<Wake>) -> *mut c_char {
    reply((|| {
        let wake = wake.ok_or("A dispatcher wake callback is required.")?;
        value(start(wake)?)
    })())
}

#[unsafe(no_mangle)]
extern "C" fn hearlier_invoke(command: *const c_char) -> *mut c_char {
    reply((|| {
        if command.is_null() {
            return Err("A hearlier command is required.".into());
        }
        // The marshaler provides a NUL-terminated UTF-8 command valid for this call.
        let command = unsafe { CStr::from_ptr(command) };
        let command = serde_json::from_slice(command.to_bytes())?;
        with_app(|app| app.invoke(command))
    })())
}

#[unsafe(no_mangle)]
extern "C" fn hearlier_free(reply: *mut c_char) {
    if !reply.is_null() {
        // Each response is freed by the host exactly once with the allocating runtime.
        drop(unsafe { CString::from_raw(reply) });
    }
}

#[unsafe(no_mangle)]
extern "C" fn hearlier_stop() {
    stop();
}

impl App {
    fn invoke(&mut self, command: Command) -> Result<Value> {
        match command {
            Command::Drain => {
                let mut actions = Vec::new();
                while let Ok(event) = self.incoming.try_recv() {
                    self.on_event(event, &mut actions);
                }
                value(actions)
            }
            Command::Snapshot => value(self.snapshot()),
            Command::Settings => value(self.preferences()),
            Command::Apply { settings } => {
                self.apply(settings)?;
                value(())
            }
            Command::Devices { track } => {
                let selected = self.source(track)?.endpoint.as_deref();
                let devices = endpoints()?;
                value(device_choices(track, &devices[track], selected))
            }
            Command::ToggleSource { track } => {
                let enabled = !self.source(track)?.enabled;
                let mut settings = self.settings.clone();
                settings.sources[track].enabled = enabled;
                self.update_sources(settings)?;
                value(())
            }
            Command::SelectDevice { track, endpoint } => {
                self.source(track)?;
                let mut settings = self.settings.clone();
                settings.sources[track].endpoint = endpoint;
                self.update_sources(settings)?;
                value(())
            }
            Command::SaveLengths => value(self.save_lengths()),
            Command::Save { minutes } => {
                let minutes = minutes.unwrap_or(self.settings.minutes);
                if !(1..=self.settings.retention).contains(&minutes) {
                    return Err("Choose a save length within the history limit.".into());
                }
                self.save(minutes);
                value(())
            }
            Command::Place {
                width,
                height,
                centered,
                new_anchor,
            } => {
                if new_anchor || self.anchor.is_none() {
                    self.anchor = Some(tray::anchor(self.tray.rect()));
                }
                value(tray::placement(
                    self.anchor.unwrap(),
                    width,
                    height,
                    if centered {
                        PlacementTarget::Setup
                    } else {
                        PlacementTarget::Panel
                    },
                )?)
            }
            Command::PlaceTrayMenu { width, height } => value(tray::placement(
                self.menu_anchor
                    .unwrap_or_else(|| tray::anchor(self.tray.rect())),
                width,
                height,
                PlacementTarget::TrayMenu,
            )?),
            Command::PointerOverTray { require_press } => value(
                (!require_press || tray::left_button_down())
                    && tray::over_icon(self.tray.hwnd, tray::cursor()),
            ),
            Command::WatchOutsideClicks { watch } => value(self.tray.watch_outside_clicks(watch)),
            Command::FocusTray => {
                self.tray.focus();
                value(())
            }
            Command::CanQuit => {
                self.can_quit()?;
                value(())
            }
            Command::Quit => {
                self.events.post(AppEvent::Tray(Event::Quit));
                value(())
            }
        }
    }

    fn source(&self, track: usize) -> Result<&crate::wasapi_input::Source> {
        self.settings
            .sources
            .get(track)
            .ok_or_else(|| "Unknown audio source.".into())
    }

    fn preferences(&self) -> Preferences {
        let (devices, issue) = match endpoints() {
            Ok(devices) => (devices, None),
            Err(err) => (
                Default::default(),
                Some(format!("Could not list audio devices. {err}")),
            ),
        };
        Preferences {
            settings: self.settings.clone(),
            devices: std::array::from_fn(|track| {
                device_choices(
                    track,
                    &devices[track],
                    self.settings.sources[track].endpoint.as_deref(),
                )
            }),
            issue,
        }
    }

    fn save_lengths(&self) -> Vec<SaveLength> {
        let default = self.settings.minutes;
        let retention = self.settings.retention;
        let mut values = [1, 5, 15, 30, 60, 180, default, retention]
            .into_iter()
            .filter(|minutes| *minutes <= retention)
            .collect::<Vec<_>>();
        values.sort_unstable();
        values.dedup();
        values
            .into_iter()
            .map(|minutes| SaveLength {
                minutes,
                label: format!("Save last {}", compact_minutes(minutes)),
                is_default: minutes == default,
            })
            .collect()
    }

    pub(super) fn refresh(&mut self) {
        self.tray.tooltip(self.snapshot().status);
    }

    fn snapshot(&self) -> Snapshot {
        let stats = self.runtime.as_ref().map(|runtime| runtime.engine.stats());
        let warning = self.warning(stats.is_some_and(|stats| stats.disk_degraded.load(Relaxed)));
        let system_on = self.settings.sources[0].enabled;
        let mic_on = self.settings.sources[1].enabled;
        let available =
            |track: usize| stats.is_some_and(|stats| stats.tracks[track].available.load(Relaxed));
        let system_capturing = system_on && available(0);
        let mic_capturing = mic_on && available(1);
        let status = if self.first || (!system_on && !mic_on) {
            "Paused"
        } else if warning.is_none() && (system_capturing || mic_capturing) {
            "Recording"
        } else {
            "Warning"
        };
        let device_issue = self
            .capture_issue
            .iter()
            .enumerate()
            .find_map(|(track, issue)| {
                let name = ["System output", "Microphone"][track];
                issue.as_ref().map(|detail| format!("{name} · {detail}"))
            });
        let saving = stats.is_some_and(|stats| stats.saving.load(Relaxed));
        let buffered = stats.map_or(Duration::ZERO, |stats| stats.buffered());
        Snapshot {
            status,
            issue: self
                .shortcut_issue
                .clone()
                .or(device_issue)
                .or_else(|| warning.clone()),
            warning,
            system_on,
            mic_on,
            can_save: !buffered.is_zero() && !saving,
            save_label: if saving {
                "Saving…".into()
            } else {
                format!("Save {}", compact_minutes(self.settings.minutes))
            },
        }
    }

    fn warning(&self, disk_degraded: bool) -> Option<String> {
        self.engine_issue
            .as_ref()
            .map(|_| "Audio capture stopped. Restart hearlier.".to_owned())
            .or_else(|| self.export_issue.clone())
            .or_else(|| {
                disk_degraded.then(|| {
                    "Could not write history to disk. Recent audio is still buffered.".into()
                })
            })
            .or_else(|| {
                self.capture_issue
                    .iter()
                    .enumerate()
                    .find_map(|(track, issue)| {
                        (issue.is_some() && self.settings.sources[track].enabled).then(|| {
                            if track == 0 {
                                "System output unavailable. Choose an output."
                            } else {
                                "Microphone unavailable. Check the device and Windows permissions."
                            }
                            .into()
                        })
                    })
            })
            .or_else(|| {
                self.shortcut_issue
                    .as_ref()
                    .map(|_| "Hotkey unavailable. Choose another in Settings.".into())
            })
    }
}

fn device_choices(track: usize, devices: &[Endpoint], selected: Option<&str>) -> Vec<DeviceChoice> {
    let mut choices = vec![DeviceChoice {
        label: [
            "Default output (Follow Windows)",
            "Default microphone (Follow Windows)",
        ][track]
            .into(),
        endpoint: None,
        selected: selected.is_none(),
    }];
    choices.extend(devices.iter().map(|device| DeviceChoice {
        label: device.name.clone(),
        endpoint: Some(device.id.clone()),
        selected: selected == Some(device.id.as_str()),
    }));
    if let Some(id) = selected
        && !devices.iter().any(|device| device.id == id)
    {
        choices.push(DeviceChoice {
            label: "Unavailable device".into(),
            endpoint: Some(id.into()),
            selected: true,
        });
    }
    choices
}

fn compact_minutes(minutes: u64) -> String {
    if minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}
