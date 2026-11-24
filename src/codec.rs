use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::{CHANNELS, FRAME, RATE, Result};

pub const MAX_PACKET: usize = 1275;
pub const PREROLL: u64 = 4;

pub struct Encoders {
    encoders: [Encoder; 2],
    pub delays: [u16; 2],
    packets: [[u8; MAX_PACKET]; 2],
}

impl Encoders {
    pub fn new() -> Result<Self> {
        let mut encoders = [
            encoder(Channels::Stereo, 96_000)?,
            encoder(Channels::Mono, 32_000)?,
        ];
        let delays = [
            u16::try_from(encoders[0].get_lookahead()?)?,
            u16::try_from(encoders[1].get_lookahead()?)?,
        ];
        if delays.iter().any(|&delay| usize::from(delay) >= FRAME) {
            return Err("The Opus encoder delay exceeds the export tail".into());
        }
        Ok(Self {
            encoders,
            delays,
            packets: [[0; MAX_PACKET]; 2],
        })
    }

    pub fn encode(&mut self, pcm: &[[i16; FRAME * 2]; 2]) -> Result<[&[u8]; 2]> {
        let mut sizes = [0; 2];
        for track in 0..2 {
            sizes[track] = self.encoders[track].encode(
                &pcm[track][..FRAME * CHANNELS[track]],
                &mut self.packets[track],
            )?;
        }
        Ok([&self.packets[0][..sizes[0]], &self.packets[1][..sizes[1]]])
    }
}

fn encoder(channels: Channels, bitrate: i32) -> Result<Encoder> {
    let mut encoder = Encoder::new(RATE as u32, channels, Application::Audio)?;
    encoder.set_bitrate(Bitrate::Bits(bitrate))?;
    encoder.set_complexity(4)?;
    encoder.set_vbr_constraint(true)?;
    Ok(encoder)
}

pub struct MixEncoder {
    system: Decoder,
    mic: Decoder,
    encoder: Encoder,
    pub delay: u16,
    stereo: [f32; FRAME * 2],
    mono: [f32; FRAME],
    packet: [u8; MAX_PACKET],
}

impl MixEncoder {
    pub fn new(delays: [u16; 2]) -> Result<Self> {
        let mut encoder = encoder(Channels::Stereo, 128_000)?;
        let lookahead = u16::try_from(encoder.get_lookahead()?)?;
        // The source PCM is decoded before pre-skip. Both stems have the same
        // Opus lookahead and the snapshot includes one packet after the cut.
        let delay = delays[0] + lookahead;
        if delays[0] != delays[1] || usize::from(delay) >= FRAME {
            return Err("Unexpected Opus delay for the preview mix".into());
        }
        Ok(Self {
            system: Decoder::new(RATE as u32, Channels::Stereo)?,
            mic: Decoder::new(RATE as u32, Channels::Mono)?,
            encoder,
            delay,
            stereo: [0.0; FRAME * 2],
            mono: [0.0; FRAME],
            packet: [0; MAX_PACKET],
        })
    }

    pub fn encode(&mut self, [system, mic]: [&[u8]; 2]) -> Result<&[u8]> {
        let system_frames = self.system.decode_float(system, &mut self.stereo, false)?;
        let mic_frames = self.mic.decode_float(mic, &mut self.mono, false)?;
        if system_frames != FRAME || mic_frames != FRAME {
            return Err("Unexpected Opus frame size in history".into());
        }
        for (stereo, &mic) in self
            .stereo
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.mono)
        {
            for sample in stereo {
                // Opus float decoding can overshoot full scale.
                *sample = 0.5 * (sample.clamp(-1.0, 1.0) + mic.clamp(-1.0, 1.0));
            }
        }
        let size = self.encoder.encode_float(&self.stereo, &mut self.packet)?;
        Ok(&self.packet[..size])
    }
}

pub fn opus_head(channels: usize, skip: u16) -> [u8; 19] {
    let mut head = *b"OpusHead\x01\0\0\0\0\0\0\0\0\0\0";
    head[9] = channels as u8;
    head[10..12].copy_from_slice(&skip.to_le_bytes());
    head[12..16].copy_from_slice(&(RATE as u32).to_le_bytes());
    head
}
