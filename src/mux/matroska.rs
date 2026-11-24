use std::io::{Seek, SeekFrom, Write};

use crate::{
    FRAME, FRAME_MS, FRAMES_PER_SECOND, RATE, Result,
    codec::{MixEncoder, opus_head},
    history::Snapshot,
};

const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CUES: u32 = 0x1C53_BB6B;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_FRAMES: u64 = FRAMES_PER_SECOND * 2;

pub fn write(snapshot: &Snapshot, writer: &mut (impl Write + Seek)) -> Result<()> {
    let mut mix = MixEncoder::new(snapshot.delays)?;
    let delays = [mix.delay, snapshot.delays[0], snapshot.delays[1]];
    let skips = [
        snapshot.skips[0] + mix.delay - snapshot.delays[0],
        snapshot.skips[0],
        snapshot.skips[1],
    ];
    let mut buf = Vec::new();
    uint(&mut buf, 0x4286, 1);
    uint(&mut buf, 0x42F7, 1);
    uint(&mut buf, 0x42F2, 4);
    uint(&mut buf, 0x42F3, 8);
    element(&mut buf, 0x4282, b"matroska");
    uint(&mut buf, 0x4287, 4);
    uint(&mut buf, 0x4285, 2);
    write_element(writer, 0x1A45_DFA3, &buf)?;

    writer.write_all(&id_bytes(0x1853_8067))?;
    let size_offset = writer.stream_position()?;
    writer.write_all(&size_fixed(0))?;
    let base = size_offset + 8;
    let placeholder = seek_head(0, 0, 0);
    writer.write_all(&placeholder)?;

    let info_pos = writer.stream_position()? - base;
    buf.clear();
    uint(&mut buf, 0x2A_D7B1, 1_000_000);
    element(
        &mut buf,
        0x4489,
        &((snapshot.frames * FRAME_MS) as f64).to_be_bytes(),
    );
    element(&mut buf, 0x4D80, b"hearlier");
    element(&mut buf, 0x5741, b"hearlier");
    write_element(writer, INFO, &buf)?;

    let tracks_pos = writer.stream_position()? - base;
    buf.clear();
    for (track, (name, channels)) in [
        ("System + mikrofon", 2),
        ("Dźwięk systemu", 2),
        ("Mikrofon", 1),
    ]
    .into_iter()
    .enumerate()
    {
        write_track(&mut buf, track, name, channels, skips[track]);
    }
    write_element(writer, TRACKS, &buf)?;

    let mut cluster = Vec::with_capacity(48 * 1024);
    let mut cues = Vec::new();
    snapshot.visit(|index, [system, mic], last| {
        let offset = index % CLUSTER_FRAMES;
        if offset == 0 {
            flush_cluster(writer, &mut cluster)?;
            let stamp = index * FRAME_MS;
            uint(&mut cluster, 0xE7, stamp);
            cue(&mut cues, stamp, writer.stream_position()? - base);
        }
        for (track, packet) in [mix.encode([system, mic])?, system, mic]
            .into_iter()
            .enumerate()
        {
            let track_byte = 0x81 + track as u8;
            let timestamp = (offset as i16 * FRAME_MS as i16).to_be_bytes();
            if last {
                buf.clear();
                block(
                    &mut buf,
                    0xA1,
                    [track_byte, timestamp[0], timestamp[1], 0],
                    packet,
                );
                let padding = nanos(FRAME as u64 - u64::from(delays[track]));
                element(&mut buf, 0x75A2, &(padding as i64).to_be_bytes());
                element(&mut cluster, 0xA0, &buf);
            } else {
                block(
                    &mut cluster,
                    0xA3,
                    [track_byte, timestamp[0], timestamp[1], 0x80],
                    packet,
                );
            }
        }
        Ok(())
    })?;
    flush_cluster(writer, &mut cluster)?;
    let cues_pos = writer.stream_position()? - base;
    write_element(writer, CUES, &cues)?;
    let end = writer.stream_position()?;

    writer.seek(SeekFrom::Start(size_offset))?;
    writer.write_all(&size_fixed(end - base))?;
    writer.write_all(&seek_head(info_pos, tracks_pos, cues_pos))?;
    writer.seek(SeekFrom::Start(end))?;
    Ok(())
}

fn write_track(tracks: &mut Vec<u8>, track: usize, name: &str, channels: usize, skip: u16) {
    let mut entry = Vec::new();
    uint(&mut entry, 0xD7, track as u64 + 1);
    uint(&mut entry, 0x73C5, track as u64 + 1);
    uint(&mut entry, 0x83, 2);
    uint(&mut entry, 0x88, u64::from(track == 0));
    element(&mut entry, 0x536E, name.as_bytes());
    element(&mut entry, 0x86, b"A_OPUS");
    element(&mut entry, 0x63A2, &opus_head(channels, skip));
    uint(&mut entry, 0x56AA, nanos(u64::from(skip)));
    uint(&mut entry, 0x56BB, 80_000_000);
    let mut audio = Vec::new();
    element(&mut audio, 0xB5, &(RATE as f64).to_be_bytes());
    uint(&mut audio, 0x9F, channels as u64);
    element(&mut entry, 0xE1, &audio);
    element(tracks, 0xAE, &entry);
}

fn seek_head(info: u64, tracks: u64, cues: u64) -> Vec<u8> {
    let mut children = Vec::new();
    for (id, pos) in [(INFO, info), (TRACKS, tracks), (CUES, cues)] {
        let mut seek = Vec::new();
        element(&mut seek, 0x53AB, &id_bytes(id));
        // Fixed width so the placeholder and the final SeekHead have identical sizes.
        element(&mut seek, 0x53AC, &pos.to_be_bytes());
        element(&mut children, 0x4DBB, &seek);
    }
    let mut result = Vec::new();
    element(&mut result, 0x114D_9B74, &children);
    result
}

fn cue(cues: &mut Vec<u8>, stamp: u64, position: u64) {
    let mut point = Vec::with_capacity(48);
    uint(&mut point, 0xB3, stamp);
    for track in 1..=3 {
        let mut location = Vec::with_capacity(16);
        uint(&mut location, 0xF7, track);
        uint(&mut location, 0xF1, position);
        element(&mut point, 0xB7, &location);
    }
    element(cues, 0xBB, &point);
}

fn flush_cluster(writer: &mut impl Write, cluster: &mut Vec<u8>) -> std::io::Result<()> {
    if !cluster.is_empty() {
        write_element(writer, CLUSTER, cluster)?;
        cluster.clear();
    }
    Ok(())
}

fn block(output: &mut Vec<u8>, id: u32, prefix: [u8; 4], packet: &[u8]) {
    header(output, id, prefix.len() + packet.len()).expect("writing to a Vec cannot fail");
    output.extend_from_slice(&prefix);
    output.extend_from_slice(packet);
}

fn unsigned_len(value: u64) -> usize {
    ((64 - value.leading_zeros()).div_ceil(8) as usize).max(1)
}

fn id_bytes(id: u32) -> [u8; 4] {
    id.to_be_bytes()
}

fn size_fixed(size: u64) -> [u8; 8] {
    ((1 << 56) | size).to_be_bytes()
}

fn nanos(samples: u64) -> u64 {
    (samples * 1_000_000_000 + RATE as u64 / 2) / RATE as u64
}

fn header(out: &mut impl Write, id: u32, length: usize) -> std::io::Result<()> {
    let length = length as u64;
    let width = (1..=8)
        .find(|width| length < (1 << (7 * width)) - 1)
        .unwrap();
    let size = ((1 << (7 * width)) | length).to_be_bytes();
    out.write_all(&id_bytes(id)[4 - unsigned_len(u64::from(id))..])?;
    out.write_all(&size[8 - width..])
}

fn uint(out: &mut Vec<u8>, id: u32, value: u64) {
    element(out, id, &value.to_be_bytes()[8 - unsigned_len(value)..]);
}

fn element(out: &mut Vec<u8>, id: u32, data: &[u8]) {
    write_element(out, id, data).expect("writing to a Vec cannot fail");
}

fn write_element(out: &mut impl Write, id: u32, data: &[u8]) -> std::io::Result<()> {
    header(out, id, data.len())?;
    out.write_all(data)
}
