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
        Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            MonitorFromRect,
        },
        System::{
            Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW},
            Threading::CreateMutexW,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Shell::{
                ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETAUTOHIDEBAREX, ABM_GETTASKBARPOS,
                APPBARDATA, SHAppBarMessage, ShellExecuteW,
            },
            WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GetClassNameW, GetWindowRect, SW_SHOWNORMAL,
            },
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
    edge: TaskbarEdge,
    work_area: WorkArea,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskbarEdge {
    Left,
    Top,
    Right,
    Bottom,
}

impl TaskbarEdge {
    fn from_rect(screen: RECT, bar: RECT) -> Self {
        if bar.right - bar.left >= bar.bottom - bar.top {
            if bar.top + bar.bottom < screen.top + screen.bottom {
                Self::Top
            } else {
                Self::Bottom
            }
        } else if bar.left + bar.right < screen.left + screen.right {
            Self::Left
        } else {
            Self::Right
        }
    }

    fn from_work_area(screen: RECT, work: RECT) -> Self {
        [
            (screen.bottom - work.bottom, Self::Bottom),
            (work.top - screen.top, Self::Top),
            (work.left - screen.left, Self::Left),
            (screen.right - work.right, Self::Right),
        ]
        .into_iter()
        .max_by_key(|&(inset, _)| inset)
        .filter(|&(inset, _)| inset > 0)
        .map_or(Self::Bottom, |(_, edge)| edge)
    }

    fn reserve(self, screen: RECT, work: RECT, bar: RECT) -> RECT {
        // Autohiding bars can sit outside rcMonitor. Keep their fully revealed
        // thickness clear so a slide ends at the same edge while the bar moves.
        let width = bar.right - bar.left;
        let height = bar.bottom - bar.top;
        let mut work = work;
        match self {
            Self::Left => work.left = work.left.max(screen.left + width).min(work.right - 1),
            Self::Top => work.top = work.top.max(screen.top + height).min(work.bottom - 1),
            Self::Right => work.right = work.right.min(screen.right - width).max(work.left + 1),
            Self::Bottom => work.bottom = work.bottom.min(screen.bottom - height).max(work.top + 1),
        }
        work
    }
}

fn taskbar(monitor: HMONITOR, screen: RECT) -> Option<(TaskbarEdge, RECT)> {
    let mut data = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } != 0
        && unsafe { MonitorFromRect(&data.rc, MONITOR_DEFAULTTONEAREST) } == monitor
    {
        return Some((TaskbarEdge::from_rect(screen, data.rc), data.rc));
    }

    // Query the shell's monitor assignment before looking at window bounds:
    // a hidden secondary taskbar can extend onto a neighboring display.
    for (native, edge) in [
        (ABE_LEFT, TaskbarEdge::Left),
        (ABE_TOP, TaskbarEdge::Top),
        (ABE_RIGHT, TaskbarEdge::Right),
        (ABE_BOTTOM, TaskbarEdge::Bottom),
    ] {
        data.uEdge = native;
        data.rc = screen;
        let hwnd = HWND(unsafe { SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut data) } as _);
        if hwnd.is_invalid() {
            continue;
        }
        let mut class = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
        if matches!(
            String::from_utf16_lossy(&class[..len]).as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) && let Ok(rect) = window_rect(hwnd)
        {
            return Some((edge, rect));
        }
    }

    let primary = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.ok();
    let secondary = std::iter::successors(
        unsafe { FindWindowExW(None, None, w!("Shell_SecondaryTrayWnd"), None) }.ok(),
        |&hwnd| unsafe { FindWindowExW(None, Some(hwnd), w!("Shell_SecondaryTrayWnd"), None) }.ok(),
    );
    primary.into_iter().chain(secondary).find_map(|hwnd| {
        let rect = window_rect(hwnd).ok()?;
        (unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) } == monitor)
            .then(|| (TaskbarEdge::from_rect(screen, rect), rect))
    })
}

fn window_rect(hwnd: HWND) -> windows::core::Result<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect)? };
    Ok(rect)
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
    let (work, edge) = if matches!(target, PlacementTarget::Setup) {
        (info.rcWork, TaskbarEdge::Bottom)
    } else if let Some((edge, bar)) = taskbar(monitor, info.rcMonitor) {
        (edge.reserve(info.rcMonitor, info.rcWork, bar), edge)
    } else {
        (
            info.rcWork,
            TaskbarEdge::from_work_area(info.rcMonitor, info.rcWork),
        )
    };
    Ok(place(
        work,
        logical_width,
        logical_height,
        dpi_x,
        target,
        rect,
        edge,
    ))
}

fn place(
    work: RECT,
    logical_width: i32,
    logical_height: i32,
    dpi: u32,
    target: PlacementTarget,
    anchor: RECT,
    edge: TaskbarEdge,
) -> Placement {
    let scale = |value: i32| (value * dpi as i32 + 48) / 96;
    let margin = scale(if matches!(target, PlacementTarget::TrayMenu) {
        6
    } else {
        12
    });
    let (mut left, mut top, mut right, mut bottom) = (
        work.left + margin,
        work.top + margin,
        work.right - margin,
        work.bottom - margin,
    );
    if matches!(target, PlacementTarget::TrayMenu) {
        // Reserve room for the menu's short slide toward the taskbar.
        let slide = scale(12);
        match edge {
            TaskbarEdge::Left => left += slide,
            TaskbarEdge::Top => top += slide,
            TaskbarEdge::Right => right -= slide,
            TaskbarEdge::Bottom => bottom -= slide,
        }
    }
    let max_width = (right - left).max(1);
    let max_height = (bottom - top).max(1);
    let width = scale(logical_width.clamp(1, 4096)).clamp(1, max_width);
    let height = scale(logical_height.clamp(1, 4096)).clamp(1, max_height);
    let (x, y) = match target {
        PlacementTarget::Setup => (
            (work.left + work.right - width) / 2,
            (work.top + work.bottom - height) / 2,
        ),
        PlacementTarget::Panel => (
            if edge == TaskbarEdge::Left {
                left
            } else {
                right - width
            },
            if edge == TaskbarEdge::Top {
                top
            } else {
                bottom - height
            },
        ),
        PlacementTarget::TrayMenu => {
            let gap = scale(16);
            let (x, y) = match edge {
                TaskbarEdge::Left => (anchor.right + gap, anchor.bottom - height),
                TaskbarEdge::Top => (anchor.right - width, anchor.bottom + gap),
                TaskbarEdge::Right => (anchor.left - gap - width, anchor.bottom - height),
                TaskbarEdge::Bottom => (anchor.right - width, anchor.top - gap - height),
            };
            (
                x.clamp(left, (right - width).max(left)),
                y.clamp(top, (bottom - height).max(top)),
            )
        }
    };
    Placement {
        x,
        y,
        width,
        height,
        dpi,
        edge,
        work_area: WorkArea {
            left: work.left,
            top: work.top,
            right: work.right,
            bottom: work.bottom,
        },
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
        let compact = place(
            work,
            432,
            100,
            96,
            PlacementTarget::Panel,
            work,
            TaskbarEdge::Bottom,
        );
        let settings = place(
            work,
            432,
            640,
            96,
            PlacementTarget::Panel,
            work,
            TaskbarEdge::Bottom,
        );
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
        let compact = place(
            work,
            432,
            100,
            144,
            PlacementTarget::Panel,
            work,
            TaskbarEdge::Bottom,
        );
        let settings = place(
            work,
            432,
            1200,
            144,
            PlacementTarget::Panel,
            work,
            TaskbarEdge::Bottom,
        );
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
        let menu = place(
            work,
            190,
            110,
            144,
            PlacementTarget::TrayMenu,
            icon,
            TaskbarEdge::Bottom,
        );
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
        let menu = place(
            work,
            190,
            110,
            96,
            PlacementTarget::TrayMenu,
            icon,
            TaskbarEdge::Top,
        );
        assert_eq!((menu.x, menu.y), (1724, 66));
    }

    #[test]
    fn hidden_taskbars_keep_the_same_docking_boundary_as_revealed_taskbars() {
        let screen = RECT {
            left: -1920,
            top: -1080,
            right: 0,
            bottom: 0,
        };
        for (edge, revealed, hidden, expected_panel) in [
            (
                TaskbarEdge::Left,
                RECT {
                    right: -1872,
                    ..screen
                },
                RECT {
                    left: -1966,
                    right: -1918,
                    ..screen
                },
                (-1860, -112),
            ),
            (
                TaskbarEdge::Top,
                RECT {
                    bottom: -1032,
                    ..screen
                },
                RECT {
                    top: -1126,
                    bottom: -1078,
                    ..screen
                },
                (-444, -1020),
            ),
            (
                TaskbarEdge::Right,
                RECT {
                    left: -48,
                    ..screen
                },
                RECT {
                    left: -2,
                    right: 46,
                    ..screen
                },
                (-492, -112),
            ),
            (
                TaskbarEdge::Bottom,
                RECT { top: -48, ..screen },
                RECT {
                    top: -2,
                    bottom: 46,
                    ..screen
                },
                (-444, -160),
            ),
        ] {
            assert_eq!(TaskbarEdge::from_rect(screen, revealed), edge);
            assert_eq!(TaskbarEdge::from_rect(screen, hidden), edge);
            let work = edge.reserve(screen, screen, revealed);
            assert_eq!(edge.reserve(screen, screen, hidden), work);
            assert_eq!(TaskbarEdge::from_work_area(screen, work), edge);
            let panel = place(work, 432, 100, 96, PlacementTarget::Panel, revealed, edge);
            assert_eq!((panel.x, panel.y), expected_panel);
        }
    }

    #[test]
    fn side_tray_menus_leave_room_for_the_slide_toward_the_taskbar() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        for (edge, bar, expected_x) in [
            (
                TaskbarEdge::Left,
                RECT {
                    right: 48,
                    ..screen
                },
                66,
            ),
            (
                TaskbarEdge::Right,
                RECT {
                    left: 1872,
                    ..screen
                },
                1664,
            ),
        ] {
            let work = edge.reserve(screen, screen, bar);
            let icon = RECT {
                top: 980,
                bottom: 1012,
                ..bar
            };
            let menu = place(work, 190, 110, 96, PlacementTarget::TrayMenu, icon, edge);
            assert_eq!((menu.x, menu.y), (expected_x, 902));
        }
    }
}
