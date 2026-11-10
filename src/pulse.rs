//! The sound-server side: one PulseAudio client connection, driven by the
//! standard (non-threaded) libpulse main loop on its own thread.
//!
//! The thread subscribes to playback (sink input) and recording (source
//! output) events, re-reads the stream lists after each batch of events,
//! and reports which kinds are active. When the server is unreachable or
//! goes away it reports that, waits, and connects again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::introspect::{SinkInputInfo, SourceOutputInfo};
use pulse::context::subscribe::{Facility, InterestMaskSet};
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::proplist::{properties, Proplist};

use crate::Event;

/// The name this program gives itself as a PulseAudio client.
const CLIENT_NAME: &str = "sway-audio-idle-inhibit";
/// How long to wait between connection attempts.
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// Which stream lists are read.
#[derive(Clone, Copy, Debug)]
pub struct Watch {
    pub playback: bool,
    pub recording: bool,
}

/// Which kinds of streams are active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub playback: bool,
    pub recording: bool,
}

/// What the sound server thread reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// The current activity, read after the connection came up or after
    /// stream events.
    Streams(Activity),
    /// The server is unreachable; nothing counts as active.
    Unreachable,
}

/// The rules for one recording stream.
#[derive(Debug)]
pub struct RecordingFacts<'a> {
    pub corked: bool,
    pub owned_by_client: bool,
    pub application_name: Option<&'a str>,
    pub resample_method: Option<&'a str>,
    pub stream_monitor: Option<&'a str>,
}

/// Whether a playback stream counts: it is running and belongs to a client
/// (streams owned by modules, such as loopbacks, do not count).
pub fn playback_counts(corked: bool, owned_by_client: bool) -> bool {
    !corked && owned_by_client
}

/// Whether a recording stream counts: it is running, belongs to a client,
/// is not on the ignore list (exact application name), and is not a level
/// meter (peak detection, or a stream that declares itself a monitor).
pub fn recording_counts(facts: &RecordingFacts<'_>, ignored: &[String]) -> bool {
    if facts.corked || !facts.owned_by_client {
        return false;
    }
    if let Some(name) = facts.application_name {
        if ignored.iter().any(|ignored| ignored == name) {
            return false;
        }
    }
    let level_meter =
        facts.resample_method == Some("peaks") || facts.stream_monitor == Some("true");
    !level_meter
}

fn sink_input_counts(info: &SinkInputInfo<'_>) -> bool {
    playback_counts(info.corked, info.client.is_some())
}

fn source_output_counts(info: &SourceOutputInfo<'_>, ignored: &[String]) -> bool {
    let application_name = info.proplist.get_str(properties::APPLICATION_NAME);
    let stream_monitor = info.proplist.get_str("stream.monitor");
    recording_counts(
        &RecordingFacts {
            corked: info.corked,
            owned_by_client: info.client.is_some(),
            application_name: application_name.as_deref(),
            resample_method: info.resample_method.as_deref(),
            stream_monitor: stream_monitor.as_deref(),
        },
        ignored,
    )
}

/// Starts the sound server thread. It runs for the life of the process.
pub fn spawn(watch: Watch, ignored: Vec<String>, events: Sender<Event>) {
    thread::Builder::new()
        .name("pulse".into())
        .spawn(move || run(watch, &ignored, &events))
        .expect("cannot start the sound server thread");
}

fn run(watch: Watch, ignored: &[String], events: &Sender<Event>) {
    loop {
        let mut was_ready = false;
        if connection(watch, ignored, events, &mut was_ready) == Outcome::Stop {
            return;
        }
        if was_ready && events.send(Event::Pulse(Report::Unreachable)).is_err() {
            return;
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The connection failed or was lost; try again.
    Retry,
    /// The receiving side is gone; the process is exiting.
    Stop,
}

/// Shared state between the libpulse callbacks and the loop.
#[derive(Default)]
struct Shared {
    /// Stream events arrived since the last read started.
    dirty: Cell<bool>,
    /// Stream list reads still running.
    pending: Cell<u32>,
    /// A full read finished and has not been reported yet.
    finished: Cell<bool>,
    /// The activity collected by the current read.
    activity: RefCell<Activity>,
}

impl Shared {
    fn list_done(&self) {
        let left = self.pending.get().saturating_sub(1);
        self.pending.set(left);
        if left == 0 {
            self.finished.set(true);
        }
    }
}

/// One connection, from connecting to losing it.
fn connection(
    watch: Watch,
    ignored: &[String],
    events: &Sender<Event>,
    was_ready: &mut bool,
) -> Outcome {
    let Some(mut mainloop) = Mainloop::new() else {
        return Outcome::Retry;
    };
    let Some(mut proplist) = Proplist::new() else {
        return Outcome::Retry;
    };
    // Best effort: the server falls back to the name given below.
    let _ = proplist.set_str(properties::APPLICATION_NAME, CLIENT_NAME);
    let Some(mut context) = Context::new_with_proplist(&mainloop, CLIENT_NAME, &proplist) else {
        return Outcome::Retry;
    };
    if context.connect(None, FlagSet::NOAUTOSPAWN, None).is_err() {
        return Outcome::Retry;
    }

    let outcome = drive(
        &mut mainloop,
        &mut context,
        watch,
        ignored,
        events,
        was_ready,
    );
    context.set_subscribe_callback(None);
    context.disconnect();
    outcome
}

fn iterate(mainloop: &mut Mainloop) -> bool {
    matches!(mainloop.iterate(true), IterateResult::Success(_))
}

fn drive(
    mainloop: &mut Mainloop,
    context: &mut Context,
    watch: Watch,
    ignored: &[String],
    events: &Sender<Event>,
    was_ready: &mut bool,
) -> Outcome {
    loop {
        match context.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => return Outcome::Retry,
            _ => {}
        }
        if !iterate(mainloop) {
            return Outcome::Retry;
        }
    }
    *was_ready = true;

    let shared = Rc::new(Shared::default());
    // Read the lists once right away, then after every batch of events.
    shared.dirty.set(true);
    {
        let shared = Rc::clone(&shared);
        context.set_subscribe_callback(Some(Box::new(move |facility, _, _| {
            if matches!(
                facility,
                Some(Facility::SinkInput) | Some(Facility::SourceOutput)
            ) {
                shared.dirty.set(true);
            }
        })));
    }
    let mut mask = InterestMaskSet::NULL;
    if watch.playback {
        mask |= InterestMaskSet::SINK_INPUT;
    }
    if watch.recording {
        mask |= InterestMaskSet::SOURCE_OUTPUT;
    }
    context.subscribe(mask, |_| {});

    let ignored: Rc<[String]> = ignored.into();
    loop {
        if shared.dirty.get() && shared.pending.get() == 0 {
            shared.dirty.set(false);
            *shared.activity.borrow_mut() = Activity::default();
            start_read(context, watch, &shared, &ignored);
        }
        if !iterate(mainloop) {
            return Outcome::Retry;
        }
        if !matches!(context.get_state(), State::Ready) {
            return Outcome::Retry;
        }
        if shared.finished.replace(false) {
            let activity = *shared.activity.borrow();
            if events
                .send(Event::Pulse(Report::Streams(activity)))
                .is_err()
            {
                return Outcome::Stop;
            }
        }
    }
}

fn start_read(context: &Context, watch: Watch, shared: &Rc<Shared>, ignored: &Rc<[String]>) {
    let introspect = context.introspect();
    let mut lists = 0;
    if watch.playback {
        lists += 1;
    }
    if watch.recording {
        lists += 1;
    }
    shared.pending.set(lists);
    if lists == 0 {
        shared.finished.set(true);
        return;
    }
    if watch.playback {
        let shared = Rc::clone(shared);
        introspect.get_sink_input_info_list(move |result| match result {
            ListResult::Item(info) => {
                if sink_input_counts(info) {
                    shared.activity.borrow_mut().playback = true;
                }
            }
            ListResult::End | ListResult::Error => shared.list_done(),
        });
    }
    if watch.recording {
        let shared = Rc::clone(shared);
        let ignored = Rc::clone(ignored);
        introspect.get_source_output_info_list(move |result| match result {
            ListResult::Item(info) => {
                if source_output_counts(info, &ignored) {
                    shared.activity.borrow_mut().recording = true;
                }
            }
            ListResult::End | ListResult::Error => shared.list_done(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>() -> RecordingFacts<'a> {
        RecordingFacts {
            corked: false,
            owned_by_client: true,
            application_name: None,
            resample_method: None,
            stream_monitor: None,
        }
    }

    #[test]
    fn playback_needs_running_client_stream() {
        assert!(playback_counts(false, true));
        assert!(!playback_counts(true, true));
        assert!(!playback_counts(false, false));
    }

    #[test]
    fn recording_rules() {
        let none: Vec<String> = Vec::new();
        assert!(recording_counts(&facts(), &none));
        assert!(!recording_counts(
            &RecordingFacts {
                corked: true,
                ..facts()
            },
            &none
        ));
        assert!(!recording_counts(
            &RecordingFacts {
                owned_by_client: false,
                ..facts()
            },
            &none
        ));
    }

    #[test]
    fn ignore_list_matches_exact_application_names() {
        let ignored = vec!["alpha".to_owned(), "beta".to_owned()];
        for (name, counts) in [
            (Some("alpha"), false),
            (Some("beta"), false),
            (Some("Alpha"), true),
            (Some("alph"), true),
            (Some("alpha beta"), true),
            (None, true),
        ] {
            let facts = RecordingFacts {
                application_name: name,
                ..facts()
            };
            assert_eq!(recording_counts(&facts, &ignored), counts, "{name:?}");
        }
    }

    #[test]
    fn level_meters_never_count() {
        let none: Vec<String> = Vec::new();
        let peaks = RecordingFacts {
            resample_method: Some("peaks"),
            ..facts()
        };
        assert!(!recording_counts(&peaks, &none));
        let monitor = RecordingFacts {
            stream_monitor: Some("true"),
            ..facts()
        };
        assert!(!recording_counts(&monitor, &none));
        let not_monitor = RecordingFacts {
            stream_monitor: Some("false"),
            resample_method: Some("speex-float-1"),
            ..facts()
        };
        assert!(recording_counts(&not_monitor, &none));
    }
}
