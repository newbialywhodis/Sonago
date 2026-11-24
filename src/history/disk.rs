use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

enum Job {
    Write(Arc<Vec<u8>>, Sender<Job>),
    Close(File),
}

pub(super) struct Spool {
    jobs: Sender<Job>,
    completed: Receiver<io::Result<DiskFile>>,
    pending: Option<u64>,
}

impl Spool {
    pub(super) fn start() -> io::Result<(Self, JoinHandle<()>)> {
        Self::start_with(|bytes| {
            let mut file = tempfile::tempfile()?;
            file.write_all(bytes)?;
            Ok(file)
        })
    }

    pub(super) fn start_with(
        mut write: impl FnMut(&[u8]) -> io::Result<File> + Send + 'static,
    ) -> io::Result<(Self, JoinHandle<()>)> {
        let (jobs, queue) = mpsc::channel();
        let (reply, completed) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("hearlier-history-disk".into())
            .spawn(move || {
                for job in queue {
                    match job {
                        Job::Write(bytes, jobs) => {
                            let result = write(&bytes).map(|file| DiskFile {
                                file: Mutex::new(Some(file)),
                                jobs,
                            });
                            let _ = reply.send(result);
                        }
                        Job::Close(file) => drop(file),
                    }
                }
            })?;
        Ok((
            Self {
                jobs,
                completed,
                pending: None,
            },
            worker,
        ))
    }

    pub(super) fn schedule(&mut self, first: u64, bytes: Arc<Vec<u8>>) -> io::Result<()> {
        // One write can be outstanding. Close jobs only retire existing files.
        if self.pending.is_none() {
            self.jobs
                .send(Job::Write(bytes, self.jobs.clone()))
                .map_err(|_| disconnected())?;
            self.pending = Some(first);
        }
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<(u64, io::Result<DiskFile>)> {
        let first = self.pending?;
        let result = match self.completed.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(disconnected()),
        };
        self.pending = None;
        Some((first, result))
    }

    #[cfg(test)]
    pub(super) fn wait(&mut self) -> Option<(u64, io::Result<DiskFile>)> {
        let first = self.pending.take()?;
        Some((
            first,
            self.completed
                .recv()
                .unwrap_or_else(|_| Err(disconnected())),
        ))
    }
}

fn disconnected() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "Wątek historii dyskowej zakończył pracę.",
    )
}

pub(super) struct DiskFile {
    file: Mutex<Option<File>>,
    jobs: Sender<Job>,
}

impl DiskFile {
    pub(super) fn read(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("Blokada segmentu została przerwana."))?;
        let file = file
            .as_mut()
            .expect("the segment keeps its file until drop");
        file.rewind()?;
        buffer.clear();
        file.read_to_end(buffer)?;
        Ok(())
    }
}

impl Drop for DiskFile {
    fn drop(&mut self) {
        let file = self.file.get_mut().unwrap_or_else(|err| err.into_inner());
        if let Some(file) = file.take() {
            let _ = self.jobs.send(Job::Close(file));
        }
    }
}
