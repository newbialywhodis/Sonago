use std::io::Write;

use ogg::{PacketWriteEndInfo as End, PacketWriter};

use crate::{CHANNELS, FRAME, FRAMES_PER_SECOND, Result, codec::opus_head, history::Snapshot};

pub fn write<W: Write>(snapshot: &Snapshot, outputs: &mut [W; 2]) -> Result<()> {
    let mut writers = outputs.each_mut().map(PacketWriter::new);
    let vendor = b"hearlier";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    for (track, writer) in writers.iter_mut().enumerate() {
        let head = opus_head(CHANNELS[track], snapshot.skips[track]);
        writer.write_packet(head.to_vec(), 1, End::EndPage, 0)?;
        writer.write_packet(tags.clone(), 1, End::EndPage, 0)?;
    }

    snapshot.visit(|index, packets, last| {
        for (track, writer) in writers.iter_mut().enumerate() {
            let (end, position) = if last {
                (
                    End::EndStream,
                    snapshot.frames * FRAME as u64 + u64::from(snapshot.skips[track]),
                )
            } else if (index + 1).is_multiple_of(FRAMES_PER_SECOND) {
                (End::EndPage, (index + 1) * FRAME as u64)
            } else {
                (End::NormalPacket, (index + 1) * FRAME as u64)
            };
            writer.write_packet(packets[track].to_vec(), 1, end, position)?;
        }
        Ok(())
    })
}
