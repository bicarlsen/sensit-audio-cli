use crossbeam::channel;
use ffmpeg_next as ffm;
use sensit_audio_cli as lib;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Event {
    /// Current stream errored.
    StreamErr(ffm::Error),

    /// Current playing song has finished.
    Done,
}

pub type LoadResponse = Result<(), error::Load>;
pub type PlayResponse = Result<lib::StreamStateLock, error::Play>;

#[derive(Debug)]
pub enum Command {
    Load(PathBuf, channel::Sender<LoadResponse>),
    Play(channel::Sender<PlayResponse>),
    Pause,

    /// Close the player
    Close,
}

pub struct AudioPlayerActor {
    builder: lib::AudioStreamBuilder,
    command_rx: channel::Receiver<Command>,
    event_tx: channel::Sender<Event>,
    stream: Option<lib::AudioStream>,
}

impl AudioPlayerActor {
    pub fn new(
        builder: lib::AudioStreamBuilder,
        command_rx: channel::Receiver<Command>,
        event_tx: channel::Sender<Event>,
    ) -> Self {
        Self {
            builder,
            command_rx,
            event_tx,
            stream: None,
        }
    }

    pub fn run(&mut self) {
        loop {
            if let Ok(cmd) = self.command_rx.recv() {
                match cmd {
                    Command::Load(file, res_tx) => self.handle_load(file, res_tx),
                    Command::Play(res_tx) => self.handle_play(res_tx),
                    Command::Pause => todo!(),
                    Command::Close => {
                        tracing::debug!("closing player actor");
                        break;
                    }
                }
            } else {
                tracing::trace!("command channel closed");
                break;
            }
        }
    }
}

impl AudioPlayerActor {
    fn handle_load(&mut self, file: PathBuf, res_tx: channel::Sender<LoadResponse>) {
        let audio = match lib::AudioFile::from_path(file).map_err(error::Load::Audio) {
            Ok(audio) => audio,
            Err(err) => {
                tracing::debug!(?err);
                res_tx.send(Err(err)).unwrap();
                return;
            }
        };

        let stream = match self.builder.load(audio).map_err(error::Load::Stream) {
            Ok(stream) => stream,
            Err(err) => {
                tracing::debug!(?err);
                res_tx.send(Err(err)).unwrap();
                return;
            }
        };

        let _ = self.stream.insert(stream);
        res_tx.send(Ok(())).unwrap();
    }

    fn handle_play(&mut self, res_tx: channel::Sender<PlayResponse>) {
        let Some(stream) = self.stream.as_mut() else {
            res_tx.send(Err(error::Play::NoStream)).unwrap();
            return;
        };

        res_tx.send(Ok(stream.state())).unwrap();
        if let Err(err) = stream.play().map_err(|err| Event::StreamErr(err)) {
            tracing::debug!(?err);
            self.event_tx.send(err).unwrap();
            return;
        };
    }
}

pub mod error {
    use ffmpeg_next as ffm;

    #[derive(Debug)]
    pub enum Load {
        /// Could not create [`AudioFile`](lib::AudioFile) from path.
        Audio(ffm::Error),

        /// Could not create [`AudioStream`](lib::AudioStream)
        /// from the [`AudioFile`](lib::AudioFile) .
        Stream(ffm::Error),
    }

    #[derive(Debug)]
    pub enum Play {
        /// No stream is loaded.
        NoStream,

        /// Could not play [`AudioStream`](lib::AudioStream)
        Play(ffm::Error),
    }
}
