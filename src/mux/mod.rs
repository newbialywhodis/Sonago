mod matroska;
mod ogg_opus;

use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Result, history::Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Matroska,
    OggOpus,
}

pub fn save(snapshot: &Snapshot, directory: &Path, format: Format) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    match format {
        Format::Matroska => {
            let destination = directory.join(format!("sonago-{id}.mka"));
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            let mut writer = BufWriter::new(temporary.as_file_mut());
            matroska::write(snapshot, &mut writer)?;
            writer.flush()?;
            drop(writer);
            temporary.as_file().sync_all()?;
            temporary.persist_noclobber(&destination)?;
            Ok(destination)
        }
        Format::OggOpus => {
            let temporary = tempfile::Builder::new()
                .prefix(".sonago-")
                .tempdir_in(directory)?;
            let destination = directory.join(format!("sonago-{id}"));
            let [system, mic] =
                ["system.opus", "microphone.opus"].map(|name| temporary.path().join(name));
            let files = [fs::File::create(system)?, fs::File::create(mic)?];
            let mut writers = files.map(BufWriter::new);
            ogg_opus::write(snapshot, &mut writers)?;
            for mut writer in writers {
                writer.flush()?;
                writer.get_ref().sync_all()?;
            }
            // The directory lives on the same volume as its final name.
            fs::rename(temporary.path(), &destination)?;
            Ok(destination)
        }
    }
}

#[cfg(test)]
mod tests;
