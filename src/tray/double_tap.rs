use super::watch_raw_input;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use windows::Win32::{
    Foundation::HWND,
    UI::{
        Input::{KeyboardAndMouse::*, RAWKEYBOARD},
        WindowsAndMessaging::{GetMessageTime, RI_KEY_BREAK, RI_KEY_E0},
    },
};

const TAP_WINDOW_MS: u32 = 400;
const KEYBOARD_USAGE: u16 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoubleTapKey {
    Off,
    #[default]
    RightCtrl,
    LeftCtrl,
    RightShift,
    LeftShift,
}

impl DoubleTapKey {
    fn virtual_key(self) -> Option<VIRTUAL_KEY> {
        match self {
            Self::Off => None,
            Self::RightCtrl => Some(VK_RCONTROL),
            Self::LeftCtrl => Some(VK_LCONTROL),
            Self::RightShift => Some(VK_RSHIFT),
            Self::LeftShift => Some(VK_LSHIFT),
        }
    }
}

thread_local! {
    static DETECTOR: RefCell<Option<(DoubleTapKey, TapSequence)>> = const { RefCell::new(None) };
}

pub struct KeyboardInput;

impl KeyboardInput {
    pub fn register(hwnd: HWND) -> Result<Self> {
        watch_raw_input(KEYBOARD_USAGE, Some(hwnd))
            .map_err(|err| format!("Could not enable the double-tap shortcut. ({err})"))?;
        Ok(Self)
    }

    pub fn set_key(&self, key: DoubleTapKey) {
        DETECTOR.with_borrow_mut(|detector| {
            if !detector
                .as_ref()
                .is_some_and(|(current, _)| *current == key)
            {
                *detector = Some((key, TapSequence::default()));
            }
        });
    }
}

impl Drop for KeyboardInput {
    fn drop(&mut self) {
        DETECTOR.set(None);
        let _ = watch_raw_input(KEYBOARD_USAGE, None);
    }
}

fn virtual_key(keyboard: RAWKEYBOARD) -> VIRTUAL_KEY {
    match VIRTUAL_KEY(keyboard.VKey) {
        VK_CONTROL if u32::from(keyboard.Flags) & RI_KEY_E0 != 0 => VK_RCONTROL,
        VK_CONTROL => VK_LCONTROL,
        VK_SHIFT => {
            VIRTUAL_KEY(
                unsafe { MapVirtualKeyW(u32::from(keyboard.MakeCode), MAPVK_VSC_TO_VK_EX) } as u16,
            )
        }
        key => key,
    }
}

pub(super) fn input(keyboard: RAWKEYBOARD) -> Option<DoubleTapKey> {
    DETECTOR.with_borrow_mut(|detector| {
        let (key, sequence) = detector.as_mut()?;
        let target = key.virtual_key()?;
        let is_target = virtual_key(keyboard) == target;
        let released = u32::from(keyboard.Flags) & RI_KEY_BREAK != 0;
        let at = unsafe { GetMessageTime() } as u32;
        if !sequence.update(is_target, released, at) {
            return None;
        }
        let other_modifier_down = [
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LSHIFT,
            VK_RSHIFT,
            VK_LMENU,
            VK_RMENU,
            VK_LWIN,
            VK_RWIN,
        ]
        .into_iter()
        .any(|modifier| {
            modifier != target && unsafe { GetAsyncKeyState(i32::from(modifier.0)) } < 0
        });
        (!other_modifier_down).then_some(*key)
    })
}

#[derive(Default)]
struct TapSequence {
    held: bool,
    started: Option<u32>,
    previous: Option<u32>,
}

impl TapSequence {
    fn update(&mut self, is_target: bool, released: bool, at: u32) -> bool {
        if !is_target {
            self.started = None;
            self.previous = None;
        } else if released {
            self.held = false;
            if let Some(started) = self.started.take()
                && at.wrapping_sub(started) <= TAP_WINDOW_MS
            {
                if self
                    .previous
                    .take()
                    .is_some_and(|last| at.wrapping_sub(last) <= TAP_WINDOW_MS)
                {
                    return true;
                }
                self.previous = Some(at);
            } else {
                self.previous = None;
            }
        } else if !self.held {
            self.held = true;
            self.started = Some(at);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_input_distinguishes_left_and_right_ctrl() {
        let mut key = RAWKEYBOARD {
            VKey: VK_CONTROL.0,
            ..Default::default()
        };
        assert_eq!(virtual_key(key), VK_LCONTROL);
        key.Flags = (RI_KEY_E0 | RI_KEY_BREAK) as u16;
        assert_eq!(virtual_key(key), VK_RCONTROL);
    }

    #[test]
    fn double_tap_fires_once_on_second_release() {
        let mut taps = TapSequence::default();
        assert!(!taps.update(true, false, 0));
        assert!(!taps.update(true, true, 50));
        assert!(!taps.update(true, false, 140));
        assert!(taps.update(true, true, 190));
        assert!(!taps.update(true, false, 250));
        assert!(!taps.update(true, true, 300));
    }

    #[test]
    fn ignores_long_holds_repeat_keydowns_and_slow_pairs() {
        let mut taps = TapSequence::default();
        assert!(!taps.update(true, false, 0));
        assert!(!taps.update(true, false, 490));
        assert!(!taps.update(true, true, 500));
        assert!(!taps.update(true, false, 560));
        assert!(!taps.update(true, true, 600));
        assert!(!taps.update(true, false, 1_100));
        assert!(!taps.update(true, true, 1_150));
    }

    #[test]
    fn other_keys_cancel_the_pair_and_any_tap_in_progress() {
        let mut taps = TapSequence::default();
        assert!(!taps.update(true, false, 0));
        assert!(!taps.update(true, true, 40));
        assert!(!taps.update(true, false, 100));
        assert!(!taps.update(false, false, 145)); // Ctrl+C
        assert!(!taps.update(true, false, 150)); // Ctrl repeat
        assert!(!taps.update(false, true, 160));
        assert!(!taps.update(true, true, 170));
        assert!(!taps.update(true, false, 230));
        assert!(!taps.update(true, true, 270));
        assert!(!taps.update(false, false, 300)); // A key between otherwise valid taps
        assert!(!taps.update(false, true, 320));
        assert!(!taps.update(true, false, 350));
        assert!(!taps.update(true, true, 385));
    }
}
