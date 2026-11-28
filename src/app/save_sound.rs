use crate::Result;
use windows::{
    Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT},
    core::PCWSTR,
};

// Positions are part of the settings format. New clips go at the end.
const CLIPS: [&[u8]; 8] = [
    include_bytes!("../../assets/save-sounds/1.wav"),
    include_bytes!("../../assets/save-sounds/2.wav"),
    include_bytes!("../../assets/save-sounds/3.wav"),
    include_bytes!("../../assets/save-sounds/4.wav"),
    include_bytes!("../../assets/save-sounds/5.wav"),
    include_bytes!("../../assets/save-sounds/6.wav"),
    include_bytes!("../../assets/save-sounds/7.wav"),
    include_bytes!("../../assets/save-sounds/8.wav"),
];

pub(super) fn choices() -> Vec<String> {
    std::iter::once("No sound".into())
        .chain((1..=CLIPS.len()).map(|number| format!("Sound {number}")))
        .collect()
}

pub(super) fn validate(sound: u8) -> Result<()> {
    if usize::from(sound) > CLIPS.len() {
        return Err("Unknown save sound.".into());
    }
    Ok(())
}

pub(super) fn play(sound: u8) -> Result<()> {
    validate(sound)?;
    let Some(index) = sound.checked_sub(1) else {
        return Ok(());
    };
    let wav = CLIPS[usize::from(index)];
    // Windows reads the WAV asynchronously. Its embedded bytes live for the process lifetime.
    let played = unsafe {
        PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_ASYNC | SND_MEMORY | SND_NODEFAULT,
        )
    };
    if !played.as_bool() {
        return Err("Could not play the save sound.".into());
    }
    Ok(())
}
