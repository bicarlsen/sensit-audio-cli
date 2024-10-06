use cpal::traits::*;
use ffmpeg_next as ffm;
use ringbuf::traits::*;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(derive_more::Debug)]
pub struct AudioFile {
    path: PathBuf,

    #[debug(skip)]
    ctx: ffm::format::context::Input,
}

impl AudioFile {
    pub fn from_path(path: PathBuf) -> Result<Self, ffm::Error> {
        let ctx = ffm::format::input(&path)?;
        Ok(Self { path, ctx })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn ctx(&self) -> &ffm::format::context::Input {
        &self.ctx
    }

    pub fn ctx_mut(&mut self) -> &mut ffm::format::context::Input {
        &mut self.ctx
    }
}

#[derive(Debug, derive_more::Deref, derive_more::DerefMut)]
pub struct Playlist(Vec<PathBuf>);
impl Playlist {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self(files)
    }
}

#[derive(Debug)]
pub struct PlaylistQueue {
    playlist: Playlist,
    index: usize,
    cfg: AudioPlayConfig,
}

impl PlaylistQueue {
    pub fn new(playlist: Playlist) -> Self {
        Self {
            playlist,
            index: 0,
            cfg: AudioPlayConfig::default(),
        }
    }

    pub fn current(&self) -> Option<&PathBuf> {
        self.playlist.get(self.index)
    }

    pub fn next(&mut self) -> Option<&PathBuf> {
        if self.cfg.loop_playlist {
            self.index += 1;
            if self.index >= self.playlist.len() {
                self.index = 0;
            }

            Some(&self.playlist[self.index])
        } else {
            if self.index < self.playlist.len() {
                self.index += 1;
            }
            self.playlist.get(self.index)
        }
    }

    pub fn next_back(&mut self) -> Option<&PathBuf> {
        if self.cfg.loop_playlist {
            if self.index == 0 {
                self.index = self.playlist.len();
            }
            self.index -= 1;

            Some(&self.playlist[self.index])
        } else {
            if self.index == 0 {
                None
            } else {
                self.index -= 1;
                Some(&self.playlist[self.index])
            }
        }
    }

    pub fn playlist(&self) -> &Vec<PathBuf> {
        &self.playlist
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn len(&self) -> usize {
        self.playlist.len()
    }

    pub fn set_index(&mut self, index: usize) -> Result<(), error::InvalidIndex> {
        if index >= self.playlist.len() {
            Err(error::InvalidIndex)
        } else {
            self.index = index;
            Ok(())
        }
    }

    pub fn is_looping(&self) -> bool {
        self.cfg.loop_playlist
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.cfg.loop_playlist = looping;
    }
}

#[derive(Debug)]
pub struct AudioPlayConfig {
    /// Return to beginning of playlist once ended.
    pub loop_playlist: bool,
}

impl Default for AudioPlayConfig {
    fn default() -> Self {
        Self {
            loop_playlist: true,
        }
    }
}

type BufferProd<T> = ringbuf::CachingProd<Arc<ringbuf::HeapRb<T>>>;
pub struct AudioStreamBuilder {
    device: cpal::Device,
    stream_config: cpal::SupportedStreamConfig,
    buffer_size: usize,
}

impl AudioStreamBuilder {
    pub fn new(
        device: cpal::Device,
        stream_config: cpal::SupportedStreamConfig,
        buffer_size: usize,
    ) -> Self {
        Self {
            device,
            stream_config,
            buffer_size,
        }
    }

    /// Plays an audio file
    ///
    /// # Returns
    /// (play/pause control, stream)
    ///
    /// # Panics
    /// + If the player is not ready. (See [`Self::is_ready`].)
    pub fn load(&self, mut audio_file: AudioFile) -> Result<AudioStream, ffm::Error> {
        // NOTE: Could create buffer pool for reuse.
        let (buffer_prod, mut buffer_cons) = ringbuf::HeapRb::new(self.buffer_size).split();

        audio_file.ctx_mut().seek(0, ..0).unwrap();

        // Find the audio stream and its index
        let audio_stream = audio_file
            .ctx()
            .streams()
            .best(ffm::media::Type::Audio)
            .ok_or(ffm::Error::StreamNotFound)?;

        let audio_stream_index = audio_stream.index();

        // Create a decoder
        let ctx = ffm::codec::Context::from_parameters(audio_stream.parameters())?;
        let audio_decoder = ctx.decoder().audio()?;

        // Set up a resampler for the audio
        let resampler = ffm::software::resampling::context::Context::get(
            audio_decoder.format(),
            audio_decoder.channel_layout(),
            audio_decoder.rate(),
            self.stream_config.sample_format().as_ffmpeg_sample(),
            audio_decoder.channel_layout(),
            self.stream_config.sample_rate().0,
        )?;

        let audio_stream = match self.stream_config.sample_format() {
            cpal::SampleFormat::F32 => {
                self.device.build_output_stream(
                    &self.stream_config.clone().into(),
                    move |data: &mut [f32], cbinfo| {
                        // Copy to the audio buffer (if there aren't enough samples, write_audio will write silence)
                        write_audio(data, &mut buffer_cons, &cbinfo);
                    },
                    |err| eprintln!("error occurred on the audio output stream: {}", err),
                    None,
                )
            }
            cpal::SampleFormat::I16 => panic!("i16 output format unimplemented"),
            cpal::SampleFormat::U16 => panic!("u16 output format unimplemented"),
            _ => panic!("output format unimplemented"),
        }
        .unwrap();

        Ok(AudioStream {
            audio_file,
            audio_stream,
            stream_index: audio_stream_index,
            decoder: audio_decoder,
            resampler,
            buffer_prod,
            state: Arc::new(Mutex::new(StreamState::Pause)),
        })
    }
}

/// # Notes
/// + !Send
pub struct AudioStream {
    audio_file: AudioFile,
    audio_stream: cpal::Stream,
    stream_index: usize,
    decoder: ffm::decoder::Audio,
    resampler: ffm::software::resampling::context::Context,
    buffer_prod: BufferProd<f32>,
    state: StreamStateLock,
}

impl AudioStream {
    pub fn state(&self) -> Arc<Mutex<StreamState>> {
        self.state.clone()
    }

    pub fn load(&mut self) -> Result<(), error::AudioStream> {
        let mut receive_and_queue_audio_frames =
            |decoder: &mut ffm::decoder::Audio| -> Result<(), error::AudioStream> {
                let mut decoded = ffm::frame::Audio::empty();

                // Ask the decoder for frames
                while decoder.receive_frame(&mut decoded).is_ok() {
                    // Resample the frame's audio into another frame
                    let mut resampled = ffm::frame::Audio::empty();
                    self.resampler
                        .run(&decoded, &mut resampled)
                        .map_err(|err| error::AudioStream::Resample(err))?;

                    // DON'T just use resampled.data(0).len() -- it might not be fully populated
                    // Grab the right number of bytes based on sample count, bytes per sample, and number of channels.
                    let both_channels = packed(&resampled);

                    // Sleep until the buffer has enough space for all of the samples
                    // (the producer will happily accept a partial write, which we don't want)
                    while self.buffer_prod.vacant_len() < both_channels.len() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }

                    // Buffer the samples for playback
                    self.buffer_prod.push_slice(both_channels);
                }
                Ok(())
            };

        // Start playing
        self.audio_stream.play()?;
        for (stream, packet) in self.audio_file.ctx_mut().packets() {
            let state = self.state.lock().unwrap();
            if state.is_paused() {
                drop(state);
                self.audio_stream.pause()?;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let state = self.state.lock().unwrap();
                    if state.is_playing() {
                        self.audio_stream.play()?;
                        break;
                    } else if state.is_stopped() {
                        return Ok(());
                    }
                }
            } else if state.is_stopped() {
                return Ok(());
            }

            // Look for audio packets (ignore video and others)
            if stream.index() == self.stream_index {
                // Send the packet to the decoder; it will combine them into frames.
                // In practice though, 1 packet = 1 frame
                self.decoder
                    .send_packet(&packet)
                    .map_err(|err| error::AudioStream::Decode(err))?;

                // Queue the audio for playback (and block if the queue is full)
                receive_and_queue_audio_frames(&mut self.decoder)?;
            }
        }

        *self.state.lock().unwrap() = StreamState::Done;
        Ok(())
    }
}

#[derive(Copy, Clone, Debug)]
pub enum StreamState {
    Play,
    Pause,

    /// Stream stopped before end.
    Stop,

    /// Stream played all the way through.
    Done,
}

impl StreamState {
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Pause)
    }

    pub fn is_playing(&self) -> bool {
        matches!(self, Self::Play)
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::Stop)
    }

    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}

pub type StreamStateLock = Arc<Mutex<StreamState>>;

trait SampleFormatConversion {
    fn as_ffmpeg_sample(&self) -> ffm::format::Sample;
}

impl SampleFormatConversion for cpal::SampleFormat {
    fn as_ffmpeg_sample(&self) -> ffm::format::Sample {
        use ffm::format::sample::Type;

        match self {
            Self::I16 => ffm::format::Sample::I16(Type::Packed),
            Self::F32 => ffm::format::Sample::F32(Type::Packed),
            Self::U16 => {
                panic!("ffmpeg resampler doesn't support u16")
            }
            _ => panic!("ffmpeg resampler can not convert type"),
        }
    }
}

// Interpret the audio frame's data as packed (alternating channels, 12121212, as opposed to planar 11112222)
pub fn packed<T: ffm::frame::audio::Sample>(frame: &ffm::frame::Audio) -> &[T] {
    if !frame.is_packed() {
        panic!("data is not packed");
    }

    if !<T as ffm::frame::audio::Sample>::is_valid(frame.format(), frame.channels()) {
        panic!("unsupported type");
    }

    unsafe {
        std::slice::from_raw_parts(
            (*frame.as_ptr()).data[0] as *const T,
            frame.samples() * frame.channels() as usize,
        )
    }
}

fn write_audio<T: cpal::Sample>(
    data: &mut [T],
    samples: &mut impl ringbuf::consumer::Consumer<Item = T>,
    _: &cpal::OutputCallbackInfo,
) {
    for d in data {
        // copy as many samples as we have.
        // if we run out, write silence
        match samples.try_pop() {
            Some(sample) => *d = sample,
            None => *d = cpal::Sample::EQUILIBRIUM,
        }
    }
}

pub mod error {
    use ffmpeg_next as ffm;

    #[derive(Debug)]
    pub struct InvalidIndex;

    #[derive(Debug)]
    pub enum AudioStream {
        Resample(ffm::Error),
        Decode(ffm::util::error::Error),
        DeviceNotAvailable,
        Other(String),
    }

    impl From<cpal::PlayStreamError> for AudioStream {
        fn from(value: cpal::PlayStreamError) -> Self {
            match value {
                cpal::PlayStreamError::DeviceNotAvailable => Self::DeviceNotAvailable,
                cpal::PlayStreamError::BackendSpecific { err } => Self::Other(err.description),
            }
        }
    }

    impl From<cpal::PauseStreamError> for AudioStream {
        fn from(value: cpal::PauseStreamError) -> Self {
            match value {
                cpal::PauseStreamError::DeviceNotAvailable => Self::DeviceNotAvailable,
                cpal::PauseStreamError::BackendSpecific { err } => Self::Other(err.description),
            }
        }
    }
}
