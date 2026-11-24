mod disk;

use std::{collections::VecDeque, io, sync::Arc, thread::JoinHandle};

use crate::{FRAME, FRAMES_PER_SECOND, Result, codec::PREROLL};
use disk::{DiskFile, Spool};

pub const RAM_FRAMES: u64 = 5 * 60 * FRAMES_PER_SECOND;
pub const SEGMENT_FRAMES: u64 = 15 * FRAMES_PER_SECOND;
const SEGMENT_BYTES: usize = SEGMENT_FRAMES as usize * 350;

enum Storage {
    Memory(Arc<Vec<u8>>),
    Disk(DiskFile),
}

struct Segment {
    first: u64,
    end: u64,
    data: Storage,
}

pub struct History {
    closed: VecDeque<Arc<Segment>>,
    active: Vec<u8>,
    first: u64,
    end: u64,
    retained_from: u64,
    retention: u64,
    ram_frames: u64,
    spool: Option<Spool>,
}

impl History {
    pub fn new(retention: u64, ram_frames: u64) -> io::Result<(Self, Option<JoinHandle<()>>)> {
        let (spool, worker) = Spool::start()?;
        Ok((
            Self::with_spool(retention, ram_frames, Some(spool)),
            Some(worker),
        ))
    }

    fn with_spool(retention: u64, ram_frames: u64, spool: Option<Spool>) -> Self {
        Self {
            closed: VecDeque::new(),
            active: Vec::with_capacity(SEGMENT_BYTES),
            first: 0,
            end: 0,
            retained_from: 0,
            retention,
            ram_frames,
            spool,
        }
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    fn oldest(&self) -> u64 {
        self.closed.front().map_or(self.first, |chunk| chunk.first)
    }

    pub fn available(&self) -> u64 {
        (self.end - self.oldest().max(self.retained_from)).min(self.retention)
    }

    pub fn set_retention(&mut self, retention: u64) {
        let shrank = retention < self.retention;
        if shrank {
            self.retained_from = self.retained_from.max(self.end.saturating_sub(retention));
        }
        self.retention = retention;
        if shrank {
            self.trim();
        }
    }

    pub fn push(&mut self, packets: [&[u8]; 2]) -> io::Result<()> {
        for packet in packets {
            self.active
                .extend_from_slice(&(packet.len() as u16).to_le_bytes());
            self.active.extend_from_slice(packet);
        }
        self.end += 1;
        if self.end - self.first < SEGMENT_FRAMES {
            return Ok(());
        }

        let bytes = std::mem::replace(&mut self.active, Vec::with_capacity(SEGMENT_BYTES));
        self.closed.push_back(Arc::new(Segment {
            first: self.first,
            end: self.end,
            data: Storage::Memory(Arc::new(bytes)),
        }));
        self.first = self.end;
        self.trim();
        self.schedule_disk()
    }

    fn trim(&mut self) {
        let oldest = self
            .end
            .saturating_sub(self.retention + PREROLL + 2)
            .max(self.retained_from.saturating_sub(PREROLL + 2));
        self.closed
            .drain(..self.closed.partition_point(|chunk| chunk.end <= oldest));
    }

    fn schedule_disk(&mut self) -> io::Result<()> {
        let Some(spool) = &mut self.spool else {
            return Ok(());
        };
        let cutoff = self.end.saturating_sub(self.ram_frames);
        let Some(last_eligible) = self
            .closed
            .partition_point(|chunk| chunk.end <= cutoff)
            .checked_sub(1)
        else {
            return Ok(());
        };
        let segment = &self.closed[last_eligible];
        let Storage::Memory(bytes) = &segment.data else {
            return Ok(());
        };
        if let Err(err) = spool.schedule(segment.first, bytes.clone()) {
            self.closed.drain(..=last_eligible);
            return Err(err);
        }
        if last_eligible > 0 && matches!(self.closed[last_eligible - 1].data, Storage::Memory(_)) {
            // Disk did not keep up. Keep the recent contiguous history in RAM.
            self.closed.drain(..last_eligible);
            return Err(io::Error::other(
                "Zapis historii na dysku przekroczył czas jednego segmentu.",
            ));
        }
        Ok(())
    }

    pub fn poll_disk(&mut self) -> io::Result<bool> {
        let Some(result) = self.spool.as_mut().and_then(Spool::poll) else {
            return Ok(false);
        };
        let applied = self.apply_write(Some(result))?;
        self.schedule_disk()?;
        Ok(applied)
    }

    #[cfg(test)]
    pub fn wait_for_disk(&mut self) -> io::Result<bool> {
        let result = self.spool.as_mut().and_then(Spool::wait);
        self.apply_write(result)
    }

    fn apply_write(&mut self, result: Option<(u64, io::Result<DiskFile>)>) -> io::Result<bool> {
        let Some((first, result)) = result else {
            return Ok(false);
        };
        let Ok(index) = self
            .closed
            .binary_search_by_key(&first, |segment| segment.first)
        else {
            return Ok(false); // Rotation already retired the pending segment.
        };
        match result {
            Ok(file) => {
                self.closed[index] = Arc::new(Segment {
                    first,
                    end: self.closed[index].end,
                    data: Storage::Disk(file),
                });
                Ok(true)
            }
            Err(err) => {
                self.closed.drain(..=index);
                Err(err)
            }
        }
    }

    pub fn snapshot(&self, end: u64, length: u64, delays: [u16; 2]) -> Result<Snapshot> {
        let oldest = self.oldest();
        let start = end
            .saturating_sub(length.min(self.retention))
            .max(oldest)
            .max(self.retained_from);
        if start >= end || end >= self.end {
            return Err("There is no audio history to save yet.".into());
        }
        let first = start.saturating_sub(PREROLL).max(oldest);
        let skips = delays.map(|delay| delay + ((start - first) as usize * FRAME) as u16);
        let mut segments: Vec<_> = self
            .closed
            .iter()
            .filter(|chunk| chunk.end > first && chunk.first <= end)
            .cloned()
            .collect();
        if end >= self.first {
            segments.push(Arc::new(Segment {
                first: self.first,
                end: self.end,
                data: Storage::Memory(Arc::new(self.active.clone())),
            }));
        }
        Ok(Snapshot {
            segments,
            first,
            end,
            frames: end - start,
            skips,
            delays,
        })
    }
}

pub struct Snapshot {
    segments: Vec<Arc<Segment>>,
    first: u64,
    end: u64,
    pub frames: u64,
    pub skips: [u16; 2],
    pub delays: [u16; 2],
}

impl Snapshot {
    pub fn visit(&self, mut write: impl FnMut(u64, [&[u8]; 2], bool) -> Result<()>) -> Result<()> {
        let mut buffer = Vec::new();
        for segment in &self.segments {
            let bytes = match &segment.data {
                Storage::Memory(bytes) => bytes.as_ref(),
                Storage::Disk(file) => {
                    file.read(&mut buffer)?;
                    &buffer
                }
            };
            let mut bytes = bytes.as_slice();
            for frame in segment.first..segment.end.min(self.end + 1) {
                let packets = [read_packet(&mut bytes)?, read_packet(&mut bytes)?];
                if frame >= self.first {
                    write(frame - self.first, packets, frame == self.end)?;
                }
            }
        }
        Ok(())
    }
}

fn read_packet<'a>(bytes: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Truncated replay segment");
    let (size, rest) = bytes.split_first_chunk::<2>().ok_or_else(invalid)?;
    let (packet, rest) = rest
        .split_at_checked(u16::from_le_bytes(*size) as usize)
        .ok_or_else(invalid)?;
    *bytes = rest;
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[test]
    fn blocked_disk_preserves_recent_history_and_recovers() -> Result<()> {
        let (started, entered) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::channel();
        let mut first = true;
        let (spool, worker) = Spool::start_with(move |bytes| {
            if std::mem::take(&mut first) {
                started.send(()).unwrap();
                blocked.recv().unwrap();
            }
            let mut file = tempfile::tempfile()?;
            file.write_all(bytes)?;
            Ok(file)
        })?;
        let mut history = History::with_spool(SEGMENT_FRAMES * 12, SEGMENT_FRAMES, Some(spool));
        for i in 0..SEGMENT_FRAMES * 2 {
            history.push([&i.to_le_bytes(), &[42]])?;
        }
        entered.recv_timeout(Duration::from_secs(2))?;
        let start = Instant::now();
        let mut errors = 0;
        for i in SEGMENT_FRAMES * 2..SEGMENT_FRAMES * 6 {
            errors += usize::from(history.push([&i.to_le_bytes(), &[42]]).is_err());
        }
        let elapsed = start.elapsed();
        release.send(())?;
        assert_eq!(errors, 4);
        assert_eq!(history.available(), SEGMENT_FRAMES * 2);
        assert_eq!(history.closed.len(), 2);
        let end = history.end() - 1;
        let snapshot = history.snapshot(end, SEGMENT_FRAMES * 2, [312; 2])?;
        snapshot.visit(|index, [system, mic], _| {
            assert_eq!(
                u64::from_le_bytes(system.try_into()?),
                SEGMENT_FRAMES * 4 + index
            );
            assert_eq!(mic, [42]);
            Ok(())
        })?;
        assert!(!history.wait_for_disk()?); // The in-flight segment expired.
        history.schedule_disk()?;
        assert!(history.wait_for_disk()?);
        println!("60 synthetic seconds under a blocked write in {elapsed:?}");
        drop(snapshot);
        drop(history);
        worker.join().unwrap();
        Ok(())
    }

    #[test]
    fn snapshot_pins_disk_and_ram_across_rotation_and_eviction() -> Result<()> {
        let (mut history, worker) = History::new(SEGMENT_FRAMES * 3, SEGMENT_FRAMES)?;
        let end = SEGMENT_FRAMES * 4 + 3;
        for i in 0..=end {
            history.push([&i.to_le_bytes(), &[3]])?;
            history.wait_for_disk()?;
        }
        assert!(matches!(history.closed[0].data, Storage::Disk(_)));
        let snapshot = history.snapshot(end, end, [312; 2])?;
        assert_eq!(snapshot.frames, SEGMENT_FRAMES * 3);
        assert_eq!(snapshot.skips, [4152; 2]);
        for _ in 0..SEGMENT_FRAMES * 6 {
            history.push([&[4], &[5]])?;
            history.wait_for_disk()?;
        }
        let mut count = 0;
        snapshot.visit(|index, packets, last| {
            let sequence = u64::from_le_bytes(packets[0].try_into()?);
            assert_eq!(sequence, end - snapshot.frames - PREROLL + index);
            assert_eq!(packets[1], [3]);
            assert_eq!(last, sequence == end);
            count += 1;
            Ok(())
        })?;
        assert_eq!(count, snapshot.frames + PREROLL + 1);
        drop(snapshot);
        drop(history);
        worker.unwrap().join().unwrap();
        Ok(())
    }

    #[test]
    #[ignore = "simulates thirteen hours and exercises thousands of temporary files"]
    fn multi_hour_retention() -> Result<()> {
        let started = std::time::Instant::now();
        let length = 13 * 3600 * FRAMES_PER_SECOND;
        let retained = crate::engine::MAX_HISTORY_MINUTES * 60 * FRAMES_PER_SECOND;
        let (mut history, worker) = History::new(retained, RAM_FRAMES)?;
        for sequence in 0..=length {
            let marker = (sequence % 251) as u8;
            history.push([&[marker], &[42]])?;
            history.wait_for_disk()?;
        }
        assert_eq!(history.available(), retained);
        let memory = history
            .closed
            .iter()
            .filter(|item| matches!(item.data, Storage::Memory(_)))
            .count();
        let disk = history.closed.len() - memory;
        assert_eq!(memory, (RAM_FRAMES / SEGMENT_FRAMES) as usize);
        assert_eq!(
            disk,
            ((retained - RAM_FRAMES) / SEGMENT_FRAMES + 1) as usize
        );
        let snapshot = history.snapshot(length, retained, [312; 2])?;
        let mut visited = 0;
        snapshot.visit(|index, [system, mic], last| {
            let sequence = length - retained - PREROLL + index;
            assert_eq!(system, [(sequence % 251) as u8]);
            assert_eq!(mic, [42]);
            assert_eq!(last, sequence == length);
            visited += 1;
            Ok(())
        })?;
        assert_eq!(visited, retained + PREROLL + 1);
        println!(
            "13 h simulated in {:.2?}, 12 h retained across {disk} disk and {memory} closed RAM segments",
            started.elapsed()
        );
        drop(snapshot);
        drop(history);
        worker.unwrap().join().unwrap();
        Ok(())
    }
}

#[test]
fn increasing_retention_after_shrinking_does_not_restore_discarded_audio() -> Result<()> {
    let mut history = History::with_spool(1_000, RAM_FRAMES, None);
    for _ in 0..750 {
        history.push([b"a", b"b"])?;
    }
    history.set_retention(100);
    history.set_retention(1_000);
    assert_eq!(history.available(), 100);
    let clip = history.snapshot(749, 1_000, [0, 0])?;
    assert_eq!(clip.frames, 99);
    Ok(())
}
