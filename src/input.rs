use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

pub enum Event {
    Key(KeyEvent),
    #[allow(dead_code)]
    Mouse(MouseEvent),
    Tick,
}

pub struct Input {
    rx: mpsc::Receiver<Event>,
    _input_handle: thread::JoinHandle<()>,
    _tick_handle: thread::JoinHandle<()>,
}

impl Input {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        
        let _input_handle = {
            let tx = tx.clone();
            thread::spawn(move || loop {
                if let Ok(true) = event::poll(Duration::from_millis(100)) {
                    if let Ok(event) = event::read() {
                        match event {
                            CrosstermEvent::Key(key) => {
                                if tx.send(Event::Key(key)).is_err() {
                                    return;
                                }
                            }
                            CrosstermEvent::Mouse(mouse) => {
                                if tx.send(Event::Mouse(mouse)).is_err() {
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            })
        };
        
        let _tick_handle = {
            thread::spawn(move || loop {
                if tx.send(Event::Tick).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            })
        };
        
        Self {
            rx,
            _input_handle,
            _tick_handle,
        }
    }
    
    pub fn next(&self) -> Result<Event, mpsc::RecvError> {
        self.rx.recv()
    }
}
