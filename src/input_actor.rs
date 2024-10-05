use super::{Command, CMD_KEY_NEXT, CMD_KEY_PREVIOUS, CMD_KEY_TOGGLE_PLAY};
use crossbeam::channel;
use std::io;

pub struct InputActor {
    command_tx: channel::Sender<Command>,
}

impl InputActor {
    pub fn new(command_tx: channel::Sender<Command>) -> Self {
        Self { command_tx }
    }

    pub fn run(&self) {
        let mut input = String::new();
        loop {
            input.clear();
            tracing::info!("waiting for input");

            // TODO: Don't wait for new line.
            io::stdin().read_line(&mut input).expect("invalid input");
            let cmd = input.trim();
            let Some(cmd) = command_from_str(cmd) else {
                continue;
            };

            if self.command_tx.send(cmd).is_err() {
                break;
            }
        }

        tracing::debug!("input actor closing");
    }
}

fn command_from_str(input: impl AsRef<str>) -> Option<Command> {
    match input.as_ref() {
        CMD_KEY_PREVIOUS => Some(Command::Previous),
        CMD_KEY_NEXT => Some(Command::Next),
        CMD_KEY_TOGGLE_PLAY => Some(Command::TogglePlay),
        _ => None,
    }
}
