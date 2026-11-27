use super::{RATE, Result};
use sherpa_onnx::*;
use std::{collections::BTreeMap, path::Path};

pub struct Analysis {
    asr: OfflineRecognizer,
    vad: VoiceActivityDetector,
    diar: OfflineSpeakerDiarization,
    voices: SpeakerEmbeddingExtractor,
    tag: AudioTagging,
    // A local diarization cluster number is valid for one block only.
    speakers: Vec<Option<Vec<f32>>>,
    pub lines: Vec<(usize, String, String)>,
    pub music: Vec<(usize, usize)>,
}

impl Analysis {
    pub fn begin_clip(&mut self) {
        self.speakers.clear();
        self.lines.clear();
        self.music.clear();
    }
    pub fn new(root: &Path) -> Result<Self> {
        let file = |name| Some(root.join(name).to_string_lossy().into_owned());
        let asr = OfflineRecognizer::create(&OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: file("encoder.onnx"),
                    decoder: file("decoder.onnx"),
                    joiner: file("joiner.onnx"),
                },
                tokens: file("tokens.txt"),
                model_type: Some("nemo_transducer".into()),
                num_threads: 2,
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            ..Default::default()
        })
        .ok_or("Could not load Parakeet")?;
        let vad = VoiceActivityDetector::create(
            &VadModelConfig {
                silero_vad: SileroVadModelConfig {
                    model: file("vad.onnx"),
                    threshold: 0.5,
                    min_silence_duration: 0.45,
                    min_speech_duration: 0.2,
                    window_size: 512,
                    max_speech_duration: 22.0,
                },
                sample_rate: RATE as i32,
                num_threads: 1,
                ..Default::default()
            },
            30.,
        )
        .ok_or("Could not load speech detector")?;
        let embedding = SpeakerEmbeddingExtractorConfig {
            model: file("embedding.onnx"),
            num_threads: 2,
            ..Default::default()
        };
        let diar = OfflineSpeakerDiarization::create(&OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: file("segmentation.onnx"),
                    ..Default::default()
                },
                num_threads: 2,
                ..Default::default()
            },
            embedding: embedding.clone(),
            ..Default::default()
        })
        .ok_or("Could not load speaker diarization")?;
        let voices = SpeakerEmbeddingExtractor::create(&embedding)
            .ok_or("Could not load speaker embeddings")?;
        let tag = AudioTagging::create(&AudioTaggingConfig {
            model: AudioTaggingModelConfig {
                zipformer: OfflineZipformerAudioTaggingModelConfig {
                    model: file("music.onnx"),
                },
                num_threads: 2,
                ..Default::default()
            },
            labels: file("labels.csv"),
            top_k: 8,
        })
        .ok_or("Could not load audio tagging")?;
        Ok(Self {
            asr,
            vad,
            diar,
            voices,
            tag,
            speakers: vec![],
            lines: vec![],
            music: vec![],
        })
    }

    pub fn block(
        &mut self,
        samples: &[f32],
        base: usize,
        source: &str,
        identify_speakers: bool,
    ) -> Result<()> {
        if quiet(samples) {
            return Ok(());
        }
        let first_music = self.music.len();
        for (i, window) in samples.chunks(10 * RATE).enumerate() {
            if quiet(window) || window.len() < RATE / 4 {
                continue;
            }
            if self.music_and_speech(window).0 >= 0.35 {
                let start = base + i * 10 * RATE;
                self.music.push((start, start + window.len()));
            }
        }
        let mut speech = self.speech_ranges(samples);
        speech.retain(|&(a, b)| {
            if !self.music[first_music..]
                .iter()
                .any(|&(from, to)| base + a < to && from < base + b)
            {
                return true;
            }
            // VAD can treat singing as conversation. Check the utterance
            // itself so a music tag on the surrounding window cannot mute it.
            let (music, talking) = self.music_and_speech(&samples[a..b]);
            !(music >= 0.60 && talking < 0.12)
        });
        let Some(&(first, _)) = speech.first() else {
            return Ok(());
        };
        let mut turns = Vec::new();
        if identify_speakers {
            // Leave a second of context, and skip leading/trailing silence.
            let from = first.saturating_sub(RATE);
            let to = (speech.last().unwrap().1 + RATE).min(samples.len());
            turns = self.turns(&samples[from..to], from)?;
        }
        for (start, end) in speech {
            let stream = self.asr.create_stream();
            stream.accept_waveform(RATE as i32, &samples[start..end]);
            self.asr.decode(&stream);
            let result = stream.get_result().ok_or("Transcription failed")?;
            let text = result.text.trim();
            if text.is_empty() {
                continue;
            }
            let timed = result.timestamps.as_deref().unwrap_or_default();
            if turns.is_empty() || timed.len() != result.tokens.len() || timed.is_empty() {
                self.lines.push((base + start, source.into(), text.into()));
                continue;
            }
            // Preserve the recognizer's punctuation and spelling. Map word
            // starts to token times only when the model exposes matching words.
            let starts: Vec<_> = result
                .tokens
                .iter()
                .zip(timed)
                .enumerate()
                .filter(|(i, (token, _))| *i == 0 || token.starts_with([' ', '▁']))
                .map(|(_, (_, t))| start + (*t * RATE as f32) as usize)
                .collect();
            let words: Vec<_> = text.split_whitespace().collect();
            if starts.len() != words.len() {
                let label = speaker_at(&turns, (start + end) / 2, source);
                self.lines.push((base + start, label, text.into()));
                continue;
            }
            let mut phrase = String::new();
            let mut label = String::new();
            let mut at = start;
            for (word, pos) in words.into_iter().zip(starts) {
                let next = speaker_at(&turns, pos, source);
                if next != label {
                    if !phrase.is_empty() {
                        self.lines
                            .push((base + at, label, std::mem::take(&mut phrase)));
                    }
                    label = next;
                    at = pos;
                }
                if !phrase.is_empty() {
                    phrase.push(' ');
                }
                phrase.push_str(word);
            }
            self.lines.push((base + at, label, phrase));
        }
        Ok(())
    }

    fn music_and_speech(&self, samples: &[f32]) -> (f32, f32) {
        let stream = self.tag.create_stream();
        stream.accept_waveform(RATE as i32, samples);
        let mut music = 0.;
        let mut talking = 0.0_f32;
        for event in self.tag.compute(&stream, 16) {
            match event.name.as_str() {
                "Music" => music = event.prob,
                "Speech" | "Conversation" | "Narration, monologue" => {
                    talking = talking.max(event.prob)
                }
                _ => {}
            }
        }
        (music, talking)
    }

    fn speech_ranges(&self, samples: &[f32]) -> Vec<(usize, usize)> {
        self.vad.reset();
        let mut ranges = Vec::new();
        let mut drain = || {
            while let Some(segment) = self.vad.front() {
                let start = (segment.start().max(0) as usize).min(samples.len());
                let end = (start + segment.samples().len()).min(samples.len());
                if end > start {
                    ranges.push((start, end));
                }
                self.vad.pop();
            }
        };
        for window in samples.chunks(512) {
            self.vad.accept_waveform(window);
            drain();
        }
        self.vad.flush();
        drain();
        ranges
    }

    fn turns(&mut self, samples: &[f32], offset: usize) -> Result<Vec<(usize, usize, usize)>> {
        let result = self
            .diar
            .process(samples)
            .ok_or("Speaker diarization failed")?;
        let turns = result.sort_by_start_time();
        let mut best = BTreeMap::new();
        let mut intervals = Vec::new();
        for turn in turns {
            let from = ((turn.start * RATE as f32) as usize).min(samples.len());
            let to = ((turn.end * RATE as f32) as usize).min(samples.len());
            if to <= from {
                continue;
            }
            intervals.push((from, to, turn.speaker));
            let sample = best.entry(turn.speaker).or_insert(from..to);
            if to - from > sample.len() {
                *sample = from..to;
            }
        }
        let prior = self.speakers.len();
        let mut assigned = BTreeMap::new();
        for (local, range) in best {
            let stream = self
                .voices
                .create_stream()
                .ok_or("Could not open speaker stream")?;
            stream.accept_waveform(
                RATE as i32,
                &samples[range.start..range.end.min(range.start + RATE * 8)],
            );
            stream.input_finished();
            let voice = self
                .voices
                .is_ready(&stream)
                .then(|| self.voices.compute(&stream))
                .flatten();
            let matched = voice.as_ref().and_then(|v| {
                self.speakers[..prior]
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !assigned.values().any(|used| used == i))
                    .filter_map(|(i, stored)| Some((i, cosine(v, stored.as_ref()?))))
                    .filter(|(_, similarity)| *similarity >= 0.55)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
            });
            let id = matched.unwrap_or_else(|| {
                self.speakers.push(voice);
                self.speakers.len() - 1
            });
            assigned.insert(local, id);
        }
        Ok(intervals
            .into_iter()
            .map(|(a, b, id)| (offset + a, offset + b, assigned[&id] + 1))
            .collect())
    }
}

fn quiet(samples: &[f32]) -> bool {
    samples.iter().all(|x| x.abs() < 0.0003)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (norm(a) * norm(b)).max(f32::EPSILON)
}

fn speaker_at(turns: &[(usize, usize, usize)], pos: usize, source: &str) -> String {
    let mut active = turns.iter().filter(|(a, b, _)| *a <= pos && pos < *b);
    match (active.next(), active.next()) {
        (Some((_, _, id)), None) => format!("Speaker {id}"),
        (Some(_), Some(_)) => format!("{source} (overlap)"),
        _ => source.into(),
    }
}
