//! Idle mode: when to hold a session-manager inhibitor.
//!
//! The rules:
//! - activity has to last `delay` without a break before inhibiting;
//! - at most one inhibitor is held, and it is released as soon as the
//!   activity ends;
//! - each owner of the session-manager name gets at most one successful
//!   `Inhibit` per active period; a failed call is not retried until the
//!   activity starts again or the owner changes;
//! - an inhibitor is gone with the owner that handed it out, and is never
//!   released on a different owner.

use std::time::{Duration, Instant};

/// The calls made to the session manager, addressed to one owner.
pub trait SessionBus {
    fn inhibit(&self, bus_owner: &str) -> Result<u32, String>;
    fn uninhibit(&self, bus_owner: &str, cookie: u32) -> Result<(), String>;
}

pub const INHIBITED: &str = "IDLE INHIBITED";
pub const NOT_INHIBITED: &str = "NOT IDLE INHIBITED";

struct Held {
    bus_owner: String,
    cookie: u32,
}

pub struct Idle<B> {
    delay: Duration,
    bus: Option<B>,
    bus_owner: Option<String>,
    held: Option<Held>,
    /// The current owner's one attempt for this active period was made.
    attempted: bool,
    /// When the current unbroken activity started.
    active_since: Option<Instant>,
}

impl<B: SessionBus> Idle<B> {
    pub fn new(delay: Duration) -> Self {
        Idle {
            delay,
            bus: None,
            bus_owner: None,
            held: None,
            attempted: false,
            active_since: None,
        }
    }

    /// The line describing the current state.
    pub fn line(&self) -> &'static str {
        if self.held.is_some() {
            INHIBITED
        } else {
            NOT_INHIBITED
        }
    }

    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }

    /// New stream activity; returns the line to print.
    pub fn activity(&mut self, active: bool, now: Instant) -> &'static str {
        if active {
            if self.active_since.is_none() {
                self.active_since = Some(now);
                self.attempted = false;
            }
        } else {
            self.active_since = None;
            self.attempted = false;
            self.release();
        }
        self.reconcile(now);
        self.line()
    }

    /// Connected to the session bus, with the session manager's owner.
    pub fn bus_up(
        &mut self,
        bus: B,
        bus_owner: Option<String>,
        now: Instant,
    ) -> Option<&'static str> {
        let before = self.is_held();
        self.bus = Some(bus);
        self.bus_owner = bus_owner;
        self.held = None;
        self.attempted = false;
        self.reconcile(now);
        self.changed(before)
    }

    /// The session-bus connection is gone, and with it any inhibitor.
    pub fn bus_down(&mut self) -> Option<&'static str> {
        let before = self.is_held();
        self.bus = None;
        self.bus_owner = None;
        self.held = None;
        self.attempted = false;
        self.changed(before)
    }

    /// The session manager's owner changed.
    pub fn owner_changed(
        &mut self,
        bus_owner: Option<String>,
        now: Instant,
    ) -> Option<&'static str> {
        if bus_owner == self.bus_owner {
            return None;
        }
        let before = self.is_held();
        self.bus_owner = bus_owner;
        // The old owner's inhibitor went with it; never release it on the
        // new owner, which did not hand out that cookie.
        self.held = None;
        self.attempted = false;
        self.reconcile(now);
        self.changed(before)
    }

    /// When the pending delay runs out, if an inhibit is waiting for it.
    pub fn deadline(&self) -> Option<Instant> {
        if self.held.is_some() || self.attempted || self.bus.is_none() || self.bus_owner.is_none() {
            return None;
        }
        self.active_since?.checked_add(self.delay)
    }

    /// Time passed; inhibit if the delay ran out.
    pub fn tick(&mut self, now: Instant) -> Option<&'static str> {
        let before = self.is_held();
        self.reconcile(now);
        self.changed(before)
    }

    /// Releases any inhibitor before exiting.
    pub fn shutdown(&mut self) {
        self.release();
    }

    fn changed(&self, before: bool) -> Option<&'static str> {
        (before != self.is_held()).then(|| self.line())
    }

    fn reconcile(&mut self, now: Instant) {
        if self.held.is_some() || self.attempted {
            return;
        }
        let Some(since) = self.active_since else {
            return;
        };
        match since.checked_add(self.delay) {
            Some(due) if due <= now => {}
            _ => return,
        }
        let (Some(bus), Some(bus_owner)) = (&self.bus, &self.bus_owner) else {
            return;
        };
        self.attempted = true;
        match bus.inhibit(bus_owner) {
            Ok(cookie) => {
                self.held = Some(Held {
                    bus_owner: bus_owner.clone(),
                    cookie,
                })
            }
            Err(error) => eprintln!("Inhibit on {bus_owner} failed: {error}"),
        }
    }

    fn release(&mut self) {
        let Some(held) = self.held.take() else {
            return;
        };
        if self.bus_owner.as_deref() != Some(held.bus_owner.as_str()) {
            return;
        }
        if let Some(bus) = &self.bus {
            if let Err(error) = bus.uninhibit(&held.bus_owner, held.cookie) {
                eprintln!("Uninhibit on {} failed: {error}", held.bus_owner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Inhibit(String),
        Uninhibit(String, u32),
    }

    #[derive(Clone, Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<Call>>>,
        fail: Rc<RefCell<u32>>,
        next: Rc<RefCell<u32>>,
    }

    impl SessionBus for Fake {
        fn inhibit(&self, bus_owner: &str) -> Result<u32, String> {
            self.calls
                .borrow_mut()
                .push(Call::Inhibit(bus_owner.into()));
            if *self.fail.borrow() > 0 {
                *self.fail.borrow_mut() -= 1;
                return Err("failed".into());
            }
            *self.next.borrow_mut() += 1;
            Ok(*self.next.borrow())
        }

        fn uninhibit(&self, bus_owner: &str, cookie: u32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(Call::Uninhibit(bus_owner.into(), cookie));
            Ok(())
        }
    }

    fn setup(delay: u64) -> (Idle<Fake>, Fake, Instant) {
        let fake = Fake::default();
        let mut idle = Idle::new(Duration::from_secs(delay));
        let now = Instant::now();
        idle.bus_up(fake.clone(), Some(":1.1".into()), now);
        (idle, fake, now)
    }

    fn calls(fake: &Fake) -> Vec<Call> {
        fake.calls.borrow().clone()
    }

    #[test]
    fn one_inhibit_per_active_period() {
        let (mut idle, fake, now) = setup(0);
        assert_eq!(idle.activity(false, now), NOT_INHIBITED);
        assert_eq!(idle.activity(true, now), INHIBITED);
        assert_eq!(idle.activity(true, now), INHIBITED);
        assert_eq!(idle.activity(false, now), NOT_INHIBITED);
        assert_eq!(idle.activity(true, now), INHIBITED);
        assert_eq!(
            calls(&fake),
            [
                Call::Inhibit(":1.1".into()),
                Call::Uninhibit(":1.1".into(), 1),
                Call::Inhibit(":1.1".into()),
            ]
        );
    }

    #[test]
    fn delay_waits_for_unbroken_activity() {
        let (mut idle, fake, start) = setup(2);
        assert_eq!(idle.activity(true, start), NOT_INHIBITED);
        assert_eq!(idle.deadline(), Some(start + Duration::from_secs(2)));
        assert_eq!(idle.tick(start + Duration::from_secs(1)), None);
        assert_eq!(
            idle.activity(false, start + Duration::from_secs(1)),
            NOT_INHIBITED
        );
        assert_eq!(idle.deadline(), None);
        let again = start + Duration::from_secs(3);
        idle.activity(true, again);
        assert_eq!(idle.tick(again + Duration::from_secs(1)), None);
        assert_eq!(idle.tick(again + Duration::from_secs(2)), Some(INHIBITED));
        assert_eq!(calls(&fake), [Call::Inhibit(":1.1".into())]);
    }

    #[test]
    fn huge_delay_never_inhibits() {
        let fake = Fake::default();
        let mut idle = Idle::new(Duration::from_secs(u64::MAX));
        let now = Instant::now();
        idle.bus_up(fake.clone(), Some(":1.1".into()), now);
        idle.activity(true, now);
        assert_eq!(idle.deadline(), None);
        assert_eq!(idle.tick(now + Duration::from_secs(1000)), None);
        assert!(calls(&fake).is_empty());
    }

    #[test]
    fn failure_is_not_retried_in_the_same_period() {
        let (mut idle, fake, now) = setup(0);
        *fake.fail.borrow_mut() = 1;
        assert_eq!(idle.activity(true, now), NOT_INHIBITED);
        assert_eq!(idle.activity(true, now), NOT_INHIBITED);
        assert_eq!(idle.tick(now + Duration::from_secs(5)), None);
        assert_eq!(idle.activity(false, now), NOT_INHIBITED);
        assert_eq!(idle.activity(true, now), INHIBITED);
        assert_eq!(
            calls(&fake),
            [Call::Inhibit(":1.1".into()), Call::Inhibit(":1.1".into())]
        );
    }

    #[test]
    fn new_owner_gets_its_own_inhibit_and_no_foreign_uninhibit() {
        let (mut idle, fake, now) = setup(0);
        idle.activity(true, now);
        assert_eq!(idle.owner_changed(Some(":1.2".into()), now), None);
        assert!(idle.is_held());
        assert_eq!(idle.activity(false, now), NOT_INHIBITED);
        assert_eq!(
            calls(&fake),
            [
                Call::Inhibit(":1.1".into()),
                Call::Inhibit(":1.2".into()),
                Call::Uninhibit(":1.2".into(), 2),
            ]
        );
    }

    #[test]
    fn owner_vanishing_clears_the_inhibitor() {
        let (mut idle, fake, now) = setup(0);
        idle.activity(true, now);
        assert_eq!(idle.owner_changed(None, now), Some(NOT_INHIBITED));
        assert_eq!(
            idle.owner_changed(Some(":1.3".into()), now),
            Some(INHIBITED)
        );
        assert_eq!(
            calls(&fake),
            [Call::Inhibit(":1.1".into()), Call::Inhibit(":1.3".into())]
        );
    }

    #[test]
    fn failed_inhibit_retried_on_new_owner() {
        let (mut idle, fake, now) = setup(0);
        *fake.fail.borrow_mut() = 1;
        idle.activity(true, now);
        assert!(!idle.is_held());
        assert_eq!(
            idle.owner_changed(Some(":1.4".into()), now),
            Some(INHIBITED)
        );
    }

    #[test]
    fn no_owner_means_no_call_until_one_appears() {
        let fake = Fake::default();
        let mut idle = Idle::new(Duration::ZERO);
        let now = Instant::now();
        idle.bus_up(fake.clone(), None, now);
        assert_eq!(idle.activity(true, now), NOT_INHIBITED);
        assert!(calls(&fake).is_empty());
        assert_eq!(
            idle.owner_changed(Some(":1.5".into()), now),
            Some(INHIBITED)
        );
    }

    #[test]
    fn shutdown_releases() {
        let (mut idle, fake, now) = setup(0);
        idle.activity(true, now);
        idle.shutdown();
        assert_eq!(
            calls(&fake),
            [
                Call::Inhibit(":1.1".into()),
                Call::Uninhibit(":1.1".into(), 1)
            ]
        );
    }
}
