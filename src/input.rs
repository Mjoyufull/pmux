use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

pub enum Event {
    Key(KeyEvent),
    #[allow(dead_code)]
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
}

pub struct Input {
    rx: mpsc::Receiver<Event>,
    _tx: mpsc::Sender<Event>, // Keep sender to control shutdown (dropped on shutdown)
    input_handle: Option<thread::JoinHandle<()>>,
    tick_handle: Option<thread::JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Input {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        
        let tx_input = tx.clone();
        let running_input = running.clone();
        
        let _input_handle = thread::spawn(move || loop {
            // Check if we should stop
            if !running_input.load(Ordering::Relaxed) {
                break;
            }

            if let Ok(true) = event::poll(Duration::from_millis(100)) {
                // CRITICAL: Check if we stopped while polling BEFORE reading
                // This prevents the thread from "eating" input intended for the next process
                if !running_input.load(Ordering::Relaxed) {
                    break;
                }

                // Try to read event - if it fails (e.g., raw mode disabled), exit
                match event::read() {
                    Ok(event) => {
                        match event {
                            CrosstermEvent::Key(key) => {
                                if tx_input.send(Event::Key(key)).is_err() {
                                    return; // Channel closed, exit
                                }
                            }
                            CrosstermEvent::Mouse(mouse) => {
                                if tx_input.send(Event::Mouse(mouse)).is_err() {
                                    return; // Channel closed, exit
                                }
                            }
                            CrosstermEvent::Resize(width, height) => {
                                if tx_input.send(Event::Resize(width, height)).is_err() {
                                    return; // Channel closed, exit
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(_) => {
                        // Raw mode disabled or terminal error - exit thread
                        return;
                    }
                }
            }
        });
        
        let tx_tick = tx.clone();
        let running_tick = running.clone();
        
        let _tick_handle = thread::spawn(move || loop {
            if !running_tick.load(Ordering::Relaxed) {
                break;
            }
            if tx_tick.send(Event::Tick).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(250));
        });
        
        Self {
            rx,
            _tx: tx,
            input_handle: Some(_input_handle),
            tick_handle: Some(_tick_handle),
            running,
        }
    }
    
    pub fn next(&self) -> Result<Event, mpsc::RecvError> {
        self.rx.recv()
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // Signal threads to stop
        self.running.store(false, Ordering::Relaxed);
        
        // Drop the sender to unblock any receivers
        // _tx will be dropped automatically when self is dropped
        
        // CRITICAL: Wait for input thread to finish!
        // This ensures it doesn't race with the next process for stdin
        if let Some(handle) = self.input_handle.take() {
            let _ = handle.join();
        }
        
        if let Some(handle) = self.tick_handle.take() {
            let _ = handle.join();
        }
    }
}
