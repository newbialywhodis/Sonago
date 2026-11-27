mod analysis;
mod audio;
mod models;

use std::{
    io::{BufWriter, Write},
    path::Path,
    process::ExitCode,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const RATE: usize = 16_000;
const BLOCK: usize = RATE * 120;

#[derive(Default)]
struct Worker {
    loaded: Option<(analysis::Analysis, std::fs::File)>,
}

impl Worker {
    fn transcribe(&mut self, clip: &Path) -> Result<()> {
        if !clip.exists() {
            return Err("The saved clip could not be found".into());
        }
        let output = if clip.is_dir() {
            clip.join("transcript.txt")
        } else {
            clip.with_extension("txt")
        };
        if output.try_exists()? {
            return Err("A transcript already exists for this clip".into());
        }
        if self.loaded.is_none() {
            let (root, slot) = models::prepare()?;
            self.loaded = Some((analysis::Analysis::new(&root)?, slot));
        }
        let analysis = &mut self.loaded.as_mut().unwrap().0;
        analysis.begin_clip();
        for mic in [false, true] {
            let source = if mic { "Microphone" } else { "System" };
            // A microphone-only meeting can contain several people in the room.
            let identify_speakers = !mic || analysis.lines.is_empty();
            let mut buffer = Vec::with_capacity(BLOCK + 320);
            let mut at = 0;
            audio::read(clip, mic, |pcm| {
                buffer.extend_from_slice(pcm);
                if buffer.len() >= BLOCK {
                    // Prefer the quietest 20 ms within the final second of the block.
                    let tail = BLOCK - RATE;
                    let (index, _) = buffer[tail..BLOCK]
                        .as_chunks::<320>()
                        .0
                        .iter()
                        .enumerate()
                        .map(|(i, w)| (i, w.iter().map(|x| x * x).sum::<f32>()))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .unwrap();
                    let cut = tail + index * 320 + 160;
                    analysis.block(&buffer[..cut], at, source, identify_speakers)?;
                    buffer.drain(..cut);
                    at += cut;
                }
                Ok(())
            })?;
            analysis.block(&buffer, at, source, identify_speakers)?;
        }
        analysis.music.sort_unstable();
        let mut intervals: Vec<(usize, usize)> = Vec::new();
        for (from, to) in analysis.music.drain(..) {
            if let Some(last) = intervals.last_mut()
                && from <= last.1
            {
                last.1 = last.1.max(to);
            } else {
                intervals.push((from, to));
            }
        }
        let mut lines: Vec<_> = analysis
            .lines
            .drain(..)
            .map(|(at, label, text)| (at, format!("[{}] {label}: {text}", stamp(at))))
            .collect();
        for (from, to) in intervals {
            lines.push((from, format!("[{}–{}] Music", stamp(from), stamp(to))));
        }
        lines.sort_by_key(|(at, _)| *at);
        let mut file = tempfile::NamedTempFile::new_in(output.parent().unwrap())?;
        let mut writer = BufWriter::new(file.as_file_mut());
        if lines.is_empty() {
            writeln!(writer, "No speech or music detected.")?;
        }
        for (_, line) in lines {
            writeln!(writer, "{line}")?;
        }
        writer.flush()?;
        drop(writer);
        file.as_file().sync_all()?;
        file.persist_noclobber(output)?;
        Ok(())
    }
}

fn stamp(sample: usize) -> String {
    let s = sample / RATE;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn main() -> ExitCode {
    let Some(clip) = std::env::args_os().nth(1) else {
        eprintln!("Usage: sonago-transcribe <Sonago clip or Ogg export folder>");
        return ExitCode::FAILURE;
    };
    let mut worker = Worker::default();
    if clip == "--queue" {
        use std::{
            io::{self, BufRead},
            sync::mpsc,
            thread,
        };
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
            };
            let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
        let (send, paths) = mpsc::channel();
        thread::spawn(move || {
            for path in io::stdin()
                .lock()
                .lines()
                .map_while(std::result::Result::ok)
            {
                if send.send(path).is_err() {
                    break;
                }
            }
        });
        for path in paths {
            let error = worker
                .transcribe(Path::new(&path))
                .err()
                .map(|e| e.to_string());
            // The tray may already have exited. Finish the remaining clips.
            let _ = writeln!(
                io::stdout().lock(),
                "{}",
                serde_json::to_string(&error).unwrap()
            );
        }
        return ExitCode::SUCCESS;
    }
    match worker.transcribe(Path::new(&clip)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
