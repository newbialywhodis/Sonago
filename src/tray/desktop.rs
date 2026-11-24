use std::{
    os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
    },
    path::Path,
};

use crate::Result;
use serde::Serialize;
use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HWND, POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        System::{
            Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW},
            Threading::CreateMutexW,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Shell::ShellExecuteW,
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{PCWSTR, w},
};

pub fn single_instance() -> Result<Option<OwnedHandle>> {
    // Share the session mutex with builds from before the rename.
    let raw = unsafe { CreateMutexW(None, false, w!("Local\\RecallAudio.Session"))? };
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    if exists {
        return Ok(None);
    }
    Ok(Some(handle))
}

pub fn anchor(rect: Option<RECT>) -> RECT {
    rect.filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
        .unwrap_or_else(|| {
            let POINT { x, y } = super::cursor();
            RECT {
                left: x,
                top: y,
                right: x + 1,
                bottom: y + 1,
            }
        })
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Placement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    dpi: u32,
}

pub enum PlacementTarget {
    Setup,
    Panel,
    TrayMenu,
}

pub fn placement(
    rect: RECT,
    logical_width: i32,
    logical_height: i32,
    target: PlacementTarget,
) -> Result<Placement> {
    let center = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    let monitor = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(windows::core::Error::from_thread().into());
    }
    let (mut dpi_x, mut dpi_y) = (96, 96);
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    Ok(place(
        info.rcWork,
        logical_width,
        logical_height,
        dpi_x,
        target,
        rect,
    ))
}

fn place(
    work: RECT,
    logical_width: i32,
    logical_height: i32,
    dpi: u32,
    target: PlacementTarget,
    anchor: RECT,
) -> Placement {
    let scale = |value: i32| (value * dpi as i32 + 48) / 96;
    let margin = scale(if matches!(target, PlacementTarget::TrayMenu) {
        6
    } else {
        12
    });
    let max_width = (work.right - work.left - 2 * margin).max(1);
    let max_height = (work.bottom - work.top - 2 * margin).max(1);
    let width = scale(logical_width.clamp(1, 4096)).clamp(1, max_width);
    let height = scale(logical_height.clamp(1, 4096)).clamp(1, max_height);
    let (x, y) = match target {
        PlacementTarget::Setup => (
            (work.left + work.right - width) / 2,
            (work.top + work.bottom - height) / 2,
        ),
        PlacementTarget::Panel => (work.right - margin - width, work.bottom - margin - height),
        PlacementTarget::TrayMenu => {
            // Leave room for the menu's downward offset during its slide.
            let gap = scale(16);
            let above = anchor.top - gap - height;
            let y = if above >= work.top + margin {
                above
            } else {
                anchor.bottom + gap
            };
            let right = (work.right - margin - width).max(work.left + margin);
            let bottom = (work.bottom - gap - height).max(work.top + margin);
            (
                (anchor.right - width).clamp(work.left + margin, right),
                y.clamp(work.top + margin, bottom),
            )
        }
    };
    Placement {
        x,
        y,
        width,
        height,
        dpi,
    }
}

pub fn open_path(hwnd: HWND, path: &Path) -> Result<()> {
    let text: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            PCWSTR(text.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err(format!("Windows could not open {}.", path.display()).into());
    }
    Ok(())
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    // Update the same startup entry used by builds from before the rename.
    let name = w!("Recall Audio");
    let status = if enabled {
        let executable = std::env::current_exe()?;
        let command: Vec<u16> = [34]
            .into_iter()
            .chain(executable.as_os_str().encode_wide())
            .chain([34, 0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                name,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, name) }
    };
    if !enabled && status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    status.ok()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_panel_twelve_pixels_inside_bottom_right() {
        let work = RECT {
            left: 0,
            right: 1920,
            top: 0,
            bottom: 1032,
        };
        let compact = place(work, 432, 100, 96, PlacementTarget::Panel, work);
        let settings = place(work, 432, 640, 96, PlacementTarget::Panel, work);
        assert_eq!((compact.x, compact.y), (1476, 920));
        assert_eq!(compact.y + compact.height, settings.y + settings.height);
        assert_eq!(compact.x, settings.x);
    }

    #[test]
    fn scales_panel_margin_on_negative_coordinate_monitors() {
        let work = RECT {
            left: -1920,
            right: -50,
            top: -120,
            bottom: 960,
        };
        let compact = place(work, 432, 100, 144, PlacementTarget::Panel, work);
        let settings = place(work, 432, 1200, 144, PlacementTarget::Panel, work);
        assert_eq!((compact.x, compact.y), (-716, 792));
        assert_eq!(settings.y, work.top + 18);
        assert_eq!(compact.y + compact.height, settings.y + settings.height);
    }

    #[test]
    fn places_tray_menu_above_overflow_icon_on_scaled_monitor() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1032,
        };
        let icon = RECT {
            left: -240,
            top: 970,
            right: -200,
            bottom: 1010,
        };
        let menu = place(work, 190, 110, 144, PlacementTarget::TrayMenu, icon);
        assert_eq!(
            (menu.x, menu.y, menu.width, menu.height),
            (-485, 781, 285, 165)
        );
    }

    #[test]
    fn places_tray_menu_below_top_taskbar_and_inside_screen() {
        let work = RECT {
            left: 0,
            top: 48,
            right: 1920,
            bottom: 1080,
        };
        let icon = RECT {
            left: 1888,
            top: 0,
            right: 1920,
            bottom: 48,
        };
        let menu = place(work, 190, 110, 96, PlacementTarget::TrayMenu, icon);
        assert_eq!((menu.x, menu.y), (1724, 64));
    }
}
