mod bridge;
mod settings;

use std::{
    cell::RefCell,
    os::windows::io::OwnedHandle,
    path::PathBuf,
    sync::{atomic::Ordering::Relaxed, mpsc},
    time::Duration,
};

use crate::tray::{self, DoubleTapKey, Event, Hotkey, KeyboardInput, Shortcut, Tray};
use crate::{
    Result,
    engine::{Config, Engine, Notice},
    wasapi_input::{CaptureEvent, WasapiInput},
};
use settings::Settings;
use windows::Win32::Foundation::RECT;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TRAY_EVENTS: RefCell<Option<EventSender>> = const { RefCell::new(None) };
}

fn with_app<T>(action: impl FnOnce(&mut App) -> Result<T>) -> Result<T> {
    APP.with(|app| {
        let mut app = app.try_borrow_mut()?;
        action(app.as_mut().ok_or("Sonago is not running.")?)
    })
}

type Wake = extern "C" fn();

#[derive(Clone)]
struct EventSender {
    queue: mpsc::Sender<AppEvent>,
    wake: Wake,
}

impl EventSender {
    fn post(&self, event: AppEvent) {
        if self.queue.send(event).is_ok() {
            (self.wake)();
        }
    }
}

struct Com;

impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

fn start(wake: Wake) -> Result<Option<bool>> {
    let Some(_instance) = tray::single_instance()? else {
        return Ok(None);
    };
    wasapi::initialize_sta().ok()?;
    let _com = Com;
    let (file, settings, first) = Settings::load()?;
    let (queue, incoming) = mpsc::channel();
    let events = EventSender { queue, wake };
    let mut app = App {
        file,
        settings,
        first,
        runtime: None,
        hotkey: None,
        keyboard: None,
        tray: Tray::new()?,
        events: events.clone(),
        incoming,
        anchor: None,
        menu_anchor: None,
        capture_issue: [None, None],
        engine_issue: None,
        shortcut_issue: None,
        export_issue: None,
        transcript_issue: None,
        pending_save: None,
        latest_clip: None,
        _com,
        _instance,
    };
    if !first {
        app.runtime = Some(Runtime::start(&app.settings, &events)?);
        match app.register_shortcut(&app.settings) {
            Ok(hotkey) => app.hotkey = hotkey,
            Err(err) => app.shortcut_issue = Some(err.to_string()),
        }
        if app.settings.double_tap != DoubleTapKey::Off {
            match KeyboardInput::register(app.tray.hwnd) {
                Ok(keyboard) => {
                    keyboard.set_key(app.settings.double_tap);
                    app.keyboard = Some(keyboard);
                }
                Err(err) => {
                    app.shortcut_issue.get_or_insert_with(|| err.to_string());
                }
            }
        }
        if app.settings.autostart
            && let Err(err) = tray::set_autostart(true)
        {
            app.show_error(&format!("Could not register Sonago at startup. {err}"));
        }
    }
    app.refresh();
    APP.set(Some(app));
    TRAY_EVENTS.set(Some(events));
    Ok(Some(first))
}

fn stop() {
    TRAY_EVENTS.take();
    drop(APP.take());
}

struct Runtime {
    capture: WasapiInput,
    engine: Engine,
}

impl Runtime {
    fn start(settings: &Settings, events: &EventSender) -> Result<Self> {
        let engine_events = events.clone();
        let (engine, input) = Engine::start(
            Config {
                history_minutes: settings.retention,
                output: settings.output.clone(),
            },
            move |event| engine_events.post(AppEvent::Engine(event)),
        )?;
        let capture_events = events.clone();
        let capture = WasapiInput::start_with(input, settings.sources.clone(), move |event| {
            capture_events.post(AppEvent::Capture(event));
        })?;
        Ok(Self { capture, engine })
    }
}

enum AppEvent {
    Tray(Event),
    Engine(Notice),
    Capture(CaptureEvent),
}

pub fn tray_event(event: Event) {
    TRAY_EVENTS.with_borrow(|events| {
        if let Some(events) = events {
            events.post(AppEvent::Tray(event));
        }
    });
}

struct App {
    // Drop audio input before the engine, and shortcut registrations before their window.
    runtime: Option<Runtime>,
    hotkey: Option<Hotkey>,
    keyboard: Option<KeyboardInput>,
    tray: Tray,
    file: PathBuf,
    settings: Settings,
    first: bool,
    events: EventSender,
    incoming: mpsc::Receiver<AppEvent>,
    anchor: Option<RECT>,
    menu_anchor: Option<RECT>,
    capture_issue: [Option<String>; 2],
    engine_issue: Option<String>,
    shortcut_issue: Option<String>,
    export_issue: Option<String>,
    transcript_issue: Option<String>,
    // Capture the transcription setting at the same time as the output format.
    pending_save: Option<bool>,
    latest_clip: Option<PathBuf>,
    _com: Com,
    _instance: OwnedHandle,
}

impl App {
    fn register_shortcut(&self, settings: &Settings) -> Result<Option<Hotkey>> {
        let Some(shortcut) = Shortcut::parse(&settings.hotkey)? else {
            return Ok(None);
        };
        let next_id = self.hotkey.as_ref().map_or(1, |hotkey| 3 - hotkey.id);
        Ok(Some(Hotkey::register(self.tray.hwnd, next_id, shortcut)?))
    }

    fn commit(&mut self, candidate: Settings) -> Result<()> {
        candidate.validate()?;
        let before = self.hotkey.as_ref().map(|hotkey| hotkey.shortcut);
        let shortcut_changed = Shortcut::parse(&candidate.hotkey)? != before;
        let new_hotkey = if shortcut_changed {
            self.register_shortcut(&candidate)?
        } else {
            None
        };
        let new_keyboard = if self.keyboard.is_none() && candidate.double_tap != DoubleTapKey::Off {
            Some(KeyboardInput::register(self.tray.hwnd)?)
        } else {
            None
        };
        let autostart_changed = candidate.autostart != self.settings.autostart;
        if autostart_changed {
            tray::set_autostart(candidate.autostart)?;
        }
        if let Err(err) = candidate.write(&self.file) {
            if autostart_changed {
                tray::set_autostart(self.settings.autostart)?;
            }
            return Err(err);
        }
        if shortcut_changed {
            self.hotkey = new_hotkey;
        }
        if candidate.double_tap == DoubleTapKey::Off {
            self.keyboard = None;
        } else if new_keyboard.is_some() {
            self.keyboard = new_keyboard;
        }
        if let Some(keyboard) = &self.keyboard {
            keyboard.set_key(candidate.double_tap);
        }
        self.shortcut_issue = None;
        if let Some(runtime) = &self.runtime {
            runtime.engine.set_retention(candidate.retention);
        }
        if candidate.output != self.settings.output {
            self.export_issue = None;
        }
        self.install(candidate);
        Ok(())
    }

    fn apply(&mut self, candidate: Settings) -> Result<()> {
        if self.first {
            // Start with both inputs off. Open devices only after settings have been saved.
            let mut initial = candidate.clone();
            for source in &mut initial.sources {
                source.enabled = false;
            }
            let runtime = Runtime::start(&initial, &self.events)?;
            self.commit(candidate)?;
            runtime.capture.set_sources(self.settings.sources.clone());
            self.runtime = Some(runtime);
            self.first = false;
        } else {
            self.commit(candidate)?;
        }
        self.refresh();
        Ok(())
    }

    fn update_sources(&mut self, candidate: Settings) -> Result<()> {
        // An occupied hotkey at startup must not prevent device or privacy changes.
        match candidate.write(&self.file) {
            Ok(()) => {
                self.install(candidate);
                Ok(())
            }
            Err(err) => {
                let turning_off = candidate
                    .sources
                    .iter()
                    .zip(&self.settings.sources)
                    .any(|(after, before)| before.enabled && !after.enabled);
                if turning_off {
                    self.install(candidate);
                    return Err(format!(
                        "Source switched off for this session. Could not save settings: {err}"
                    )
                    .into());
                }
                Err(err)
            }
        }
    }

    fn install(&mut self, candidate: Settings) {
        if let Some(runtime) = &self.runtime {
            runtime.capture.set_sources(candidate.sources.clone());
        }
        for (track, source) in candidate.sources.iter().enumerate() {
            if *source != self.settings.sources[track] {
                self.capture_issue[track] = None;
            }
        }
        self.settings = candidate;
        self.refresh();
    }

    fn save(&mut self, minutes: u64) {
        let Some(runtime) = &self.runtime else { return };
        if self.pending_save.is_some() || runtime.engine.stats().saving.load(Relaxed) {
            return;
        }
        if let Err(err) = runtime.engine.save_to(
            Duration::from_secs(minutes * 60),
            self.settings.format.engine(),
            self.settings.output.clone(),
        ) {
            self.show_error(&err.to_string());
        } else {
            self.pending_save = Some(self.settings.transcribe);
        }
        self.refresh();
    }

    fn can_quit(&self) -> Result<()> {
        if self.pending_save.is_some()
            || self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.engine.stats().saving.load(Relaxed))
        {
            return Err("Wait for the current clip to finish saving.".into());
        }
        Ok(())
    }

    fn show_error(&mut self, message: &str) {
        self.export_issue = Some(message.to_owned());
        self.notify("Sonago", message, None);
        self.refresh();
    }

    fn notify(&mut self, title: &str, message: &str, clip: Option<PathBuf>) {
        if self.settings.notifications {
            let success = clip.is_some();
            self.latest_clip = clip;
            self.tray.notify(title, message, success);
        }
    }

    fn on_event(
        &mut self,
        event: AppEvent,
        actions: &mut Vec<&'static str>,
        clips: &mut Vec<PathBuf>,
    ) {
        match event {
            AppEvent::Tray(Event::OutsideClick) => actions.push("outside_click"),
            AppEvent::Tray(Event::Toggle { activate }) => {
                actions.push(if activate {
                    "toggle_activate"
                } else {
                    "toggle"
                });
            }
            AppEvent::Tray(Event::ContextMenu(rect)) => {
                self.menu_anchor = Some(rect);
                actions.push("context_menu");
            }
            AppEvent::Tray(Event::Quit) => match self.can_quit() {
                Ok(()) => actions.push("quit"),
                Err(err) => self.show_error(&err.to_string()),
            },
            AppEvent::Tray(Event::Save(id)) => {
                if self.hotkey.as_ref().is_some_and(|hotkey| hotkey.id == id) {
                    self.save(self.settings.minutes);
                }
            }
            AppEvent::Tray(Event::DoubleTap(key)) => {
                if self.keyboard.is_some() && self.settings.double_tap == key {
                    self.save(self.settings.minutes);
                }
            }
            AppEvent::Tray(Event::Open) => {
                if let Some(path) = &self.latest_clip
                    && let Err(err) = tray::open_path(self.tray.hwnd, path)
                {
                    self.show_error(&err.to_string());
                }
            }
            AppEvent::Tray(Event::Resume) => {
                if let Some(runtime) = &self.runtime {
                    runtime.capture.resume();
                }
            }
            AppEvent::Tray(Event::Restore) => {
                if let Err(err) = self.tray.restore() {
                    self.show_error(&err.to_string());
                }
            }
            AppEvent::Capture(CaptureEvent::Ready(track)) => self.capture_issue[track] = None,
            AppEvent::Capture(CaptureEvent::Unavailable(track, detail)) => {
                self.capture_issue[track] = Some(detail);
                let message = if track == 0 {
                    "Check the selected system output."
                } else {
                    "Check the selected microphone and Windows microphone permissions."
                };
                self.notify("Audio device unavailable", message, None);
            }
            AppEvent::Capture(CaptureEvent::Failed(err)) => self.engine_issue = Some(err),
            AppEvent::Engine(Notice::EngineError(err)) => self.engine_issue = Some(err),
            AppEvent::Engine(Notice::DiskError(_)) => {
                self.notify(
                    "History shortened",
                    "Windows could not write the temporary audio buffer.",
                    None,
                );
            }
            AppEvent::Engine(Notice::SaveFailed(err)) => {
                self.pending_save = None;
                self.export_issue = Some(err.clone());
                self.notify("Could not save the clip", &err, None);
            }
            AppEvent::Engine(Notice::Saved(path)) => {
                self.export_issue = None;
                let file = path.file_name().unwrap_or_default().to_string_lossy();
                let message = if self.pending_save.take() == Some(true) {
                    clips.push(path.clone());
                    format!("{file}\nPreparing transcript")
                } else {
                    format!("{file}\nClick to open")
                };
                self.notify("Sonago · Clip saved", &message, Some(path));
            }
        }
        self.refresh();
    }
}
