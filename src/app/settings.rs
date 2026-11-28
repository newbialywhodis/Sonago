use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use crate::{
    Result,
    engine::{Format, MAX_HISTORY_MINUTES},
    tray::DoubleTapKey,
    wasapi_input::Source,
};
use serde::{Deserialize, Serialize};

use super::save_sound;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Settings {
    pub sources: [Source; 2],
    pub retention: u64,
    pub minutes: u64,
    pub output: PathBuf,
    pub format: ExportFormat,
    pub hotkey: String,
    #[serde(default)]
    pub double_tap: DoubleTapKey,
    pub autostart: bool,
    pub notifications: bool,
    #[serde(default)]
    pub save_sound: u8,
    #[serde(default = "transcribe_by_default")]
    pub transcribe: bool,
}

fn transcribe_by_default() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExportFormat {
    Matroska,
    OggOpus,
}

impl ExportFormat {
    pub fn engine(self) -> Format {
        match self {
            Self::Matroska => Format::Matroska,
            Self::OggOpus => Format::OggOpus,
        }
    }
}

impl Settings {
    pub fn load() -> Result<(PathBuf, Self, bool)> {
        let directory = std::env::var_os("LOCALAPPDATA")
            .ok_or("Windows did not provide a local app data folder.")?;
        let root = PathBuf::from(directory);
        let directory = root.join("Sonago");
        let file = directory.join("settings.json");
        let mut source = file.clone();
        if !file.try_exists()? {
            for legacy in ["hearlier", "Recall Audio"] {
                let candidate = root.join(legacy).join("settings.json");
                if candidate.try_exists()? {
                    source = candidate;
                    break;
                }
            }
        }
        let (mut settings, first): (Self, bool) = match fs::read(&source) {
            Ok(bytes) => (
                serde_json::from_slice(&bytes)
                    .map_err(|err| format!("Could not read {}: {err}", source.display()))?,
                false,
            ),
            Err(err) if err.kind() == ErrorKind::NotFound => (
                Self {
                    sources: [
                        Source {
                            enabled: true,
                            endpoint: None,
                        },
                        Source {
                            enabled: false,
                            endpoint: None,
                        },
                    ],
                    retention: 180,
                    minutes: 5,
                    output: directory.join("Recordings"),
                    format: ExportFormat::Matroska,
                    hotkey: "Ctrl+Alt+F9".into(),
                    double_tap: DoubleTapKey::default(),
                    autostart: false,
                    notifications: true,
                    save_sound: 0,
                    transcribe: true,
                },
                true,
            ),
            Err(err) => return Err(err.into()),
        };
        let mut args = std::env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--history-minutes") => {
                    settings.retention = args
                        .next()
                        .and_then(|value| value.to_str()?.parse().ok())
                        .ok_or("Enter a number after --history-minutes.")?;
                    settings.minutes = settings.minutes.min(settings.retention);
                }
                Some("--output") => {
                    settings.output = std::path::absolute(
                        args.next()
                            .filter(|path| !path.is_empty())
                            .ok_or("Enter a folder after --output.")?,
                    )?
                }
                _ => return Err(format!("Unknown argument: {}", arg.to_string_lossy()).into()),
            }
        }
        settings.validate()?;
        Ok((file, settings, first))
    }

    pub fn validate(&self) -> Result<()> {
        save_sound::validate(self.save_sound)?;
        if !(1..=MAX_HISTORY_MINUTES).contains(&self.retention) {
            return Err("History must be between 1 and 720 minutes.".into());
        }
        if !(1..=self.retention).contains(&self.minutes) {
            return Err(
                "Default save length must be between 1 minute and the history limit.".into(),
            );
        }
        if !self.output.is_absolute() {
            return Err("Choose an absolute path for the recordings folder.".into());
        }
        Ok(())
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let directory = path.parent().expect("settings file has a parent");
        fs::create_dir_all(directory)?;
        let mut temp = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(temp.as_file_mut(), self)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_settings_default_to_right_ctrl_and_no_save_sound() -> Result<()> {
        let mut stored = serde_json::json!({
            "sources": [
                {"enabled": true, "endpoint": null},
                {"enabled": false, "endpoint": null}
            ],
            "retention": 180,
            "minutes": 5,
            "output": "C:/Recordings",
            "format": "matroska",
            "hotkey": "Ctrl+Alt+F9",
            "autostart": false,
            "notifications": true
        });
        let settings: Settings = serde_json::from_value(stored.clone())?;
        assert_eq!(settings.double_tap, DoubleTapKey::RightCtrl);
        assert_eq!(settings.save_sound, 0);

        stored["double_tap"] = "off".into();
        stored["save_sound"] = 8.into();
        let settings: Settings = serde_json::from_value(stored)?;
        assert_eq!(settings.double_tap, DoubleTapKey::Off);
        assert_eq!(settings.save_sound, 8);
        settings.validate()?;
        let stored = serde_json::to_value(settings)?;
        assert_eq!(stored["double_tap"], "off");
        assert_eq!(stored["save_sound"], 8);
        Ok(())
    }
}
