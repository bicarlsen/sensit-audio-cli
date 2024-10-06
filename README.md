# Sensit audio play CLI

A simple audio player for [Sensit](https://sensit.tech) technical interview.

## Use
Accepts a single argument which is the path to the folder to pull the audio files from.
The directory is traversed recursively, creating a playlist from any file
that can be interpreted as audio.
```sh
cargo run -- <path/to/dir>
```

### Commands
`q`: quit  
`p`: play/pause  
`k`: play next track  
`j`: play previous track  
`r`: restart  
`l`: toggle looping  
`a`: toggle autoplay  
`s`: toggle show state  

## Discussion 

### Design decisions
#### Why `actors` instead of `async`?

Only two main tasks of listening for user input and playing audio,
so `async` seemed like overkill.

#### Modelling state
There are three major pieces of state to be modeled.
1. Playlist
2. Position in the playlist
3. Play state

#### Changing play state
To change the play state of a song an `Arc<Mutex<State>>` is modified.
This is checked periodically by the stream.
Allows the stream's state to be modified from outside the play loop.
Could be made more performant using events, but doesn't seem to impact playback. 

### Other things
+ Preload and cache recent audio to reduce time between plays.
+ Could use a buffer pool to reuse memory.
+ For _no enter_ mode, need to direct output manually.
+ Audio streams are `!Send` so loading first stream to player needed to be done after creation.
This results in the field being an `Option` when it is only `None` before the first load.

## TODOs
+ Test on individual large files. Does it take a long time to play first sound?
+ Test on large folders. Does it take long to load?
+ Restart audio track via seek, rather than unload/load.
+ Accept input directly from keyboard, rather than via stdin.

## References
https://github.com/dceddia/ffmpeg-cpal-play-audio  
https://www.bekk.christmas/post/2023/19/make-some-noise-with-rust  

