mod desktop;
pub use desktop::{PlacementTarget, anchor, open_path, placement, set_autostart, single_instance};

use crate::Result;
use std::{cell::Cell, mem::size_of, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
        UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

pub enum Event {
    Toggle,
    ContextMenu(RECT),
    Save(i32),
    Open,
    Resume,
    Restore,
    Quit,
}

thread_local! {
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

pub struct Tray {
    pub hwnd: HWND,
    icon: NOTIFYICONDATAW,
    image: HICON,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
        let class = w!("HearlierTrayWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err(windows::core::Error::from_thread().into());
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("hearlier"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )?
        };
        TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        let image = app_icon().unwrap_or_default();
        let mut icon = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: image,
            ..Default::default()
        };
        if image.is_invalid() {
            icon.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
        }
        copy_text(&mut icon.szTip, "hearlier");
        let tray = Self { hwnd, icon, image };
        tray.restore()?;
        Ok(tray)
    }

    pub fn restore(&self) -> Result<()> {
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &self.icon) }.as_bool() {
            return Err("Windows could not add hearlier to the system tray.".into());
        }
        let mut data = self.icon;
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        Ok(())
    }

    pub fn rect(&self) -> Option<RECT> {
        icon_rect(self.hwnd)
    }

    pub fn focus(&self) {
        let _ = unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &self.icon) };
    }

    pub fn tooltip(&mut self, state: &str) {
        let text = format!("hearlier · {state}");
        let previous = self.icon.szTip;
        copy_text(&mut self.icon.szTip, &text);
        if previous != self.icon.szTip {
            let mut data = self.icon;
            data.uFlags = NIF_TIP | NIF_SHOWTIP;
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        }
    }

    pub fn notify(&self, title: &str, message: &str, success: bool) {
        let mut data = self.icon;
        data.uFlags = NIF_INFO | NIF_REALTIME;
        data.dwInfoFlags = if success { NIIF_INFO } else { NIIF_WARNING };
        data.dwInfoFlags |= NIIF_NOSOUND | NIIF_RESPECT_QUIET_TIME;
        copy_text(&mut data.szInfoTitle, title);
        copy_text(&mut data.szInfo, message);
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.icon);
            let _ = DestroyWindow(self.hwnd);
            if !self.image.is_invalid() {
                let _ = DestroyIcon(self.image);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Shortcut(HOT_KEY_MODIFIERS, u32);

impl Shortcut {
    pub fn parse(text: &str) -> Result<Option<Self>> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let invalid =
            "Use Ctrl, Alt, Shift or Win with A–Z, 0–9 or F1–F11. Leave blank to disable.";
        let mut parts = text.split('+').map(|part| part.trim().to_ascii_uppercase());
        let key = parts.next_back().unwrap_or_default();
        let mut modifiers = HOT_KEY_MODIFIERS(0);
        for part in parts {
            let modifier = match part.as_str() {
                "CTRL" => MOD_CONTROL,
                "ALT" => MOD_ALT,
                "SHIFT" => MOD_SHIFT,
                "WIN" => MOD_WIN,
                _ => return Err(invalid.into()),
            };
            if modifiers.contains(modifier) {
                return Err(invalid.into());
            }
            modifiers |= modifier;
        }
        let vk = match key.as_bytes() {
            [ch @ (b'A'..=b'Z' | b'0'..=b'9')] if modifiers.0 != 0 => u32::from(*ch),
            _ => key
                .strip_prefix('F')
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|number| (1..12).contains(number))
                .map(|number| u32::from(VK_F1.0) + number - 1)
                .ok_or(invalid)?,
        };
        Ok(Some(Self(modifiers, vk)))
    }
}

pub struct Hotkey {
    hwnd: HWND,
    pub id: i32,
    pub shortcut: Shortcut,
}

impl Hotkey {
    pub fn register(hwnd: HWND, id: i32, shortcut: Shortcut) -> Result<Self> {
        unsafe { RegisterHotKey(Some(hwnd), id, shortcut.0 | MOD_NOREPEAT, shortcut.1) }.map_err(
            |err| format!("The shortcut is unavailable. Choose a different combination. ({err})"),
        )?;
        Ok(Self { hwnd, id, shortcut })
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterHotKey(Some(self.hwnd), self.id) };
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let event = match message {
        WM_HOTKEY => Event::Save(wparam.0 as i32),
        TRAY_MESSAGE if (lparam.0 as u32 & 0xffff) == NIN_BALLOONUSERCLICK => Event::Open,
        TRAY_MESSAGE if matches!(lparam.0 as u32 & 0xffff, NIN_KEYSELECT | WM_LBUTTONUP) => {
            Event::Toggle
        }
        TRAY_MESSAGE if (lparam.0 as u32 & 0xffff) == WM_CONTEXTMENU => {
            // Read the icon bounds before activating our window, which can close overflow.
            Event::ContextMenu(anchor(icon_rect(hwnd)))
        }
        WM_CLOSE => Event::Quit,
        WM_POWERBROADCAST if wparam.0 == PBT_APMRESUMEAUTOMATIC as usize => Event::Resume,
        id if id != 0 && id == TASKBAR_CREATED.get() => Event::Restore,
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    };
    if matches!(event, Event::Toggle | Event::ContextMenu(_)) {
        // Claim Explorer's activation without deactivating a panel we already own.
        let mut foreground_process = 0;
        unsafe {
            GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut foreground_process));
            if foreground_process != GetCurrentProcessId() {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
    // Queue actions for the WinUI dispatcher before touching application state.
    crate::app::tray_event(event);
    LRESULT(if message == WM_POWERBROADCAST { 1 } else { 0 })
}

fn icon_rect(hwnd: HWND) -> Option<RECT> {
    unsafe {
        Shell_NotifyIconGetRect(&NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            ..Default::default()
        })
    }
    .ok()
}

fn app_icon() -> Result<HICON> {
    let path = std::env::current_exe()?.with_file_name("hearlier.ico");
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let image = unsafe {
        LoadImageW(
            None,
            PCWSTR(path.as_ptr()),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_LOADFROMFILE,
        )?
    };
    Ok(HICON(image.0))
}

fn copy_text<const N: usize>(output: &mut [u16; N], text: &str) {
    let mut index = 0;
    for character in text.chars() {
        if index + character.len_utf16() >= N {
            break;
        }
        index += character.encode_utf16(&mut output[index..]).len();
    }
    output[index..].fill(0);
}

pub fn cursor() -> POINT {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    point
}

pub fn left_button_down() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}
