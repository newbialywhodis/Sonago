use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use super::Result;
use sha2::{Digest, Sha256};

const ASR: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/ed5d745623472f97cfc2e6c2916ee248798fc5a8";
const TAG: &str = "https://huggingface.co/k2-fsa/sherpa-onnx-zipformer-small-audio-tagging-2024-04-15/resolve/c24f9d0fdf7d0b8c0c4f9733aa0cac51fda10c95";
const SEG: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0";
const RELEASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

// A versioned cache is shared by the installed and portable apps.
const FILES: &[(&str, &str, &str, u64, &str)] = &[
    (
        "encoder.onnx",
        ASR,
        "encoder.int8.onnx",
        652282294,
        "1afbe69c1ffdabee94755f42e4daf1d93549131a3da11356bda84fcef0f2c479",
    ),
    (
        "decoder.onnx",
        ASR,
        "decoder.int8.onnx",
        11845274,
        "0222a86147299d4dee90edf95b6d13827edb42ad8b7c9cf0b2582f030937c0b5",
    ),
    (
        "joiner.onnx",
        ASR,
        "joiner.int8.onnx",
        6355277,
        "cbd02f3696e9944970111fb439d9c3fd365a80374565f13f2b9c7007d253fc98",
    ),
    (
        "tokens.txt",
        ASR,
        "tokens.txt",
        93939,
        "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    ),
    (
        "segmentation.onnx",
        SEG,
        "model.int8.onnx",
        1540506,
        "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
    ),
    (
        "embedding.onnx",
        RELEASE,
        "speaker-recongition-models/nemo_en_titanet_small.onnx",
        40257283,
        "ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e",
    ),
    (
        "music.onnx",
        TAG,
        "model.int8.onnx",
        27038066,
        "69304b8a1b96bbe6b7d16c24079f0732c65faf3568a14cb82a8238c709afe76c",
    ),
    (
        "labels.csv",
        TAG,
        "class_labels_indices.csv",
        14675,
        "cdd1049833c4b86127c2773ac0d14a2754b6a6d0d1798002ed5c66e699708429",
    ),
    (
        "vad.onnx",
        RELEASE,
        "asr-models/silero_vad.onnx",
        643854,
        "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    ),
];

pub fn prepare() -> Result<(PathBuf, fs::File)> {
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("Missing LOCALAPPDATA")?)
        .join("Sonago/models/v1");
    fs::create_dir_all(&root)?;
    // A previous tray session may still be finishing its last clips.
    let slot = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("worker.lock"))?;
    slot.lock()?;
    for &(name, base, remote, bytes, sha) in FILES {
        let path = root.join(name);
        if fs::metadata(&path).is_ok_and(|m| m.len() == bytes) {
            continue;
        }
        download(&path, &format!("{base}/{remote}"), bytes, sha)
            .map_err(|e| format!("Could not install {name}: {e}"))?;
    }
    Ok((root, slot))
}

fn download(path: &Path, url: &str, bytes: u64, sha: &str) -> Result<()> {
    use io::Seek;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    let curl = PathBuf::from(std::env::var_os("SystemRoot").ok_or("Missing Windows directory")?)
        .join("System32/curl.exe");
    let mut command = Command::new(curl);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW.0);
    }
    let result = command
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--speed-time",
            "60",
            "--speed-limit",
            "512",
            "--retry",
            "1",
            "--max-filesize",
            &bytes.to_string(),
            "--output",
        ])
        .arg(file.path())
        .arg(url)
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr)
            .trim()
            .to_owned()
            .into());
    }
    if file.as_file().metadata()?.len() != bytes {
        return Err("Incomplete model download".into());
    }
    file.rewind()?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if format!("{:x}", hash.finalize()) != sha {
        return Err("Model checksum mismatch".into());
    }
    file.persist(path)?;
    Ok(())
}
