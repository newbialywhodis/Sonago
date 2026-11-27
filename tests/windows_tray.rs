#![cfg(windows)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use sonago_core::Result;
use windows::{
    Win32::{
        Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey, VK_F9,
            },
            WindowsAndMessaging::{
                FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE, WM_HOTKEY,
            },
        },
    },
    core::{HRESULT, w},
};

#[test]
#[ignore = "requires an interactive Windows desktop, a free Ctrl+Alt+F9, and ffprobe"]
fn tray_hotkey_message_exports_mix_and_two_stems() -> Result<()> {
    let class = w!("SonagoTrayWindow");
    assert!(
        unsafe { FindWindowW(class, None) }.is_err(),
        "close the running Sonago instance"
    );
    let register = || unsafe { RegisterHotKey(None, 1, MOD_CONTROL | MOD_ALT, u32::from(VK_F9.0)) };
    register()?;
    unsafe { UnregisterHotKey(None, 1)? };

    let output = tempfile::tempdir()?;
    let settings = output.path().join("Sonago");
    std::fs::create_dir(&settings)?;
    let models = std::env::var_os("SONAGO_TEST_MODELS");
    if let Some(models) = &models {
        let cache = settings.join("models/v1");
        std::fs::create_dir_all(&cache)?;
        for entry in std::fs::read_dir(models)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && entry.file_name() != "worker.lock" {
                std::fs::hard_link(entry.path(), cache.join(entry.file_name()))?;
            }
        }
    }
    std::fs::write(
        settings.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "sources": [
                {"enabled": true, "endpoint": null},
                {"enabled": false, "endpoint": null}
            ],
            "retention": 5,
            "minutes": 5,
            "output": output.path(),
            "format": "matroska",
            "hotkey": "Ctrl+Alt+F9",
            "autostart": false,
            "notifications": false,
            "transcribe": models.is_some()
        }))?,
    )?;
    let executable = std::env::var_os("SONAGO_WINUI_EXE").map_or_else(
        || {
            let runtime = if cfg!(target_arch = "aarch64") {
                "win-arm64"
            } else {
                "win-x64"
            };
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/winui")
                .join(runtime)
                .join("Release/app/sonago.exe")
        },
        PathBuf::from,
    );
    assert!(
        executable.is_file(),
        "Build the WinUI Release app or set SONAGO_WINUI_EXE"
    );
    let mut process = AppProcess(
        Command::new(&executable)
            .env("LOCALAPPDATA", output.path())
            .spawn()?,
    );
    let hwnd = wait_for(|| {
        let hwnd = unsafe { FindWindowW(class, None) }.ok()?;
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (pid == process.0.id()).then_some(hwnd)
    })
    .ok_or("The tray window did not start")?;

    let mut duplicate = AppProcess(
        Command::new(&executable)
            .env("LOCALAPPDATA", output.path())
            .spawn()?,
    );
    assert!(
        wait_for(|| duplicate.0.try_wait().ok().flatten())
            .ok_or("The duplicate instance did not exit")?
            .success()
    );

    thread::sleep(Duration::from_secs(3));
    let error = match register() {
        Ok(()) => {
            unsafe { UnregisterHotKey(None, 1)? };
            return Err("Sonago did not register the expected shortcut".into());
        }
        Err(error) => error,
    };
    assert_eq!(
        error.code(),
        HRESULT::from_win32(ERROR_HOTKEY_ALREADY_REGISTERED.0)
    );
    unsafe { PostMessageW(Some(hwnd), WM_HOTKEY, WPARAM(1), LPARAM(0))? };
    let clip = wait_for(|| find_clip(output.path()))
        .ok_or("Hotkey message did not publish a recording")?;
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,channels:stream_disposition=default",
            "-of",
            "csv=p=0",
        ])
        .arg(&clip)
        .output()?;
    assert!(probe.status.success());
    assert_eq!(
        String::from_utf8(probe.stdout)?.lines().collect::<Vec<_>>(),
        ["opus,2,1", "opus,2,0", "opus,1,0"]
    );
    let mut transcripts = Vec::new();
    if models.is_some() {
        unsafe { PostMessageW(Some(hwnd), WM_HOTKEY, WPARAM(1), LPARAM(0))? };
        let second = wait_for(|| {
            std::fs::read_dir(output.path())
                .ok()?
                .flatten()
                .map(|f| f.path())
                .find(|p| p != &clip && p.extension().is_some_and(|e| e == "mka"))
        })
        .ok_or("Second save did not finish while transcription was pending")?;
        transcripts.extend([clip.with_extension("txt"), second.with_extension("txt")]);
    }
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0))? };
    let status = wait_for(|| process.0.try_wait().ok().flatten())
        .ok_or("Application did not exit normally")?;
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(120);
    for transcript in transcripts {
        while !transcript.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        assert!(!std::fs::read_to_string(transcript)?.trim().is_empty());
    }
    Ok(())
}

fn find_clip(directory: &Path) -> Option<PathBuf> {
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|item| item.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "mka"))
}

fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if start.elapsed() >= Duration::from_secs(5) {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct AppProcess(Child);

impl Drop for AppProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
