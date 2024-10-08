//! A simple audio player for [Sensit](https://sensit.tech) technical interview.
//!
//! # Commands
//! + `q`: quit
//! + `p`: play/pause
//! + `k`: play next track
//! + `j`: play previous track
//! + `r`: restart
//! + `l`: toggle looping
//! + `a`: toggle autoplay
//! + `s`: toggle show state
//!
//! # References
//! + https://github.com/dceddia/ffmpeg-cpal-play-audio
//! + https://www.bekk.christmas/post/2023/19/make-some-noise-with-rust
mod input_actor;
mod player_actor;

use cpal::traits::*;
use crossbeam::{channel, select};
use ffmpeg_next as ffm;
use sensit_audio_cli as lib;
use std::{
    cmp, env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

macro_rules! write_trace {
    ($dst:expr, $($arg:tt)*) => {
        if let Err(err) = $dst.write_fmt(std::format_args!($($arg)*)) {
            tracing::error!(?err);
        }
    };
}

const AUDIO_BUFFER_SIZE: usize = 8192;
const CMD_KEY_QUIT: &str = "q";
const CMD_KEY_PREVIOUS: &str = "j";
const CMD_KEY_NEXT: &str = "k";
const CMD_KEY_RESTART: &str = "r";
const CMD_KEY_TOGGLE_PLAY: &str = "p";
const CMD_KEY_TOGGLE_LOOP: &str = "l";
const CMD_KEY_TOGGLE_AUTOPLAY: &str = "a";
const CMD_KEY_TOGGLE_SHOW_STATE: &str = "s";

#[derive(Debug)]
enum Command {
    Quit,
    Next,
    Previous,
    Restart,
    TogglePlay,
    ToggleLoop,
    ToggleAutoplay,
    ToggleShowState,
}

pub fn main() -> Result<(), ()> {
    log::enable();

    let mut args = env::args();
    let program = args.next().expect("program name");
    let mut args = args.collect::<Vec<String>>();
    let dir = match args.len() {
        0 => {
            tracing::info!("No path provided, using current location.");
            env::current_dir().expect("can not get current directory")
        }
        1 => PathBuf::from(args.remove(0)),
        _ => {
            let mut stdout = io::stdout();
            writeln!(stdout, "{program} use").expect("write to stdout");
            writeln!(stdout, "{program} <path>").expect("write to stdout");
            return Err(());
        }
    };

    ffm::init().expect("could not initialize ffmpeg");
    let (output_device, stream_config) = init_cpal();
    let stream_builder =
        lib::AudioStreamBuilder::new(output_device, stream_config, AUDIO_BUFFER_SIZE);

    run(stream_builder, dir);
    Ok(())
}

/// # Arguments
/// + `dir`: Path to directory containing sound files.
fn run(stream_builder: lib::AudioStreamBuilder, dir: impl AsRef<Path>) {
    let playlist = create_playlist_from_dir(dir.as_ref());
    if playlist.is_empty() {
        tracing::info!("No audio files are present");
        return;
    }
    let queue = lib::PlaylistQueue::new(playlist);

    let (input_tx, input_rx) = channel::bounded(1);
    let mut input_listener = input_actor::InputActor::new(input_tx);
    let _t_input = std::thread::Builder::new()
        .name("input actor".to_string())
        .spawn(move || input_listener.run())
        .expect("could not launch input actor");

    let (command_tx, command_rx) = channel::bounded(1);
    let (event_tx, event_rx) = channel::bounded(1);
    let _t_player = std::thread::Builder::new()
        .name("player actor".to_string())
        .spawn(move || {
            let mut player =
                player_actor::AudioPlayerActor::new(stream_builder, command_rx, event_tx);

            player.run();
        })
        .expect("could not launch player actor");

    let mut jukebox = JukeBox::new(queue, input_rx, command_tx, event_rx);
    jukebox.run()
}

struct JukeboxConfig {
    /// Automatically play the next song.
    autoplay: bool,

    /// Show the current player state.
    show_state: bool,

    /// The number of songs to show on either side of the current one.
    /// i.e. `0` will only show the current song,
    /// `1` will show the the previous, current, and next song.
    playlist_buffer: usize,
}

impl Default for JukeboxConfig {
    fn default() -> Self {
        Self {
            autoplay: true,
            playlist_buffer: 1,
            show_state: true,
        }
    }
}

struct JukeBox {
    queue: lib::PlaylistQueue,
    input_rx: channel::Receiver<Command>,
    command_tx: channel::Sender<player_actor::Command>,
    event_rx: channel::Receiver<player_actor::Event>,
    stream_state: Option<lib::StreamStateLock>,
    cfg: JukeboxConfig,
}

impl JukeBox {
    pub fn new(
        queue: lib::PlaylistQueue,
        input_rx: channel::Receiver<Command>,
        command_tx: channel::Sender<player_actor::Command>,
        event_rx: channel::Receiver<player_actor::Event>,
    ) -> Self {
        Self {
            queue,
            input_rx,
            command_tx,
            event_rx,
            stream_state: None,
            cfg: JukeboxConfig::default(),
        }
    }

    fn run(&mut self) {
        self.prepare_current_song()
            .map_err(|_| ())
            .expect("could not play song");

        if self.cfg.autoplay {
            self.play();
        }

        loop {
            select! {
                recv(self.input_rx) -> cmd => match cmd{
                    Ok(cmd) => {
                        tracing::debug!(?cmd);
                        if matches!(cmd, Command::Quit) {
                            break;
                        }

                        if let Err(err) = self.handle_command(cmd) {
                            tracing::info!("An error occured");
                            tracing::error!(?err);
                            break;
                        };
                    }
                    Err(_) => {
                        tracing::info!("An error occured");
                        tracing::debug!("command channel closed");
                        break;
                    }
                },

                recv(self.event_rx) -> event => match event{
                    Ok(event) => {
                        tracing::debug!(?event);
                        if let Err(err) = self.handle_event(event) {
                            tracing::info!("An error occured");
                            tracing::error!(?err);
                            break;
                        }
                    },
                    Err(_) => {
                        tracing::info!("An error occured");
                        tracing::debug!("input channel closed");
                        break;
                    }
                },
            }
        }
    }

    fn handle_command(&mut self, cmd: Command) -> Result<(), ()> {
        match cmd {
            Command::Next => {
                self.prepare_next_song().map_err(|_| ())?;
                self.play();
            }
            Command::Previous => {
                self.play_previous_song().map_err(|_| ())?;
                self.play();
            }
            Command::Restart => {
                // TODO: Currently unloads and reloads the audio file.
                // Should be able to seek and restart without unloading.
                let state = self
                    .stream_state
                    .as_ref()
                    .map(|state_lock| *state_lock.lock().unwrap());

                self.prepare_current_song().map_err(|_| ())?;
                if matches!(state, Some(lib::StreamState::Play)) {
                    self.play();
                }
            }
            Command::TogglePlay => {
                self.toggle_play().map_err(|_| ())?;
            }
            Command::ToggleLoop => {
                self.queue.set_looping(!self.queue.is_looping());
            }
            Command::ToggleAutoplay => {
                self.cfg.autoplay = !self.cfg.autoplay;
                tracing::info!("autoplay {:?}", self.cfg.autoplay);
            }
            Command::ToggleShowState => {
                self.cfg.show_state = !self.cfg.show_state;
                tracing::info!("show state {:?}", self.cfg.show_state);
            }
            Command::Quit => unreachable!("handled elsewhere"),
        }

        Ok(())
    }

    fn handle_event(&mut self, event: player_actor::Event) -> Result<(), error::Player> {
        match event {
            player_actor::Event::Done => {
                if self.cfg.autoplay {
                    self.prepare_next_song()?;
                    self.play();
                    Ok(())
                } else {
                    self.prepare_next_song()?;
                    Ok(())
                }
            }
            player_actor::Event::StreamErr(err) => {
                tracing::error!(?err);
                Err(error::Player::Stream(err))
            }
        }
    }

    fn prepare_current_song(&mut self) -> Result<(), error::Player> {
        if let Some(file) = self.queue.current().cloned() {
            self.load_and_prepare_stream(file.clone())
        } else {
            self.pause();
            tracing::info!("End of playlist");
            Ok(())
        }
    }

    fn prepare_next_song(&mut self) -> Result<(), error::Player> {
        if let Some(file) = self.queue.next().cloned() {
            self.load_and_prepare_stream(file.clone())
        } else {
            self.pause();
            tracing::info!("End of playlist");
            Ok(())
        }
    }

    fn play_previous_song(&mut self) -> Result<(), error::Player> {
        if let Some(file) = self.queue.next_back().cloned() {
            self.load_and_prepare_stream(file)
        } else {
            self.pause();
            tracing::info!("End of playlist");
            Ok(())
        }
    }

    /// Loads a new song to the player actor and begins playing it.
    ///
    /// # Returns
    /// + `Err` if the command channel closed.
    fn load_and_prepare_stream(&mut self, file: PathBuf) -> Result<(), error::Player> {
        if let Some(state_lock) = self.stream_state.as_ref() {
            *state_lock.lock().unwrap() = lib::StreamState::Stop;
        };

        let (res_tx, res_rx) = channel::bounded(1);
        self.command_tx
            .send(player_actor::Command::Load(file.clone(), res_tx))?;

        if let Err(err) = res_rx.recv()? {
            tracing::error!(?err);
            return Err(err.into());
        }
        tracing::trace!("{file:?} loaded");

        let (res_tx, res_rx) = channel::bounded(1);
        self.command_tx
            .send(player_actor::Command::Prepare(res_tx))?;

        match res_rx.recv()? {
            Ok(stream_state) => {
                tracing::debug!("{:?}", stream_state.lock().unwrap());
                let _ = self.stream_state.insert(stream_state);
            }
            Err(err) => {
                tracing::error!(?err);
                return Err(err.into());
            }
        }

        if self.cfg.show_state {
            let mut stdout = io::stdout();
            let playlist = self.queue.playlist();
            let index = self.queue.index();
            let idx_start = index.checked_sub(self.cfg.playlist_buffer).unwrap_or(0);
            let idx_end = cmp::min(index + self.cfg.playlist_buffer, playlist.len());
            for idx in idx_start..=idx_end {
                if idx == playlist.len() {
                    write_trace!(stdout, "[END]\n");
                } else if idx == index && self.cfg.playlist_buffer > 0 {
                    // highlight current song
                    write_trace!(stdout, "\x1B[1;7m");
                    write_trace!(
                        stdout,
                        "{:>3}. {}\n",
                        idx + 1,
                        playlist[idx].to_string_lossy()
                    );
                    write_trace!(stdout, "\x1B[0m");
                } else {
                    write_trace!(
                        stdout,
                        "{:>3}. {}\n",
                        idx + 1,
                        playlist[idx].to_string_lossy()
                    );
                }
            }
            if idx_end < playlist.len() {
                write_trace!(stdout, "     (of {} tracks)", playlist.len()); // align with track
            }
            write_trace!(stdout, "\n");
            write_trace!(
                stdout,
                "looping: {:?}, autoplay: {:?}\n",
                self.queue.is_looping(),
                self.cfg.autoplay,
            );
        }

        Ok(())
    }

    fn play(&mut self) {
        if let Some(state_lock) = self.stream_state.as_ref() {
            let mut state = state_lock.lock().unwrap();
            *state = lib::StreamState::Play;
            tracing::info!("Playing");
        }
    }

    fn pause(&mut self) {
        if let Some(state_lock) = self.stream_state.as_ref() {
            let mut state = state_lock.lock().unwrap();
            *state = lib::StreamState::Pause;
            tracing::info!("Paused");
        }
    }

    fn toggle_play(&mut self) -> Result<(), channel::SendError<player_actor::Command>> {
        let Some(state_lock) = self.stream_state.as_ref() else {
            return Ok(());
        };

        let mut state = state_lock.lock().unwrap();
        if state.is_playing() {
            *state = lib::StreamState::Pause;
            tracing::info!("Paused");
        } else {
            *state = lib::StreamState::Play;
            tracing::info!("Playing");
        }

        Ok(())
    }
}

/// Creates a playlist from files in a directory.
/// Files that do not contain audio or can not be read are ignored.
/// Directory is walked recursively.
///
/// # Arguments
/// + `dir`: Path to directory containing sound files.
fn create_playlist_from_dir(dir: impl AsRef<Path>) -> lib::Playlist {
    let audio_files = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let Ok(ctx) = ffm::format::input(entry.path()) else {
                return None;
            };

            if ctx.streams().best(ffm::media::Type::Audio).is_some() {
                fs::canonicalize(entry.into_path()).ok()
            } else {
                None
            }
        })
        .collect();

    lib::Playlist::new(audio_files)
}

fn init_cpal() -> (cpal::Device, cpal::SupportedStreamConfig) {
    let device = cpal::default_host()
        .default_output_device()
        .expect("no output device available");

    // Create an output stream for the audio so we can play it
    // NOTE: If system doesn't support the file's sample rate, the program will panic when we try to play,
    //       so we'll need to resample the audio to a supported config
    let supported_config_range = device
        .supported_output_configs()
        .expect("error querying audio output configs")
        .next()
        .expect("no supported audio config found");

    (device, supported_config_range.with_max_sample_rate())
}

mod error {
    use super::player_actor;
    use crossbeam::channel;
    use ffmpeg_next as ffm;
    use sensit_audio_cli as lib;

    #[derive(thiserror::Error, Debug)]
    pub enum Player {
        #[error("channel closed")]
        Channel,

        #[error("no stream loaded")]
        NoStream,

        #[error("could not load audio: {0}")]
        Load(ffm::Error),

        #[error("could not play audio: {0:?}")]
        Stream(lib::error::AudioStream),
    }

    impl<T> From<channel::SendError<T>> for Player {
        fn from(_: channel::SendError<T>) -> Self {
            Self::Channel
        }
    }

    impl From<channel::RecvError> for Player {
        fn from(_: channel::RecvError) -> Self {
            Self::Channel
        }
    }

    impl From<player_actor::error::Load> for Player {
        fn from(value: player_actor::error::Load) -> Self {
            use player_actor::error::Load;

            match value {
                Load::Audio(err) => Self::Load(err),
                Load::Stream(err) => Self::Load(err),
            }
        }
    }

    impl From<player_actor::error::Play> for Player {
        fn from(value: player_actor::error::Play) -> Self {
            use player_actor::error::Play;

            match value {
                Play::NoStream => Self::NoStream,
            }
        }
    }
}

mod log {
    use tracing_subscriber::{fmt, prelude::*};

    pub fn enable() {
        tracing_subscriber::registry().with(fmt::layer()).init();
    }
}
