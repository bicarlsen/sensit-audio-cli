use cpal::traits::*;
use ffmpeg_next as ffm;
use sensit_audio_cli as lib;
use std::{fs, io, path::Path};

const AUDIO_BUFFER_SIZE: usize = 8192;

pub fn main() {
    log::enable();
    ffm::init().expect("could not initialize ffmpeg");
    let (output_device, stream_config) = init_cpal();
    let mut player = lib::AudioFilePlayer::new(output_device, stream_config, AUDIO_BUFFER_SIZE);

    run(&mut player, "samples");
}

/// # Arguments
/// + `dir`: Path to directory containing sound files.
fn run(player: &mut lib::AudioFilePlayer, dir: impl AsRef<Path>) {
    let playlist = create_playlist_from_dir(dir.as_ref());
    let mut jukebox = lib::Jukebox::new(playlist);
    let mut input = String::new();
    loop {
        // TODO: Don't wait for new line.
        input.clear();
        tracing::info!("waiting for input");
        io::stdin().read_line(&mut input).expect("invalid input");
        let cmd = input.trim();
        let Some(cmd) = Command::from_str(cmd) else {
            continue;
        };

        match cmd {
            Command::Next => next_song(&mut jukebox, player),
            Command::Previous => previous_song(&mut jukebox, player),
            Command::TogglePlay => toggle_play(&mut jukebox),
        }
    }
}

fn next_song(jukebox: &mut lib::Jukebox, player: &mut lib::AudioFilePlayer) {
    let file = jukebox.next().unwrap();
    tracing::info!("playing {:?}", file.path());
    player.play(file).unwrap();
}

fn previous_song(jukebox: &mut lib::Jukebox, player: &mut lib::AudioFilePlayer) {
    let file = jukebox.next_back().unwrap();
    tracing::info!("playing {:?}", file.path());
    player.play(file).unwrap();
}

fn toggle_play(player: &mut lib::Jukebox) {
    tracing::info!("play");
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
                fs::canonicalize(entry.into_path())
                    .ok()
                    .map(|path| lib::AudioFile::new(path, ctx))
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

enum Command {
    Next,
    Previous,
    TogglePlay,
}

impl Command {
    pub fn from_str(input: impl AsRef<str>) -> Option<Self> {
        match input.as_ref() {
            "j" => Some(Self::Previous),
            "k" => Some(Self::Next),
            "p" => Some(Self::TogglePlay),
            _ => None,
        }
    }
}

mod log {
    use tracing_subscriber::{fmt, prelude::*};

    pub fn enable() {
        tracing_subscriber::registry().with(fmt::layer()).init();
    }
}
