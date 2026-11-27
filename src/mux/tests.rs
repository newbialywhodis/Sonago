use std::{path::Path, process::Command, time::Instant};

use crate::{CHANNELS, FRAME, FRAMES_PER_SECOND, RATE, Result, codec::Encoders, history::History};

use super::{Format, save};

#[test]
#[ignore = "requires a two-stem speech/music fixture and the built transcription worker"]
fn queued_transcripts_match_for_both_exports() -> Result<()> {
    use std::{fs, io::Write, process::Stdio};
    // Interleaved 48 kHz s16le: left = system, right = microphone.
    let bytes = fs::read(std::env::var("SONAGO_TEST_STEMS")?)?;
    let executable = std::env::var("SONAGO_TEST_TRANSCRIBER")?;
    let frames = bytes.len() / (FRAME * 4);
    let mut codec = Encoders::new()?;
    let (mut history, _) = History::new(frames as u64 + 1, frames as u64 + 1)?;
    let mut pcm = [[0; FRAME * 2]; 2];
    for frame in bytes.as_chunks::<{ FRAME * 4 }>().0 {
        for (i, sample) in frame.as_chunks::<4>().0.iter().enumerate() {
            pcm[0][2 * i..2 * i + 2].fill(i16::from_le_bytes([sample[0], sample[1]]));
            pcm[1][i] = i16::from_le_bytes([sample[2], sample[3]]);
        }
        history.push(codec.encode(&pcm)?)?;
    }
    let snapshot = history.snapshot(frames as u64 - 1, frames as u64 - 3, codec.delays)?;
    let temp = tempfile::tempdir()?;
    let mka = save(&snapshot, temp.path(), Format::Matroska)?;
    let mut worker = Command::new(executable)
        .arg("--queue")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut queue = worker.stdin.take().unwrap();
    writeln!(queue, "{}", mka.display())?;
    // Publish another export while the first transcript is pending.
    let ogg = save(&snapshot, temp.path(), Format::OggOpus)?;
    writeln!(queue, "{}", temp.path().join("missing.mka").display())?;
    writeln!(queue, "{}", ogg.display())?;
    drop(queue); // The tray can exit before either transcript completes.
    let result = worker.wait_with_output()?;
    assert!(result.status.success());
    let replies = String::from_utf8(result.stdout)?
        .lines()
        .map(serde_json::from_str::<Option<String>>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(replies.len(), 3);
    assert!(
        replies[0].is_none() && replies[1].is_some() && replies[2].is_none(),
        "{replies:?}"
    );
    let text = fs::read_to_string(mka.with_extension("txt"))?;
    assert_eq!(text, fs::read_to_string(ogg.join("transcript.txt"))?);
    for label in ["Microphone:", "Speaker ", "Music"] {
        assert!(text.contains(label), "{text}");
    }
    Ok(())
}

#[test]
#[ignore = "requires FFmpeg and ffprobe on PATH"]
fn remuxed_audio_matches_across_containers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut codec = Encoders::new()?;
    let (mut history, _) = History::new(1000, 1000)?;
    for i in 0..301 {
        history.push(codec.encode(&signal(i))?)?;
    }

    for (end, length) in [(1, 1), (3, 3), (300, 251)] {
        let snapshot = history.snapshot(end, length, codec.delays)?;
        let matroska = save(&snapshot, temp.path(), Format::Matroska)?;
        let ogg = save(&snapshot, temp.path(), Format::OggOpus)?;
        let mix = decode_default(&matroska)?;
        assert_eq!(mix.len(), length as usize * FRAME * 2 * 2);
        assert_eq!(mix, decode(&matroska, 0)?);
        let mut stems = Vec::new();
        for (track, file) in ["system.opus", "microphone.opus"].into_iter().enumerate() {
            let ogg_pcm = decode(&ogg.join(file), 0)?;
            let mka_pcm = decode(&matroska, track + 1)?;
            assert_eq!(ogg_pcm.len(), length as usize * FRAME * CHANNELS[track] * 2);
            assert_eq!(
                mka_pcm.len(),
                ogg_pcm.len(),
                "Matroska sample count, track {track}"
            );
            assert!(
                mka_pcm == ogg_pcm,
                "Matroska and Ogg PCM differ, track {track}"
            );
            stems.push(ogg_pcm);
        }
        if length > 200 {
            assert_mixed_signal(&mix, &stems[0], &stems[1]);
            let probe = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=noprint_wrappers=1:nokey=1",
                ])
                .arg(&matroska)
                .output()?;
            assert!(probe.status.success());
            let duration: f64 = String::from_utf8(probe.stdout)?.trim().parse()?;
            assert!((duration - length as f64 / FRAMES_PER_SECOND as f64).abs() < 0.000_001);
            for track in 0..3 {
                let pcm = Command::new("ffmpeg")
                    .args(["-v", "error", "-ss", "4.0", "-i"])
                    .arg(&matroska)
                    .args([
                        "-map",
                        &format!("0:a:{track}"),
                        "-t",
                        "0.2",
                        "-f",
                        "s16le",
                        "-",
                    ])
                    .output()?;
                assert!(
                    pcm.status.success() && !pcm.stdout.is_empty(),
                    "Matroska seek failed"
                );
            }
        }
    }
    Ok(())
}

fn decode(file: &Path, track: usize) -> Result<Vec<u8>> {
    decode_with_map(file, Some(&format!("0:a:{track}")))
}

fn decode_default(file: &Path) -> Result<Vec<u8>> {
    decode_with_map(file, None)
}

fn decode_with_map(file: &Path, map: Option<&str>) -> Result<Vec<u8>> {
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-i"]).arg(file);
    if let Some(map) = map {
        command.args(["-map", map]);
    }
    let result = command.args(["-f", "s16le", "-"]).output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(result.stdout)
}

fn assert_mixed_signal(mix: &[u8], system: &[u8], mic: &[u8]) {
    let pcm = |bytes: &[u8]| {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(i16::from_le_bytes)
            .collect::<Vec<_>>()
    };
    let (mix, system, mic) = (pcm(mix), pcm(system), pcm(mic));
    // Ignore decoder warmup and compare a full second against both stems.
    let start = 2 * FRAME;
    for channel in 0..2 {
        let mut dot = [0.0; 3];
        let mut energy = [0.0; 3];
        let mut mixed_energy = 0.0;
        for frame in start..start + RATE {
            let mixed = f64::from(mix[frame * 2 + channel]);
            let system = f64::from(system[frame * 2 + channel]);
            let mic = f64::from(mic[frame]);
            let expected = (system + mic) * 0.5;
            for (i, sample) in [system, mic, expected].into_iter().enumerate() {
                dot[i] += mixed * sample;
                energy[i] += sample * sample;
            }
            mixed_energy += mixed * mixed;
        }
        assert!(dot[0] / energy[0] > 0.43 && dot[0] / energy[0] < 0.57);
        assert!(dot[1] / energy[1] > 0.43 && dot[1] / energy[1] < 0.57);
        assert!(dot[2] / (energy[2] * mixed_energy).sqrt() > 0.98);
    }
}

#[test]
#[ignore = "synthetic performance benchmark"]
fn encode_throughput() -> Result<()> {
    const SECONDS: u64 = 600;
    let signals: Vec<_> = (0..50).map(signal).collect();
    let silence = [[0; FRAME * 2]; 2];
    println!("{}", opus::version());
    for (name, active) in [("silence", false), ("tone", true)] {
        let mut codec = Encoders::new()?;
        let (mut history, worker) =
            History::new(SECONDS * FRAMES_PER_SECOND, 5 * 60 * FRAMES_PER_SECOND)?;
        let mut bytes = 0;
        let elapsed = Instant::now();
        for i in 0..SECONDS * FRAMES_PER_SECOND {
            let pcm = if active {
                &signals[i as usize % signals.len()]
            } else {
                &silence
            };
            let packets = codec.encode(pcm)?;
            bytes += packets.iter().map(|packet| packet.len() + 2).sum::<usize>();
            history.push(packets)?;
            history.wait_for_disk()?;
        }
        let limit = SECONDS * FRAMES_PER_SECOND - 1;
        let wall = elapsed.elapsed().as_secs_f64();
        let snapshot = history.snapshot(limit, limit - 10, codec.delays)?;
        let export = tempfile::tempdir()?;
        let elapsed = Instant::now();
        let result = save(&snapshot, export.path(), Format::Matroska)?;
        println!(
            "{name}: {SECONDS} s encoded in {wall:.3} s ({:.1}x real time), {} history bytes; {} export bytes in {:.3} s",
            SECONDS as f64 / wall,
            bytes,
            result.metadata()?.len(),
            elapsed.elapsed().as_secs_f64()
        );
        drop(snapshot);
        drop(history);
        worker.unwrap().join().unwrap();
    }
    Ok(())
}

fn signal(frame: u64) -> [[i16; FRAME * 2]; 2] {
    let mut pcm = [[0; FRAME * 2]; 2];
    for i in 0..FRAME {
        let time = (frame * FRAME as u64 + i as u64) as f64 / RATE as f64;
        for (track, frequency) in [997.0, 521.0].into_iter().enumerate() {
            let sample = (14_000.0 * (std::f64::consts::TAU * frequency * time).sin()) as i16;
            for channel in 0..CHANNELS[track] {
                pcm[track][i * CHANNELS[track] + channel] = sample;
            }
        }
    }
    pcm
}
