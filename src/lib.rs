mod codec;
pub mod engine;
mod history;
mod mux;
mod timeline;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
pub mod wasapi_input;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

const RATE: usize = 48_000;
const FRAME: usize = 960;
const FRAME_MS: u64 = 20;
const FRAMES_PER_SECOND: u64 = 50;
const CHANNELS: [usize; 2] = [2, 1];
