use super::{
    Command, CMD_KEY_NEXT, CMD_KEY_PREVIOUS, CMD_KEY_QUIT, CMD_KEY_TOGGLE_LOOP, CMD_KEY_TOGGLE_PLAY,
};
use crossbeam::channel;
use std::io;

pub struct InputActor {
    command_tx: channel::Sender<Command>,
}

impl InputActor {
    pub fn new(command_tx: channel::Sender<Command>) -> Self {
        Self { command_tx }
    }
    pub fn run(&mut self) {
        let mut input = String::new();
        loop {
            input.clear();
            tracing::trace!("waiting for input");
            if let Err(err) = io::stdin().read_line(&mut input) {
                tracing::error!(?err);
                continue;
            }

            if let Some(cmd) = command_from_str(input.trim()) {
                tracing::debug!(?cmd);
                if self.command_tx.send(cmd).is_err() {
                    tracing::error!("command channel closed");
                    break;
                }
            }
        }
        tracing::debug!("closing input actor");
    }
}

fn command_from_str(input: impl AsRef<str>) -> Option<Command> {
    match input.as_ref() {
        CMD_KEY_QUIT => Some(Command::Quit),
        CMD_KEY_PREVIOUS => Some(Command::Previous),
        CMD_KEY_NEXT => Some(Command::Next),
        CMD_KEY_TOGGLE_PLAY => Some(Command::TogglePlay),
        CMD_KEY_TOGGLE_LOOP => Some(Command::ToggleLoop),
        _ => None,
    }
}
