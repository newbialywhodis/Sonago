use super::{RATE, Result};
use opus::{Channels, Decoder};
use std::{
    fs::File,
    io::{BufReader, ErrorKind},
    path::Path,
};
use symphonia::core::{errors::Error, formats::FormatReader, io::MediaSourceStream};

// Sonago writes the same 20 ms Opus packets to both containers. libopus
// downmixes and resamples directly to the models' 16 kHz mono input.
struct Track {
    opus: Decoder,
    skip: u64,
    position: u64,
}

impl Track {
    fn new(head: &[u8]) -> Result<Self> {
        if !head.starts_with(b"OpusHead") || head.len() < 19 {
            return Err("Missing Opus header".into());
        }
        let skip = u16::from_le_bytes([head[10], head[11]]);
        Ok(Self {
            opus: Decoder::new(RATE as u32, Channels::Mono)?,
            skip: u64::from(skip) / 3,
            position: 0,
        })
    }

    fn packet(
        &mut self,
        bytes: &[u8],
        end: u64,
        emit: &mut impl FnMut(&[f32]) -> Result<()>,
    ) -> Result<()> {
        let mut pcm = [0.; RATE / 50];
        let n = self.opus.decode_float(bytes, &mut pcm, false)?;
        let first = self.skip.saturating_sub(self.position).min(n as u64) as usize;
        let last = end.saturating_sub(self.position).min(n as u64) as usize;
        self.position += n as u64;
        if last > first {
            emit(&pcm[first..last])?;
        }
        Ok(())
    }
}

pub fn read(clip: &Path, mic: bool, mut emit: impl FnMut(&[f32]) -> Result<()>) -> Result<()> {
    if clip.is_dir() {
        let file = clip.join(if mic {
            "microphone.opus"
        } else {
            "system.opus"
        });
        let mut reader = ogg::PacketReader::new(BufReader::new(File::open(file)?));
        let mut track = Track::new(&reader.read_packet_expected()?.data)?;
        if !reader.read_packet_expected()?.data.starts_with(b"OpusTags") {
            return Err("Missing Opus tags".into());
        }
        loop {
            let packet = reader.read_packet_expected()?;
            let last = packet.last_in_stream();
            let end = if last {
                packet.absgp_page() / 3
            } else {
                u64::MAX
            };
            track.packet(&packet.data, end, &mut emit)?;
            if last {
                return Ok(());
            }
        }
    }
    let stream = MediaSourceStream::new(Box::new(File::open(clip)?), Default::default());
    let mut reader = symphonia::default::formats::MkvReader::try_new(stream, &Default::default())?;
    // src/mux/matroska.rs writes mix=1, system=2, microphone=3.
    let id = if mic { 3 } else { 2 };
    let params = &reader
        .tracks()
        .iter()
        .find(|t| t.id == id)
        .ok_or("Missing Sonago audio track")?
        .codec_params;
    let mut track = Track::new(params.extra_data.as_deref().unwrap_or_default())?;
    let time = params.time_base.ok_or("Missing Matroska time base")?;
    let duration = params.n_frames.ok_or("Missing clip duration")?;
    let end = track.skip + duration * u64::from(time.numer) * RATE as u64 / u64::from(time.denom);
    while track.position < end {
        let packet = match reader.next_packet() {
            Err(Error::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                return Err("Truncated Matroska clip".into());
            }
            packet => packet?,
        };
        if packet.track_id() == id {
            track.packet(packet.buf(), end, &mut emit)?;
        }
    }
    Ok(())
}
