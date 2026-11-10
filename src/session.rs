//! The session-manager side: gnome-session's `org.gnome.SessionManager`
//! on the session bus.
//!
//! A watcher thread keeps a session-bus connection, follows the owner of
//! the session-manager name, and reports it. Calls go to the owner's unique
//! name, so an `Uninhibit` can never reach an owner that did not hand out
//! the cookie.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use zbus::blocking::fdo::{DBusProxy, NameOwnerChangedIterator};
use zbus::blocking::Connection;
use zbus::names::BusName;

use crate::idle::SessionBus;
use crate::Event;

/// The session manager's well-known name, object and interface.
pub const NAME: &str = "org.gnome.SessionManager";
const PATH: &str = "/org/gnome/SessionManager";
const INTERFACE: &str = "org.gnome.SessionManager";

/// The arguments of the `Inhibit` call.
const APP_ID: &str = "sway-audio-idle-inhibit";
const TOPLEVEL_XID: u32 = 0;
const REASON: &str = "Audio is playing";
/// `GSM_INHIBITOR_FLAG_IDLE`.
const FLAGS_IDLE: u32 = 8;

/// How long a method call may take before it counts as failed.
const CALL_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long to wait before connecting to the session bus again.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// What the watcher reports.
pub enum BusEvent {
    /// Connected to the session bus; the session manager's current owner.
    Up {
        bus: ZbusSession,
        bus_owner: Option<String>,
    },
    /// The session manager's owner changed.
    Owner(Option<String>),
    /// The session-bus connection is gone.
    Down,
}

/// Calls into the session manager over a zbus connection.
pub struct ZbusSession {
    connection: Connection,
}

impl SessionBus for ZbusSession {
    fn inhibit(&self, bus_owner: &str) -> Result<u32, String> {
        let reply = self
            .connection
            .call_method(
                Some(bus_owner),
                PATH,
                Some(INTERFACE),
                "Inhibit",
                &(APP_ID, TOPLEVEL_XID, REASON, FLAGS_IDLE),
            )
            .map_err(|error| error.to_string())?;
        reply
            .body()
            .deserialize::<u32>()
            .map_err(|error| error.to_string())
    }

    fn uninhibit(&self, bus_owner: &str, cookie: u32) -> Result<(), String> {
        self.connection
            .call_method(
                Some(bus_owner),
                PATH,
                Some(INTERFACE),
                "Uninhibit",
                &(cookie,),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

struct Watch {
    connection: Connection,
    bus_owner: Option<String>,
    changes: NameOwnerChangedIterator,
}

fn connect() -> zbus::Result<Watch> {
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()?;
    let proxy = DBusProxy::new(&connection)?;
    // Subscribe before asking for the owner, so no change is missed.
    let changes = proxy.receive_name_owner_changed_with_args(&[(0, NAME)])?;
    let bus_owner = proxy
        .get_name_owner(BusName::try_from(NAME)?)
        .ok()
        .map(|name| name.to_string());
    Ok(Watch {
        connection,
        bus_owner,
        changes,
    })
}

fn up(watch: &Watch) -> Event {
    Event::Bus(BusEvent::Up {
        bus: ZbusSession {
            connection: watch.connection.clone(),
        },
        bus_owner: watch.bus_owner.clone(),
    })
}

/// Connects once right away, reporting the result before returning, and
/// then keeps watching (and reconnecting) on a thread of its own.
pub fn start(events: Sender<Event>) {
    let first = connect().ok();
    if let Some(watch) = &first {
        // The receiver lives in this process; it cannot be gone yet.
        let _ = events.send(up(watch));
    }
    thread::Builder::new()
        .name("session-bus".into())
        .spawn(move || run(first, &events))
        .expect("cannot start the session bus thread");
}

fn run(mut current: Option<Watch>, events: &Sender<Event>) {
    loop {
        if let Some(watch) = current.take() {
            for signal in watch.changes {
                let Ok(args) = signal.args() else {
                    continue;
                };
                if args.name().as_str() != NAME {
                    continue;
                }
                let bus_owner = args.new_owner().as_ref().map(|name| name.to_string());
                if events.send(Event::Bus(BusEvent::Owner(bus_owner))).is_err() {
                    return;
                }
            }
            if events.send(Event::Bus(BusEvent::Down)).is_err() {
                return;
            }
        }
        thread::sleep(RETRY_INTERVAL);
        if let Ok(watch) = connect() {
            if events.send(up(&watch)).is_err() {
                return;
            }
            current = Some(watch);
        }
    }
}
