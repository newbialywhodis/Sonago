use rtrb::Consumer;

use crate::{FRAME, RATE};

pub(crate) const BLOCK: usize = 480;

pub(crate) struct CaptureBlock {
    pub start: i64,
    pub position: u64,
    pub len: usize,
    pub discontinuity: bool,
    pub measured: bool,
    pub samples: [i16; BLOCK * 2],
}

pub struct Timeline {
    input: Consumer<CaptureBlock>,
    packet: Option<CaptureBlock>,
    start: i64,
    len: usize,
    next: i64,
    position: Option<u64>,
    phase: f64,
    drift: f64,
}

impl Timeline {
    pub fn new(input: Consumer<CaptureBlock>) -> Self {
        Self {
            input,
            packet: None,
            start: 0,
            len: 0,
            next: 0,
            position: None,
            phase: 0.0,
            drift: 0.0,
        }
    }

    fn align(&mut self, packet: &CaptureBlock) {
        let error = packet.start - self.next;
        let continuous = !packet.discontinuity && self.position == Some(packet.position);
        let adjust = if continuous && (!packet.measured || error.abs() < (RATE / 50) as i64) {
            self.start = self.next;
            if packet.measured {
                // Estimate both the phase and the rate. A phase-only filter
                // lags several samples behind a steadily drifting device.
                let residual = error as f64 - self.phase;
                self.phase += residual / 8.0;
                self.drift =
                    (self.drift + residual / (128.0 * self.len as f64)).clamp(-0.002, 0.002);
            }
            self.phase += packet.len as f64 * self.drift;
            let adjust =
                self.phase
                    .round()
                    .clamp(if packet.len == 1 { 0.0 } else { -1.0 }, 1.0) as i64;
            self.phase -= adjust as f64;
            adjust
        } else {
            self.phase = 0.0;
            self.drift = 0.0;
            self.start = packet.start;
            0
        };
        self.len = (packet.len as i64 + adjust).max(1) as usize;
        self.next = self.start + self.len as i64;
        self.position = Some(packet.position + packet.len as u64);
    }

    pub fn fill(&mut self, start: i64, channels: usize, output: &mut [i16; FRAME * 2]) -> u64 {
        let end = start + FRAME as i64;
        let mut cursor = start;
        let mut late = 0;
        output[..FRAME * channels].fill(0);
        while cursor < end {
            if self.packet.is_none() {
                let Ok(packet) = self.input.pop() else { break };
                self.align(&packet);
                late += (start - self.start).clamp(0, self.len as i64) as u64;
                self.packet = Some(packet);
            }
            let packet = self.packet.as_ref().unwrap();
            let packet_end = self.start + self.len as i64;
            if self.start >= end {
                break;
            }
            let from = cursor.max(self.start);
            let to = end.min(packet_end);
            if from < to {
                let offset = (from - self.start) as usize;
                let output_offset = (from - start) as usize * channels;
                let count = (to - from) as usize * channels;
                copy_samples(
                    packet,
                    self.len,
                    channels,
                    offset,
                    &mut output[output_offset..output_offset + count],
                );
                cursor = to;
            }
            if packet_end > end {
                break;
            }
            self.packet = None;
        }
        late
    }
}

fn copy_samples(
    packet: &CaptureBlock,
    len: usize,
    channels: usize,
    offset: usize,
    output: &mut [i16],
) {
    if len == packet.len {
        let first = offset * channels;
        output.copy_from_slice(&packet.samples[first..first + output.len()]);
        return;
    }
    // A phase correction stretches or shrinks this block by one sample.
    // Interpolate within the block so there is no abrupt sample insertion.
    let denominator = len.saturating_sub(1).max(1) as i32;
    for (index, target) in output.chunks_exact_mut(channels).enumerate() {
        let phase = ((index + offset) * (packet.len - 1)) as i32;
        let first = (phase / denominator) as usize;
        let fraction = phase % denominator;
        let second = (first + 1).min(packet.len - 1);
        debug_assert!(first < BLOCK && second < BLOCK);
        for (channel, sample) in target.iter_mut().enumerate() {
            let a = i32::from(packet.samples[first * channels + channel]);
            let b = i32::from(packet.samples[second * channels + channel]);
            *sample = (a + (b - a) * fraction / denominator) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use rtrb::RingBuffer;

    use super::*;

    fn block(start: i64, position: u64) -> CaptureBlock {
        CaptureBlock {
            start,
            position,
            len: BLOCK,
            discontinuity: false,
            measured: true,
            samples: [123; BLOCK * 2],
        }
    }

    #[test]
    fn fills_real_gaps_and_keeps_future_packets() {
        let (mut input, reader) = RingBuffer::new(8);
        input.push(block(240, 0)).ok().unwrap();
        input.push(block(1200, 960)).ok().unwrap();
        let mut timeline = Timeline::new(reader);
        let mut samples = [0; FRAME * 2];
        timeline.fill(0, 1, &mut samples);
        assert_eq!(&samples[..240], &[0; 240]);
        assert_eq!(&samples[240..720], &[123; 480]);
        assert_eq!(&samples[720..960], &[0; 240]);
        timeline.fill(960, 1, &mut samples);
        assert_eq!(&samples[240..720], &[123; 480]);
    }

    #[test]
    fn queue_overflow_keeps_missing_samples_at_their_original_time() {
        let (mut input, reader) = RingBuffer::new(2);
        input.push(block(0, 0)).ok().unwrap();
        input.push(block(480, 480)).ok().unwrap();
        assert!(input.push(block(960, 960)).is_err());
        let mut timeline = Timeline::new(reader);
        let mut samples = [0; FRAME * 2];
        assert_eq!(timeline.fill(0, 1, &mut samples), 0);
        assert_eq!(&samples[..FRAME], &[123; FRAME]);
        input.push(block(1440, 1440)).ok().unwrap();
        assert_eq!(timeline.fill(960, 1, &mut samples), 0);
        assert_eq!(&samples[..BLOCK], &[0; BLOCK]);
        assert_eq!(&samples[BLOCK..FRAME], &[123; BLOCK]);
    }

    #[test]
    fn tracks_positive_and_negative_device_drift_without_gaps() {
        for ppm in [-200, 200] {
            let (mut input, reader) = RingBuffer::new(3);
            let mut timeline = Timeline::new(reader);
            let mut samples = [0; FRAME * 2];
            for i in 0..10_000i64 {
                let position = i * BLOCK as i64;
                let stamp = position + position * ppm / 1_000_000;
                let packet = block(stamp, position as u64);
                input.push(packet).ok().unwrap();
                let packet = timeline.input.pop().unwrap();
                timeline.align(&packet);
                assert!((timeline.start - stamp).abs() <= 4);
                copy_samples(&packet, timeline.len, 1, 0, &mut samples[..timeline.len]);
                assert!(samples[..timeline.len].iter().all(|&sample| sample == 123));
            }
        }
    }

    #[test]
    fn drift_with_jitter_keeps_tone_aligned_on_a_twelve_hour_timeline() {
        let base = 12 * 3600 * RATE as i64;
        let step = std::f64::consts::TAU * 997.0 / RATE as f64;
        for ppm in [-500, 500] {
            let (_, reader) = RingBuffer::new(1);
            let mut timeline = Timeline::new(reader);
            let mut samples = [0; BLOCK + 1];
            let mut error_squared = 0.0;
            let mut peak_offset = 0;
            let mut count = 0;
            for i in 0..6_000 {
                let position = i * BLOCK as i64;
                let time_scale = 1.0 + f64::from(ppm) / 1_000_000.0;
                let time = position as f64 * time_scale;
                let jitter = [0, 3, -2, 1, -3, 2, -1][(i % 7) as usize];
                let measured = i % 13 != 5;
                let mut packet = block(base + time.round() as i64 + jitter, position as u64);
                packet.measured = measured;
                for (index, sample) in packet.samples[..BLOCK].iter_mut().enumerate() {
                    *sample = (16_000.0 * (step * (time + index as f64 * time_scale)).sin()) as i16;
                }
                let previous_end = timeline.next;
                timeline.align(&packet);
                if i > 0 {
                    assert_eq!(timeline.start, previous_end);
                }
                let offset = (timeline.start - base - time.round() as i64).abs();
                peak_offset = peak_offset.max(offset);
                copy_samples(&packet, timeline.len, 1, 0, &mut samples[..timeline.len]);
                for (index, &sample) in samples[..timeline.len].iter().enumerate() {
                    let ideal = (step * ((timeline.start - base) as f64 + index as f64)).sin();
                    error_squared += (ideal - f64::from(sample) / 16_000.0).powi(2);
                }
                count += timeline.len;
            }
            let rms = (error_squared / count as f64).sqrt();
            println!(
                "{ppm:+} ppm with jitter: peak offset {peak_offset} samples, normalized tone RMSE {rms:.3}"
            );
            assert!(peak_offset <= 6);
            assert!(rms < 0.25);
        }
    }
}
