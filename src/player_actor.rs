use crossbeam::channel;
use sensit_audio_cli as lib;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Event {
    /// Current stream errored.
    StreamErr(lib::error::AudioStream),

    /// Current playing song has finished.
    Done,
}

pub type LoadResponse = Result<(), error::Load>;
pub type PrepareResponse = Result<lib::StreamStateLock, error::Play>;

#[derive(Debug)]
pub enum Command {
    Load(PathBuf, channel::Sender<LoadResponse>),
    Prepare(channel::Sender<PrepareResponse>),
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
                    Command::Load(file, res_tx) => {
                        if let Err(_) = self.handle_load(file, res_tx) {
                            tracing::error!("response channel closed");
                        }
                    }
                    Command::Prepare(res_tx) => {
                        if let Err(_) = self.handle_prepare(res_tx) {
                            tracing::error!("response channel closed");
                        }
                    }
                }
            } else {
                tracing::error!("command channel closed");
                break;
            }
        }
    }
}

impl AudioPlayerActor {
    /// # Returns
    /// + `Err` if the response could not be handled.
    fn handle_load(
        &mut self,
        file: PathBuf,
        res_tx: channel::Sender<LoadResponse>,
    ) -> Result<(), error::Channel> {
        let audio = match lib::AudioFile::from_path(file).map_err(error::Load::Audio) {
            Ok(audio) => audio,
            Err(err) => {
                tracing::debug!(?err);
                res_tx.send(Err(err))?;
                return Ok(());
            }
        };

        let stream = match self.builder.load(audio).map_err(error::Load::Stream) {
            Ok(stream) => stream,
            Err(err) => {
                tracing::debug!(?err);
                res_tx.send(Err(err))?;
                return Ok(());
            }
        };

        let _ = self.stream.insert(stream);
        res_tx.send(Ok(()))?;
        Ok(())
    }

    /// # Returns
    /// + `Err` if the response could not be handled.
    fn handle_prepare(
        &mut self,
        res_tx: channel::Sender<PrepareResponse>,
    ) -> Result<(), error::Channel> {
        let Some(stream) = self.stream.as_mut() else {
            res_tx.send(Err(error::Play::NoStream))?;
            return Ok(());
        };

        res_tx.send(Ok(stream.state())).unwrap();
        if let Err(err) = stream.load().map_err(Event::StreamErr) {
            tracing::debug!(?err);
            self.event_tx.send(err)?;
            return Ok(());
        };

        if stream.state().lock().unwrap().is_done() {
            self.event_tx.send(Event::Done)?;
        }
        Ok(())
    }
}

pub mod error {
    use crossbeam::channel;
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
    }

    /// A channel was closed.
    pub struct Channel;
    impl<T> From<channel::SendError<T>> for Channel {
        fn from(_: channel::SendError<T>) -> Self {
            Self
        }
    }

    impl From<channel::RecvError> for Channel {
        fn from(_: channel::RecvError) -> Self {
            Self
        }
    }
}
