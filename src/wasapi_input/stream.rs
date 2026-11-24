use std::{sync::atomic::Ordering::Relaxed, time::Instant};

use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, DeviceState, Direction, Handle, Role,
    SampleType, StreamMode, WaveFormat,
};
use windows::Win32::System::Threading::SetEvent;

use crate::{
    RATE, Result,
    engine::TrackStats,
    timeline::{BLOCK, CaptureBlock},
};

use super::clock::PacketClock;

pub(super) struct Stream {
    client: AudioClient,
    capture: AudioCaptureClient,
    pub(super) event: Handle,
    buffer: Vec<u8>,
    channels: usize,
    packet_clock: PacketClock,
    pub(super) last_packet: Instant,
}

impl Stream {
    pub(super) fn open(
        devices: &DeviceEnumerator,
        direction: Direction,
        channels: usize,
        endpoint: Option<&str>,
    ) -> Result<Self> {
        let device = match endpoint {
            Some(id) => devices.get_device(id)?,
            None => devices.get_default_device_for_role(direction, Role::Console)?,
        };
        if device.get_direction() != direction || device.get_state()? != DeviceState::Active {
            return Err("The selected audio device is unavailable.".into());
        }
        let mut client = device.get_iaudioclient()?;
        let config = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 1_000_000,
        };
        let wave = capture_format(channels);
        client
            .initialize_client(&wave, Direction::Capture, config, None)
            .map_err(|err| {
                format!("Nie udało się otworzyć PCM 16-bit/48 kHz ({channels} kan.): {err}")
            })?;
        let buffer = vec![0; client.get_buffer_size()? as usize * channels * 2];
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        client.start_stream()?;
        Ok(Self {
            client,
            capture,
            event,
            buffer,
            channels,
            packet_clock: PacketClock::default(),
            last_packet: Instant::now(),
        })
    }

    pub(super) fn drain(
        &mut self,
        output: &mut rtrb::Producer<CaptureBlock>,
        stats: &TrackStats,
        origin: i64,
        clock: Instant,
    ) -> Result<bool> {
        let mut remaining = self.buffer.len() / (self.channels * 2);
        let mut received = false;
        while remaining > 0 && self.capture.get_next_packet_size()?.unwrap_or(0) != 0 {
            let (frames, info) = self.capture.read_from_device(&mut self.buffer)?;
            let frames = frames as usize;
            if frames == 0 {
                break;
            }
            received = true;
            remaining = remaining.saturating_sub(frames);
            self.last_packet = Instant::now();
            let broken = info.flags.data_discontinuity;
            if broken {
                stats.discontinuities.fetch_add(1, Relaxed);
            }
            let arrival = (self.last_packet.duration_since(clock).as_nanos() * RATE as u128
                / 1_000_000_000) as i64;
            let measured = (!info.flags.timestamp_error && info.timestamp != 0).then(|| {
                let hns = i128::from(info.timestamp) - i128::from(origin);
                // 48 kHz equals 3/625 frames per 100-ns unit.
                ((hns * 3 + 312).div_euclid(625)) as i64
            });
            let stamp = self.packet_clock.stamp(frames, arrival, measured, broken);
            if !stamp.measured {
                stats.invalid_timestamps.fetch_add(1, Relaxed);
            }
            if stamp.reset {
                stats.clock_resets.fetch_add(1, Relaxed);
            }
            stats.captured.fetch_add(frames as u64, Relaxed);
            for offset in (0..frames).step_by(BLOCK) {
                let len = (frames - offset).min(BLOCK);
                let mut packet = CaptureBlock {
                    start: stamp.start + offset as i64,
                    position: stamp.position + offset as u64,
                    len,
                    discontinuity: offset == 0 && stamp.reset,
                    measured: offset == 0 && stamp.measured,
                    samples: [0; BLOCK * 2],
                };
                if !info.flags.silent {
                    let bytes = &self.buffer
                        [offset * self.channels * 2..(offset + len) * self.channels * 2];
                    for (sample, bytes) in packet.samples.iter_mut().zip(bytes.as_chunks::<2>().0) {
                        *sample = i16::from_le_bytes(*bytes);
                    }
                }
                if output.push(packet).is_err() {
                    stats.dropped.fetch_add(len as u64, Relaxed);
                }
            }
        }
        if remaining == 0 {
            // A full device buffer is enough work in one pass. Wake again
            // even if the driver coalesced events while we were draining.
            unsafe { SetEvent(self.event.as_handle())? };
        }
        Ok(received)
    }
}

fn capture_format(channels: usize) -> WaveFormat {
    // WAVEFORMATEXTENSIBLE speaker masks. wasapi's generic one-channel
    // default is front left, which is not a mono downmix target.
    let mask = if channels == 1 { 0x4 } else { 0x3 };
    WaveFormat::new(16, 16, SampleType::Int, RATE, channels, Some(mask))
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_front_center_mono_and_front_left_right_stereo() {
        for (channels, mask) in [(1, 4), (2, 3)] {
            let wave = capture_format(channels);
            assert_eq!(wave.get_dwchannelmask(), mask);
            assert_eq!(wave.get_nchannels(), channels as u16);
            assert_eq!(wave.get_samplespersec(), 48_000);
            assert_eq!(wave.get_validbitspersample(), 16);
            assert_eq!(wave.get_blockalign(), 2 * channels as u32);
            assert_eq!(wave.get_subformat().unwrap(), SampleType::Int);
        }
    }
}
