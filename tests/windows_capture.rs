#![cfg(windows)]

use std::{
    process::Command,
    sync::{atomic::Ordering::Relaxed, mpsc},
    thread,
    time::{Duration, Instant},
};

use hearlier_core::{
    Result,
    engine::{Config, Engine, Format, Notice},
    wasapi_input::{Source, WasapiInput},
};
use windows::{
    Win32::{
        Foundation::FILETIME,
        Media::Audio::{PlaySoundW, SND_ASYNC, SND_LOOP, SND_MEMORY, SND_NODEFAULT, SND_SYNC},
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    },
    core::PCWSTR,
};

#[test]
#[ignore = "uses default Windows audio endpoints and optionally plays a quiet test tone"]
fn capture_live() -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let mix_formats = (|| -> Result<String> {
        let devices = wasapi::DeviceEnumerator::new()?;
        for direction in [wasapi::Direction::Render, wasapi::Direction::Capture] {
            let device = devices.get_default_device_for_role(direction, wasapi::Role::Console)?;
            let wave = device.get_iaudioclient()?.get_mixformat()?;
            println!(
                "{direction:?} endpoint: {} Hz, {} ch, {} valid bits, {:?}, mask {:#x}",
                wave.get_samplespersec(),
                wave.get_nchannels(),
                wave.get_validbitspersample(),
                wave.get_subformat()?,
                wave.get_dwchannelmask()
            );
        }
        Ok(devices
            .get_default_device(wasapi::Direction::Render)?
            .get_id()?)
    })();
    wasapi::deinitialize();
    let default_output = mix_formats?;
    let output = tempfile::tempdir()?;
    let (notices, events) = mpsc::channel();
    let (engine, input) = Engine::start(
        Config {
            history_minutes: 5,
            output: output.path().into(),
        },
        move |notice| {
            let _ = notices.send(notice);
        },
    )?;
    let (failed, errors) = mpsc::channel();
    let capture = WasapiInput::start(input, move |error| {
        let _ = failed.send(error);
    })?;
    let wave = if std::env::var_os("RECALL_LIVE_TONE").is_some() {
        let rate = std::env::var("RECALL_LIVE_RATE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(48_000);
        println!("Playing 997 Hz from {rate} Hz mono PCM");
        Some(Playback::start(rate)?)
    } else {
        None
    };

    thread::sleep(Duration::from_secs(2));
    if let Ok(err) = errors.try_recv() {
        return Err(err.into());
    }
    let stats = engine.stats();
    let starts = stats
        .tracks
        .each_ref()
        .map(|track| track.captured.load(Relaxed));
    let seconds = std::env::var("RECALL_LIVE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15);
    let cpu_start = cpu_time()?;
    let wall = Instant::now();
    thread::sleep(Duration::from_secs(seconds));
    let wall = wall.elapsed().as_secs_f64();
    let cpu = (cpu_time()? - cpu_start) as f64 / 10_000_000.0;
    for (index, track) in stats.tracks.iter().enumerate() {
        let frames = track.captured.load(Relaxed) - starts[index];
        let dropped = track.dropped.load(Relaxed);
        println!(
            "track {index}: {frames} captured samples/channel; {dropped} queue drops; {} late; {} driver discontinuities; {} bad timestamps; {} clock resets; {} starts; {} failures",
            track.late.load(Relaxed),
            track.discontinuities.load(Relaxed),
            track.invalid_timestamps.load(Relaxed),
            track.clock_resets.load(Relaxed),
            track.starts.load(Relaxed),
            track.errors.load(Relaxed)
        );
        assert!(
            track.available.load(Relaxed),
            "endpoint {index} unavailable"
        );
        assert_eq!(dropped, 0, "capture queue overflow");
        if index == 1 || wave.is_some() {
            assert!(
                frames > seconds * 40_000,
                "endpoint {index} is delivering too few samples"
            );
        }
    }
    let memory = memory()?;
    println!(
        "wall {wall:.3} s; process CPU {cpu:.4} s ({:.2}% of one logical core); working set {:.2} MiB; peak {:.2} MiB",
        cpu / wall * 100.0,
        memory.WorkingSetSize as f64 / 1_048_576.0,
        memory.PeakWorkingSetSize as f64 / 1_048_576.0
    );

    engine.save(Duration::from_secs(2), Format::Matroska)?;
    let path = match events.recv_timeout(Duration::from_secs(5))? {
        Notice::Saved(path) => path,
        Notice::SaveFailed(err) | Notice::EngineError(err) | Notice::DiskError(err) => {
            return Err(err.into());
        }
    };
    if wave.is_some() {
        let pcm = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:a:0", "-f", "s16le", "-"])
            .output()?;
        assert!(pcm.status.success());
        assert_eq!(pcm.stdout.len(), 2 * 48_000 * 2 * 2);
        assert!(
            pcm.stdout
                .as_chunks::<2>()
                .0
                .iter()
                .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > 8)
        );
    }
    let starts = stats
        .tracks
        .each_ref()
        .map(|track| track.starts.load(Relaxed));
    let pauses = stats.pauses.load(Relaxed);
    capture.resume();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let restarted = stats.tracks.iter().zip(starts).all(|(track, before)| {
            track.starts.load(Relaxed) > before && track.available.load(Relaxed)
        });
        if restarted && stats.pauses.load(Relaxed) > pauses {
            println!("Resume handler reopened both WASAPI clients and rebased history");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "resume did not reopen both streams"
        );
        thread::sleep(Duration::from_millis(20));
    }
    if let Ok(err) = errors.try_recv() {
        return Err(err.into());
    }
    let output_source = Source {
        enabled: true,
        endpoint: None,
    };
    let off = Source {
        enabled: false,
        endpoint: None,
    };
    let before = stats.tracks[0].starts.load(Relaxed);
    capture.set_sources([output_source.clone(), off.clone()]);
    until(|| !stats.tracks[1].available.load(Relaxed))?;
    assert!(stats.tracks[0].available.load(Relaxed));
    assert_eq!(stats.tracks[0].starts.load(Relaxed), before);

    capture.set_sources([
        Source {
            enabled: true,
            endpoint: Some(default_output),
        },
        off.clone(),
    ]);
    until(|| {
        stats.tracks[0].starts.load(Relaxed) > before && stats.tracks[0].available.load(Relaxed)
    })?;
    assert!(!stats.tracks[1].available.load(Relaxed));

    capture.set_sources([off.clone(), off]);
    until(|| {
        stats
            .tracks
            .iter()
            .all(|track| !track.available.load(Relaxed))
    })?;
    thread::sleep(Duration::from_millis(300));
    let retained = stats.buffered();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(stats.buffered(), retained);
    engine.save(Duration::from_secs(1), Format::Matroska)?;
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5))?,
        Notice::Saved(_)
    ));
    capture.set_sources([
        output_source,
        Source {
            enabled: false,
            endpoint: None,
        },
    ]);
    until(|| stats.buffered() > retained)?;
    println!(
        "Source switching kept the other input running. Paused history stayed exportable and resumed."
    );
    drop(wave);
    drop(capture);
    drop(engine);
    Ok(())
}

fn until(mut condition: impl FnMut() -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        if Instant::now() >= deadline {
            return Err("Timed out waiting for the audio state to change".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn cpu_time() -> Result<u64> {
    let [mut creation, mut exit, mut kernel, mut user] = [FILETIME::default(); 4];
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )?
    };
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Ok(ticks(kernel) + ticks(user))
}

fn memory() -> Result<PROCESS_MEMORY_COUNTERS> {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = std::mem::size_of_val(&counters) as u32;
    counters.cb = size;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).ok()? };
    Ok(counters)
}

struct Playback(Vec<u8>);

impl Playback {
    fn start(samples: u32) -> Result<Self> {
        let bytes = samples * 2;
        let mut wave = Vec::with_capacity(bytes as usize + 44);
        wave.extend_from_slice(b"RIFF");
        wave.extend_from_slice(&(bytes + 36).to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        wave.extend_from_slice(&samples.to_le_bytes());
        wave.extend_from_slice(&bytes.to_le_bytes());
        wave.extend_from_slice(b"\x02\0\x10\0data");
        wave.extend_from_slice(&bytes.to_le_bytes());
        for i in 0..samples {
            let phase = std::f64::consts::TAU * 997.0 * f64::from(i) / f64::from(samples);
            wave.extend_from_slice(&((1200.0 * phase.sin()) as i16).to_le_bytes());
        }
        let playback = Self(wave);
        // SND_MEMORY reads WAV bytes until playback is explicitly stopped in Drop.
        let data = PCWSTR(playback.0.as_ptr().cast());
        unsafe {
            PlaySoundW(
                data,
                None,
                SND_MEMORY | SND_NODEFAULT | SND_ASYNC | SND_LOOP,
            )
            .ok()?
        };
        Ok(playback)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        let _ = unsafe { PlaySoundW(PCWSTR::null(), None, SND_SYNC) };
    }
}
