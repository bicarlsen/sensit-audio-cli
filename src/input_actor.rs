use super::{Command, CMD_KEY_NEXT, CMD_KEY_PREVIOUS, CMD_KEY_QUIT, CMD_KEY_TOGGLE_PLAY};
use crossbeam::channel;
use device_query::DeviceEvents;
use std::{
    io,
    sync::{Arc, Mutex},
};

enum State {
    Active,
    Closed,
}

pub struct InputActor {
    command_tx: channel::Sender<Command>,
    state: Arc<Mutex<State>>,
    device_state: device_query::DeviceState,
}

impl InputActor {
    pub fn new(command_tx: channel::Sender<Command>) -> Self {
        Self {
            command_tx,
            state: Arc::new(Mutex::new(State::Active)),
            device_state: device_query::DeviceState::new(),
        }
    }
    pub fn run(&mut self) {
        //crossterm::terminal::enable_raw_mode().expect("could not setup terminal");
        crossterm::execute!(io::stdout(), crossterm::event::EnableFocusChange)
            .expect("could not setup terminal");

        //crossterm::terminal::disable_raw_mode().expect("could not clean up terminal");
        let mut cb_keydown = Some(self.register_key_down_callback());
        loop {
            if is_event_available() {
                tracing::debug!("a");
                let event = match crossterm::event::read() {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::error!(?err);
                        continue;
                    }
                };

                match event {
                    crossterm::event::Event::FocusGained => {
                        if cb_keydown.is_none() {
                            let _ = cb_keydown.insert(self.register_key_down_callback());
                        }
                    }
                    crossterm::event::Event::FocusLost => {
                        let _ = cb_keydown.take();
                    }
                    _ => {}
                }
            } else {
                if matches!(*self.state.lock().unwrap(), State::Closed) {
                    break;
                }
            }
        }

        tracing::debug!("input actor closing");
        crossterm::execute!(io::stdout(), crossterm::event::DisableFocusChange,)
            .expect("could not setup terminal");
    }

    fn register_key_down_callback(
        &mut self,
    ) -> device_query::CallbackGuard<impl Fn(&device_query::Keycode)> {
        let cb_guard = self.device_state.on_key_down({
            let command_tx = self.command_tx.clone();
            let state = self.state.clone();
            move |key| {
                if let Some(cmd) = command_from_code(key) {
                    if command_tx.send(cmd).is_err() {
                        tracing::error!("command channel closed");
                        *state.lock().unwrap() = State::Closed;
                    }
                }
            }
        });

        cb_guard
    }
}
fn command_from_code(code: &device_query::Keycode) -> Option<Command> {
    match *code {
        CMD_KEY_QUIT => Some(Command::Quit),
        CMD_KEY_PREVIOUS => Some(Command::Previous),
        CMD_KEY_NEXT => Some(Command::Next),
        CMD_KEY_TOGGLE_PLAY => Some(Command::TogglePlay),
        _ => None,
    }
}

fn is_event_available() -> bool {
    match crossterm::event::poll(std::time::Duration::from_millis(500)) {
        Ok(available) => available,
        Err(err) => {
            tracing::error!(?err);
            false
        }
    }
}
