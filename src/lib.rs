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
    pub fn new(path: PathBuf, ctx: ffm::format::context::Input) -> Self {
        Self { path, ctx }
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
pub struct Playlist(Vec<AudioFile>);
impl Playlist {
    pub fn new(files: Vec<AudioFile>) -> Self {
        Self(files)
    }
}

#[derive(Debug)]
pub struct Jukebox {
    playlist: Playlist,
    current_track: usize,
    cfg: AudioPlayConfig,
}

impl Jukebox {
    pub fn new(playlist: Playlist) -> Self {
        Self {
            playlist,
            current_track: 0,
            cfg: AudioPlayConfig::default(),
        }
    }

    pub fn next(&mut self) -> Option<&mut AudioFile> {
        self.current_track += 1;
        if self.cfg.loop_playlist {
            if self.current_track >= self.playlist.len() {
                self.current_track = 0;
            }

            Some(&mut self.playlist[self.current_track])
        } else {
            self.playlist.get_mut(self.current_track)
        }
    }

    pub fn next_back(&mut self) -> Option<&mut AudioFile> {
        if self.cfg.loop_playlist {
            if self.current_track == 0 {
                self.current_track = self.playlist.len();
            }
            self.current_track -= 1;

            Some(&mut self.playlist[self.current_track])
        } else {
            if self.current_track == 0 {
                None
            } else {
                Some(&mut self.playlist[self.current_track])
            }
        }
    }

    pub fn to_beginning(&mut self) {
        self.current_track = 0;
    }
}

#[derive(Debug)]
pub struct AudioPlayConfig {
    /// Automatically play next song.
    pub autoplay: bool,

    /// Return to beginning of playlist once ended.
    pub loop_playlist: bool,
}

impl Default for AudioPlayConfig {
    fn default() -> Self {
        Self {
            autoplay: true,
            loop_playlist: true,
        }
    }
}

type BufferProd<T> = ringbuf::CachingProd<Arc<ringbuf::HeapRb<T>>>;
type BufferCons<T> = ringbuf::CachingCons<Arc<ringbuf::HeapRb<T>>>;
pub struct AudioFilePlayer {
    device: cpal::Device,
    stream_config: cpal::SupportedStreamConfig,
    buffer_prod: BufferProd<f32>,

    // TODO: Need better data structure here.
    // Data callback where this is consumed is called rapidly and often
    // leading to the mutex being un/locked rapidly and often.
    // This could allow for multiple processes to interleave and harm performance.
    // Another option would be to create a new audio buffer for each call of `play`,
    // but seems wasteful of memory.
    buffer_cons: Arc<Mutex<BufferCons<f32>>>,
}
impl AudioFilePlayer {
    pub fn new(
        device: cpal::Device,
        stream_config: cpal::SupportedStreamConfig,
        buffer_size: usize,
    ) -> Self {
        let (buffer_prod, buffer_cons) = ringbuf::HeapRb::new(buffer_size).split();

        Self {
            device,
            stream_config,
            buffer_prod,
            buffer_cons: Arc::new(Mutex::new(buffer_cons)),
        }
    }

    /// Plays an audio file
    ///
    /// # Panics
    /// + If the player is not ready. (See [`Self::is_ready`].)
    pub fn play(&mut self, audio: &mut AudioFile) -> Result<(), ffm::Error> {
        audio.ctx.seek(0, ..0).unwrap();

        // Find the audio stream and its index
        let audio_stream = audio
            .ctx()
            .streams()
            .best(ffm::media::Type::Audio)
            .ok_or(ffm::Error::StreamNotFound)?;

        let audio_stream_index = audio_stream.index();

        // Create a decoder
        let ctx = ffm::codec::Context::from_parameters(audio_stream.parameters())?;
        let mut audio_decoder = ctx.decoder().audio()?;

        // Set up a resampler for the audio
        let mut resampler = ffm::software::resampling::context::Context::get(
            audio_decoder.format(),
            audio_decoder.channel_layout(),
            audio_decoder.rate(),
            self.stream_config.sample_format().as_ffmpeg_sample(),
            audio_decoder.channel_layout(),
            self.stream_config.sample_rate().0,
        )?;

        let audio_stream = match self.stream_config.sample_format() {
            cpal::SampleFormat::F32 => {
                let buffer_cons = self.buffer_cons.clone();
                self.device.build_output_stream(
                    &self.stream_config.clone().into(),
                    move |data: &mut [f32], cbinfo| {
                        // Copy to the audio buffer (if there aren't enough samples, write_audio will write silence)
                        write_audio(data, &mut *buffer_cons.lock().unwrap(), &cbinfo);
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

        let mut receive_and_queue_audio_frames =
            |decoder: &mut ffm::decoder::Audio| -> Result<(), ffm::Error> {
                let mut decoded = ffm::frame::Audio::empty();

                // Ask the decoder for frames
                while decoder.receive_frame(&mut decoded).is_ok() {
                    // Resample the frame's audio into another frame
                    let mut resampled = ffm::frame::Audio::empty();
                    resampler.run(&decoded, &mut resampled)?;

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
        audio_stream.play().unwrap();

        for (stream, packet) in audio.ctx_mut().packets() {
            // Look for audio packets (ignore video and others)
            if stream.index() == audio_stream_index {
                // Send the packet to the decoder; it will combine them into frames.
                // In practice though, 1 packet = 1 frame
                audio_decoder.send_packet(&packet)?;

                // Queue the audio for playback (and block if the queue is full)
                receive_and_queue_audio_frames(&mut audio_decoder)?;
            }
        }

        Ok(())
    }
}

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

pub mod error {}
