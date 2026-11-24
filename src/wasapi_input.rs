//! Windows adapter. The engine only receives timestamped PCM blocks.

mod clock;
mod stream;

use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use wasapi::{DeviceEnumerator, DeviceEventCallbacks, Direction, Role};
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_FAILED, WAIT_TIMEOUT},
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                SetEvent, WaitForMultipleObjects,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{
    CHANNELS, Result,
    engine::{CaptureInput, Stats, TrackStats, frames},
};
use stream::Stream;

const RETRY: Duration = Duration::from_secs(2);

pub struct WasapiInput {
    control: Arc<Control>,
    stats: Arc<Stats>,
    clock: Instant,
    thread: Option<JoinHandle<()>>,
}

struct Control {
    stop: Event,
    changed: Event,
    mask: AtomicU8,
    sources: Mutex<[Source; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Source {
    pub enabled: bool,
    pub endpoint: Option<String>,
}

pub struct Endpoint {
    pub id: String,
    pub name: String,
}

pub enum CaptureEvent {
    Ready(usize),
    Unavailable(usize, String),
    Failed(String),
}

fn direction(track: usize) -> Direction {
    [Direction::Render, Direction::Capture][track]
}

/// Enumerate on a COM-initialized control thread. The capture thread owns its own clients.
pub fn endpoints() -> Result<[Vec<Endpoint>; 2]> {
    let devices = DeviceEnumerator::new()?;
    let list = |track| -> Result<Vec<Endpoint>> {
        let collection = devices.get_device_collection(direction(track))?;
        let mut entries = Vec::new();
        for index in 0..collection.get_nbr_devices()? {
            // The device may disappear between enumeration and the property read.
            let entry = (|| -> Result<Endpoint> {
                let device = collection.get_device_at_index(index)?;
                Ok(Endpoint {
                    id: device.get_id()?,
                    name: device.get_friendlyname()?,
                })
            })();
            entries.extend(entry);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(entries)
    };
    Ok([list(0)?, list(1)?])
}

impl Control {
    fn restart(&self, mask: u8) {
        self.mask.fetch_or(mask, Ordering::Release);
        self.changed.signal();
    }
}

impl WasapiInput {
    pub fn start(input: CaptureInput, error: impl Fn(String) + Send + 'static) -> Result<Self> {
        Self::start_with(
            input,
            [(); 2].map(|()| Source {
                enabled: true,
                endpoint: None,
            }),
            move |event| match event {
                CaptureEvent::Unavailable(_, message) | CaptureEvent::Failed(message) => {
                    error(message)
                }
                CaptureEvent::Ready(_) => {}
            },
        )
    }

    pub fn start_with(
        input: CaptureInput,
        sources: [Source; 2],
        notify: impl Fn(CaptureEvent) + Send + 'static,
    ) -> Result<Self> {
        let control = Arc::new(Control {
            stop: Event::new()?,
            changed: Event::new()?,
            mask: AtomicU8::new(0),
            sources: Mutex::new(sources),
        });
        let worker = control.clone();
        let stats = input.stats.clone();
        let clock = input.clock;
        let thread = thread::Builder::new()
            .name("hearlier-wasapi".into())
            .spawn(move || {
                let stats = input.stats.clone();
                let result = capture(input, &worker, &notify);
                for track in &stats.tracks {
                    track.available.store(false, Ordering::Relaxed);
                }
                if let Err(err) = result {
                    notify(CaptureEvent::Failed(err.to_string()));
                }
            })?;
        Ok(Self {
            control,
            stats,
            clock,
            thread: Some(thread),
        })
    }

    pub fn set_sources(&self, sources: [Source; 2]) {
        let mut current = self.control.sources.lock().unwrap();
        let mut changed = 0;
        for track in 0..2 {
            if current[track] != sources[track] {
                changed |= 1 << track;
            }
        }
        if changed != 0 {
            *current = sources;
            self.control.restart(changed);
        }
    }

    pub fn resume(&self) {
        self.stats
            .resume_at
            .store(frames(self.clock.elapsed()).max(1), Ordering::Relaxed);
        self.control.restart(0b11);
    }
}

impl Drop for WasapiInput {
    fn drop(&mut self) {
        self.control.stop.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Event(OwnedHandle);

impl Event {
    fn new() -> Result<Self> {
        // CreateEvent transfers ownership of the new kernel object to this wrapper.
        let raw = unsafe { CreateEventW(None, false, false, PCWSTR::null())? };
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(raw.0) }))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
    fn signal(&self) {
        let _ = unsafe { SetEvent(self.handle()) };
    }
}

struct Com;

impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

struct Mmcss(HANDLE);

impl Drop for Mmcss {
    fn drop(&mut self) {
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

fn capture(
    mut input: CaptureInput,
    control: &Arc<Control>,
    notify: &impl Fn(CaptureEvent),
) -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let _com = Com;
    let mut index = 0;
    let _priority = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut index) }
        .ok()
        .map(Mmcss);
    let origin = qpc_origin(input.clock)?;
    let devices = DeviceEnumerator::new()?;
    let mut callbacks = DeviceEventCallbacks::new();
    let callback = control.clone();
    callbacks.set_default_device_callback(move |direction, role, _| {
        if role == Role::Console {
            let bit = if direction == Direction::Render { 1 } else { 2 };
            callback.restart(bit << 2);
        }
    });
    let _registration = devices.register_notification_callback(callbacks)?;
    let mut next_retry = [Instant::now(); 2];
    let mut streams = [None, None];
    let mut last_error = [None, None];
    let mut sources = control.sources.lock().unwrap().clone();
    loop {
        for track in 0..2 {
            let source = &sources[track];
            if source.enabled && streams[track].is_none() && Instant::now() >= next_retry[track] {
                let stats = &input.stats.tracks[track];
                match Stream::open(
                    &devices,
                    direction(track),
                    CHANNELS[track],
                    source.endpoint.as_deref(),
                ) {
                    Ok(stream) => {
                        streams[track] = Some(stream);
                        stats.starts.fetch_add(1, Ordering::Relaxed);
                        stats.available.store(true, Ordering::Relaxed);
                        last_error[track] = None;
                        notify(CaptureEvent::Ready(track));
                    }
                    Err(err) => report_error(track, stats, &mut last_error[track], err, notify),
                }
                next_retry[track] = Instant::now() + RETRY;
            }
        }

        let mut events = [
            control.stop.handle(),
            control.changed.handle(),
            HANDLE::default(),
            HANDLE::default(),
        ];
        let mut count = 2;
        let mut timeout = if sources.iter().any(|source| source.enabled) {
            RETRY.as_millis() as u32
        } else {
            u32::MAX
        };
        for (track, stream) in streams.iter().enumerate() {
            if let Some(stream) = stream {
                events[count] = stream.event.as_handle();
                count += 1;
            } else if sources[track].enabled {
                let wait = next_retry[track].saturating_duration_since(Instant::now());
                timeout = timeout.min(wait.as_millis() as u32 + 1);
            }
        }
        // These handles remain valid throughout the wait. This thread owns both WASAPI clients.
        let res = unsafe { WaitForMultipleObjects(&events[..count], false, timeout) };
        match res.0 as usize {
            0 => return Ok(()),
            1 => {
                let changed = control.mask.swap(0, Ordering::Acquire);
                if changed & 0b11 != 0 {
                    sources = control.sources.lock().unwrap().clone();
                }
                for track in 0..2 {
                    let follows_default =
                        sources[track].enabled && sources[track].endpoint.is_none();
                    if changed & (1 << track) != 0 || follows_default && changed & (4 << track) != 0
                    {
                        streams[track] = None;
                        last_error[track] = None;
                        next_retry[track] = Instant::now();
                        input.stats.tracks[track]
                            .available
                            .store(false, Ordering::Relaxed);
                    }
                }
                continue;
            }
            _ if res == WAIT_FAILED => return Err(windows::core::Error::from_thread().into()),
            index if res == WAIT_TIMEOUT || index < count => {}
            _ => return Err("Nieoczekiwany wynik oczekiwania na WASAPI.".into()),
        }
        // Poll both clients even if only one signalled. This also detects a
        // failed, silent endpoint and avoids priority by handle order.
        for track in 0..2 {
            let Some(stream) = streams[track].as_mut() else {
                continue;
            };
            let stats = &input.stats.tracks[track];
            let result = stream.drain(&mut input.tracks[track], stats, origin, input.clock);
            match result {
                Ok(true) => last_error[track] = None,
                Ok(false) if track == 0 || stream.last_packet.elapsed() < RETRY => {}
                result => {
                    let err = result
                        .err()
                        .unwrap_or_else(|| "Mikrofon przestał dostarczać próbki.".into());
                    streams[track] = None;
                    stats.available.store(false, Ordering::Relaxed);
                    report_error(track, stats, &mut last_error[track], err, notify);
                    next_retry[track] = Instant::now() + RETRY;
                }
            }
        }
    }
}

fn report_error(
    track: usize,
    stats: &TrackStats,
    last: &mut Option<String>,
    err: crate::Error,
    notify: &impl Fn(CaptureEvent),
) {
    stats.errors.fetch_add(1, Ordering::Relaxed);
    let message = err.to_string();
    if last.as_ref() != Some(&message) {
        notify(CaptureEvent::Unavailable(track, message.clone()));
        *last = Some(message);
    }
}

fn qpc_origin(clock: Instant) -> Result<i64> {
    let (mut frequency, mut ticks) = (0, 0);
    unsafe {
        QueryPerformanceFrequency(&mut frequency)?;
        QueryPerformanceCounter(&mut ticks)?;
    }
    Ok(
        ((i128::from(ticks) * 10_000_000) / i128::from(frequency)) as i64
            - (clock.elapsed().as_nanos() / 100) as i64,
    )
}
