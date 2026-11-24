use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use crate::{
    Result,
    engine::{Format, MAX_HISTORY_MINUTES},
    wasapi_input::Source,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Settings {
    pub sources: [Source; 2],
    pub retention: u64,
    pub minutes: u64,
    pub output: PathBuf,
    pub format: ExportFormat,
    pub hotkey: String,
    pub autostart: bool,
    pub notifications: bool,
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
        let mut directory = root.join("hearlier");
        let legacy_directory = root.join("Recall Audio");
        if !directory.join("settings.json").try_exists()?
            && legacy_directory.join("settings.json").try_exists()?
        {
            directory = legacy_directory;
        }
        let file = directory.join("settings.json");
        let (mut settings, first): (Self, bool) = match fs::read(&file) {
            Ok(bytes) => (
                serde_json::from_slice(&bytes)
                    .map_err(|err| format!("Could not read {}: {err}", file.display()))?,
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
                    autostart: false,
                    notifications: true,
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
