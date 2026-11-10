//! sway-audio-idle-inhibit: keeps the session from going idle while audio
//! plays or records, by holding a gnome-session idle inhibitor, or prints
//! whether audio is active (the dry-print modes).

#![forbid(unsafe_code)]

mod cli;
mod idle;
mod pulse;
mod session;

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use cli::{Command, Kinds, Mode, Options};
use idle::Idle;
use pulse::{Activity, Report, Watch};
use session::BusEvent;

/// How long a signal may wait for a clean shutdown before the process
/// exits anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1200);

/// Everything the main loop reacts to.
pub enum Event {
    Pulse(Report),
    Bus(BusEvent),
    Quit,
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let program = args
        .first()
        .and_then(|arg0| Path::new(arg0).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sway-audio-idle-inhibit".to_owned());

    match cli::parse(args.get(1..).unwrap_or_default()) {
        Command::Help => {
            emit_raw(&cli::help(&program));
        }
        Command::Usage(message) => {
            eprintln!("{program}: {message}");
            eprintln!("Try '{program} --help' for more information.");
            process::exit(2);
        }
        Command::Run(options) => run(options),
    }
}

fn run(options: Options) {
    let (events, receiver) = mpsc::channel();
    watch_signals(events.clone());

    let watch = match options.mode {
        Mode::Idle => Watch {
            playback: options.kinds != Kinds::SourceOnly,
            recording: options.kinds != Kinds::SinkOnly,
        },
        Mode::DryBoth | Mode::DryBothWaybar => Watch {
            playback: true,
            recording: true,
        },
        Mode::DrySink => Watch {
            playback: true,
            recording: false,
        },
        Mode::DrySource => Watch {
            playback: false,
            recording: true,
        },
    };

    if options.mode == Mode::Idle {
        // The dry-print modes never touch D-Bus.
        session::start(events.clone());
    }
    pulse::spawn(watch, options.ignored_source_outputs.clone(), events);

    match options.mode {
        Mode::Idle => idle_loop(&options, &receiver),
        mode => dry_print_loop(mode, &receiver),
    }
}

/// Signals end the process within the grace period: the main loop gets a
/// chance to release its inhibitor, and the process exits regardless.
fn watch_signals(events: Sender<Event>) {
    let mut signals = match Signals::new([SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("cannot install signal handlers: {error}");
            return;
        }
    };
    thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = events.send(Event::Quit);
                thread::sleep(SHUTDOWN_GRACE);
                process::exit(0);
            }
        })
        .expect("cannot start the signal thread");
}

/// Writes text to stdout right away; a closed stdout ends the program.
fn emit_raw(text: &str) {
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        process::exit(1);
    }
}

fn emit(line: &str) {
    emit_raw(&format!("{line}\n"));
}

fn running(active: bool) -> String {
    if active { "RUNNING" } else { "NOT RUNNING" }.to_owned()
}

/// The waybar line: which of output (playback) and input (recording) are
/// active, as `alt` and `class`.
fn waybar(activity: Activity) -> String {
    let text = match (activity.playback, activity.recording) {
        (true, true) => "output-input",
        (true, false) => "output",
        (false, true) => "input",
        (false, false) => "none",
    };
    format!("{{\"text\": \"\", \"alt\": \"{text}\", \"tooltip\": \"\", \"class\": \"{text}\"}}")
}

/// The dry-print line for an activity, and whether it shows activity.
fn dry_print_line(mode: Mode, activity: Activity) -> (String, bool) {
    let both = activity.playback || activity.recording;
    match mode {
        Mode::DryBothWaybar => (waybar(activity), both),
        Mode::DrySink => (running(activity.playback), activity.playback),
        Mode::DrySource => (running(activity.recording), activity.recording),
        Mode::DryBoth | Mode::Idle => (running(both), both),
    }
}

fn dry_print_loop(mode: Mode, receiver: &Receiver<Event>) {
    let mut last_active = false;
    for event in receiver {
        match event {
            Event::Pulse(Report::Streams(activity)) => {
                let (line, active) = dry_print_line(mode, activity);
                emit(&line);
                last_active = active;
            }
            Event::Pulse(Report::Unreachable) => {
                if last_active {
                    emit(&dry_print_line(mode, Activity::default()).0);
                    last_active = false;
                }
            }
            Event::Quit => process::exit(0),
            Event::Bus(_) => {}
        }
    }
}

fn counted(kinds: Kinds, activity: Activity) -> bool {
    match kinds {
        Kinds::Both => activity.playback || activity.recording,
        Kinds::SinkOnly => activity.playback,
        Kinds::SourceOnly => activity.recording,
    }
}

fn idle_loop(options: &Options, receiver: &Receiver<Event>) {
    let mut idle = Idle::new(options.delay);
    loop {
        let event = match idle.deadline() {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(wait) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match receiver.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            },
        };
        let now = Instant::now();
        let line = match event {
            None => idle.tick(now),
            Some(Event::Pulse(Report::Streams(activity))) => {
                Some(idle.activity(counted(options.kinds, activity), now))
            }
            Some(Event::Pulse(Report::Unreachable)) => Some(idle.activity(false, now)),
            Some(Event::Bus(BusEvent::Up { bus, bus_owner })) => idle.bus_up(bus, bus_owner, now),
            Some(Event::Bus(BusEvent::Owner(bus_owner))) => idle.owner_changed(bus_owner, now),
            Some(Event::Bus(BusEvent::Down)) => idle.bus_down(),
            Some(Event::Quit) => {
                idle.shutdown();
                process::exit(0);
            }
        };
        if let Some(line) = line {
            emit(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(playback: bool, recording: bool) -> Activity {
        Activity {
            playback,
            recording,
        }
    }

    #[test]
    fn waybar_lines() {
        assert_eq!(
            waybar(activity(false, false)),
            r#"{"text": "", "alt": "none", "tooltip": "", "class": "none"}"#
        );
        assert_eq!(
            waybar(activity(true, false)),
            r#"{"text": "", "alt": "output", "tooltip": "", "class": "output"}"#
        );
        assert_eq!(
            waybar(activity(false, true)),
            r#"{"text": "", "alt": "input", "tooltip": "", "class": "input"}"#
        );
        assert_eq!(
            waybar(activity(true, true)),
            r#"{"text": "", "alt": "output-input", "tooltip": "", "class": "output-input"}"#
        );
    }

    #[test]
    fn dry_print_modes_pick_their_kind() {
        let recording = activity(false, true);
        assert_eq!(dry_print_line(Mode::DryBoth, recording).0, "RUNNING");
        assert_eq!(dry_print_line(Mode::DrySink, recording).0, "NOT RUNNING");
        assert_eq!(dry_print_line(Mode::DrySource, recording).0, "RUNNING");
        let playback = activity(true, false);
        assert_eq!(dry_print_line(Mode::DrySource, playback).0, "NOT RUNNING");
    }

    #[test]
    fn idle_kinds() {
        assert!(counted(Kinds::Both, activity(false, true)));
        assert!(!counted(Kinds::SinkOnly, activity(false, true)));
        assert!(!counted(Kinds::SourceOnly, activity(true, false)));
        assert!(counted(Kinds::SourceOnly, activity(false, true)));
    }
}
