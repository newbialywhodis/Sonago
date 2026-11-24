use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rtrb::{Producer, RingBuffer};

pub use crate::mux::Format;
use crate::{
    CHANNELS, FRAME, FRAME_MS, FRAMES_PER_SECOND, Result,
    codec::Encoders,
    history::{History, RAM_FRAMES, Snapshot},
    mux,
    timeline::{CaptureBlock, Timeline},
};

pub const MAX_HISTORY_MINUTES: u64 = 720;
const QUEUE_BLOCKS: usize = 256;
const GRACE: Duration = Duration::from_millis(180);
const TICK: Duration = Duration::from_millis(100);

pub struct CaptureInput {
    pub(crate) clock: Instant,
    pub(crate) stats: Arc<Stats>,
    pub(crate) tracks: [Producer<CaptureBlock>; 2],
}

#[derive(Default)]
pub struct TrackStats {
    pub available: AtomicBool,
    pub captured: AtomicU64,
    pub dropped: AtomicU64,
    pub late: AtomicU64,
    pub discontinuities: AtomicU64,
    pub invalid_timestamps: AtomicU64,
    pub clock_resets: AtomicU64,
    pub starts: AtomicU64,
    pub errors: AtomicU64,
}

#[derive(Default)]
pub struct Stats {
    pub tracks: [TrackStats; 2],
    pub recording: AtomicBool,
    pub saving: AtomicBool,
    pub disk_errors: AtomicU64,
    pub disk_degraded: AtomicBool,
    pub pauses: AtomicU64,
    pub(crate) resume_at: AtomicU64,
    retention: AtomicU64,
    buffered_frames: AtomicU64,
}

impl Stats {
    pub fn buffered(&self) -> Duration {
        Duration::from_millis(self.buffered_frames.load(Relaxed) * FRAME_MS)
    }
}

pub struct Config {
    pub history_minutes: u64,
    pub output: PathBuf,
}

pub enum Notice {
    Saved(PathBuf),
    SaveFailed(String),
    DiskError(String),
    EngineError(String),
}

type Notification = Arc<dyn Fn(Notice) + Send + Sync>;

struct Request {
    end: u64,
    length: u64,
    format: Format,
    output: PathBuf,
}

enum Command {
    Export(Request),
    Stop,
}

pub struct Engine {
    commands: SyncSender<Command>,
    clock: Instant,
    stats: Arc<Stats>,
    output: PathBuf,
    worker: Option<JoinHandle<()>>,
    exporter: Option<JoinHandle<()>>,
    disk: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(
        config: Config,
        notify: impl Fn(Notice) + Send + Sync + 'static,
    ) -> Result<(Self, CaptureInput)> {
        if !(1..=MAX_HISTORY_MINUTES).contains(&config.history_minutes) {
            return Err(format!("Historia musi mieć od 1 do {MAX_HISTORY_MINUTES} min.").into());
        }
        let encoders = Encoders::new()?;
        let retention = config.history_minutes * 60 * FRAMES_PER_SECOND;
        let (history, disk) = History::new(retention, RAM_FRAMES)?;
        let notify: Notification = Arc::new(notify);
        let stats = Arc::new(Stats::default());
        stats.retention.store(retention, Relaxed);
        let clock = Instant::now();
        let (render, out) = RingBuffer::new(QUEUE_BLOCKS);
        let (mic, input) = RingBuffer::new(QUEUE_BLOCKS);
        let (commands, receive) = mpsc::sync_channel(2);
        let (jobs, exports) = mpsc::sync_channel::<(Snapshot, Request)>(1);

        let export_notice = notify.clone();
        let export_stats = stats.clone();
        let exporter = thread::Builder::new()
            .name("hearlier-export".into())
            .spawn(move || {
                for (snapshot, request) in exports {
                    let result = mux::save(&snapshot, &request.output, request.format);
                    export_stats.saving.store(false, Relaxed);
                    report_save(&export_notice, result);
                }
            })?;

        let worker_stats = stats.clone();
        let worker = thread::Builder::new()
            .name("hearlier-encode".into())
            .spawn(move || {
                worker_stats.recording.store(true, Relaxed);
                let mut recorder = Recorder {
                    timelines: [Timeline::new(out), Timeline::new(input)],
                    encoders,
                    history,
                    clock,
                    stats: worker_stats.clone(),
                    notify: notify.clone(),
                };
                let result = recorder.run(receive, jobs);
                worker_stats.recording.store(false, Relaxed);
                if let Err(err) = result {
                    worker_stats.saving.store(false, Relaxed);
                    notify(Notice::EngineError(err.to_string()));
                }
            })?;
        let capture = CaptureInput {
            tracks: [render, mic],
            clock,
            stats: stats.clone(),
        };
        let engine = Self {
            commands,
            clock,
            stats,
            output: config.output,
            worker: Some(worker),
            exporter: Some(exporter),
            disk,
        };
        Ok((engine, capture))
    }

    pub fn save(&self, duration: Duration, format: Format) -> Result<()> {
        self.save_to(duration, format, self.output.clone())
    }

    pub fn save_to(&self, duration: Duration, format: Format, output: PathBuf) -> Result<()> {
        let length = (duration.as_millis() / u128::from(FRAME_MS))
            .min(u128::from(MAX_HISTORY_MINUTES * 60 * FRAMES_PER_SECOND))
            as u64;
        if length == 0 {
            return Err("Zakres zapisu musi wynosić co najmniej 20 ms.".into());
        }
        if self.stats.saving.swap(true, Relaxed) {
            return Err("The previous clip is still saving.".into());
        }
        let end = frames(self.clock.elapsed());
        if self
            .commands
            .try_send(Command::Export(Request {
                end,
                length,
                format,
                output,
            }))
            .is_err()
        {
            self.stats.saving.store(false, Relaxed);
            return Err("The recording engine is unavailable.".into());
        }
        Ok(())
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    pub fn set_retention(&self, minutes: u64) {
        self.stats.retention.store(
            minutes.clamp(1, MAX_HISTORY_MINUTES) * 60 * FRAMES_PER_SECOND,
            Relaxed,
        );
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(exporter) = self.exporter.take() {
            let _ = exporter.join();
        }
        if let Some(disk) = self.disk.take() {
            let _ = disk.join();
        }
    }
}

struct Recorder {
    timelines: [Timeline; 2],
    encoders: Encoders,
    history: History,
    clock: Instant,
    stats: Arc<Stats>,
    notify: Notification,
}

impl Recorder {
    fn run(
        &mut self,
        commands: Receiver<Command>,
        jobs: SyncSender<(Snapshot, Request)>,
    ) -> Result<()> {
        let mut pending = None::<Request>;
        let mut tick = Instant::now();
        let mut pcm = [[0; FRAME * 2]; 2];
        let mut stop = false;
        let mut sequence = SequenceClock::default();
        let mut active = false;
        loop {
            let now = Instant::now();
            if now >= tick {
                tick = now + TICK;
                let raw_ready = frames(self.clock.elapsed().saturating_sub(GRACE));
                let capturing = self
                    .stats
                    .tracks
                    .iter()
                    .any(|track| track.available.load(Relaxed));
                let resumed = self.stats.resume_at.swap(0, Relaxed);
                if (resumed != 0 && sequence.skip(resumed, self.history.end()))
                    || sequence.skip_long_pause(raw_ready, self.history.end())
                {
                    self.stats.pauses.fetch_add(1, Relaxed);
                }
                if !capturing || !active {
                    sequence.skip(raw_ready, self.history.end());
                }
                active = capturing;
                let ready = raw_ready.saturating_sub(sequence.skipped);
                let retention = self.stats.retention.load(Relaxed);
                self.history.set_retention(retention);
                match self.history.poll_disk() {
                    Ok(true) => self.stats.disk_degraded.store(false, Relaxed),
                    Ok(false) => {}
                    Err(err) => self.report_disk_error(err),
                }
                if retention <= RAM_FRAMES && self.stats.disk_degraded.load(Relaxed) {
                    self.stats.disk_degraded.store(false, Relaxed);
                }
                for _ in 0..ready.saturating_sub(self.history.end()).min(10) {
                    let start = ((self.history.end() + sequence.skipped) * FRAME as u64) as i64;
                    for (track, timeline) in self.timelines.iter_mut().enumerate() {
                        let late = timeline.fill(start, CHANNELS[track], &mut pcm[track]);
                        if late > 0 {
                            self.stats.tracks[track].late.fetch_add(late, Relaxed);
                        }
                    }
                    if let Err(err) = self.history.push(self.encoders.encode(&pcm)?) {
                        self.report_disk_error(err);
                    }
                }
                if self.history.end() < ready {
                    // Yield to commands before another batch when catching up.
                    tick = Instant::now();
                }
                self.stats
                    .buffered_frames
                    .store(self.history.available(), Relaxed);
            }
            if let Some(end) = pending
                .as_ref()
                .map(|request| sequence.project(request.end))
                .map(|end| {
                    if active {
                        end
                    } else {
                        end.min(self.history.end().saturating_sub(1))
                    }
                })
                .filter(|end| !active || *end < self.history.end())
            {
                let request = pending.take().unwrap();
                let result = self
                    .history
                    .snapshot(end, request.length, self.encoders.delays)
                    .and_then(|snapshot| {
                        jobs.try_send((snapshot, request))
                            .map_err(|_| "Eksporter jest niedostępny.".into())
                    });
                if let Err(err) = result {
                    self.stats.saving.store(false, Relaxed);
                    (self.notify)(Notice::SaveFailed(err.to_string()));
                }
            }
            if stop && pending.is_none() {
                return Ok(());
            }
            if stop {
                thread::sleep(tick.saturating_duration_since(Instant::now()));
                continue;
            }
            match commands.recv_timeout(tick.saturating_duration_since(Instant::now())) {
                Ok(Command::Export(request)) => pending = Some(request),
                Ok(Command::Stop) => stop = true,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    fn report_disk_error(&self, err: std::io::Error) {
        self.stats.disk_errors.fetch_add(1, Relaxed);
        if !self.stats.disk_degraded.swap(true, Relaxed) {
            (self.notify)(Notice::DiskError(err.to_string()));
        }
    }
}

#[derive(Default)]
struct SequenceClock {
    skipped: u64,
    previous_skip: u64,
    seam: u64,
}

impl SequenceClock {
    fn project(&self, raw: u64) -> u64 {
        raw.saturating_sub(self.previous_skip)
            .min(self.seam)
            .max(raw.saturating_sub(self.skipped))
    }

    fn skip_long_pause(&mut self, raw: u64, encoded: u64) -> bool {
        self.project(raw).saturating_sub(encoded) > 30 * FRAMES_PER_SECOND
            && self.skip(raw, encoded)
    }

    fn skip(&mut self, raw: u64, encoded: u64) -> bool {
        let backlog = raw.saturating_sub(self.skipped + encoded);
        if backlog == 0 {
            return false;
        }
        self.previous_skip = self.skipped;
        self.skipped += backlog;
        self.seam = encoded;
        true
    }
}

pub(crate) fn frames(elapsed: Duration) -> u64 {
    (elapsed.as_millis() / u128::from(FRAME_MS)) as u64
}

fn report_save(notify: &Notification, result: Result<PathBuf>) {
    match result {
        Ok(path) => notify(Notice::Saved(path)),
        Err(err) => {
            let message = match err.downcast_ref::<std::io::Error>().map(std::io::Error::kind) {
                Some(std::io::ErrorKind::StorageFull) => "The recordings drive is full. Free some space or choose a different folder.".into(),
                Some(std::io::ErrorKind::PermissionDenied) => "hearlier cannot write to the recordings folder. Choose a writable folder in Settings.".into(),
                _ => err.to_string(),
            };
            notify(Notice::SaveFailed(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_pause_keeps_existing_history_and_rebases_pending_request() {
        let mut clock = SequenceClock::default();
        assert!(!clock.skip_long_pause(500, 490));
        assert_eq!(clock.project(500), 500);
        assert!(clock.skip_long_pause(30_000, 490));
        assert_eq!(clock.project(489), 489);
        assert_eq!(clock.project(499), 490);
        assert_eq!(clock.project(30_100), 590);
    }

    #[test]
    fn explicit_resume_skips_short_sleep_and_keeps_precise_save_end() {
        let mut clock = SequenceClock::default();
        assert!(clock.skip(750, 500));
        assert_eq!(clock.project(497), 497);
        assert_eq!(clock.project(749), 500);
        assert_eq!(clock.project(755), 505);
        assert!(!clock.skip(750, 505));
    }
}
