//! Execution observation: what actually happened, reported as it happens.
//!
//! The evaluator is the only thing that can say what ran. A trace read
//! off a value, a stdout line, or a source string is a *claim* about
//! execution, and a claim is what this whole architecture exists to
//! avoid trusting. So the observer is a port on the evaluator itself,
//! and it is opt-in:
//!
//! * **off costs nothing.** With no observer the evaluator does not
//!   construct a payload, does not serialize, and does not push to a
//!   container. That is a property of the code path, not a promise.
//! * **on is about semantics.** Events describe branches taken, calls
//!   made, errors raised, macro expansions performed — things that
//!   happened, in the order they happened.
//! * **it holds no product state.** An observer learns what ran; what
//!   that *means* — whether it was allowed, what it cost, whether it may
//!   be published — is the host's decision, made from these events
//!   through its own trust boundary.
//!
//! The core never imports an AI, a harness, or a product. A host that
//! wants a trace installs an [`Observer`]; nothing here knows one exists.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::span::Span;

/// What kind of thing happened. A closed set: a host that has never
/// heard of a kind cannot act on it, and adding one is a deliberate
/// change rather than an accident of a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A conditional was evaluated. `detail` names the branch taken.
    Branch,
    /// A function was applied. `detail` names it.
    Call,
    /// A tail call transferred the current frame.
    TailCall,
    /// An error was raised. `detail` carries the message.
    Error,
    /// A macro was expanded. `detail` names the macro.
    MacroExpansion,
    /// A value was returned from a function.
    Return,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Branch => "branch",
            EventKind::Call => "call",
            EventKind::TailCall => "tail-call",
            EventKind::Error => "error",
            EventKind::MacroExpansion => "macro-expansion",
            EventKind::Return => "return",
        }
    }
}

/// One thing that happened, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Dense, ordered, assigned at the observation point. A host can read
    /// events after `N` by this number alone.
    pub sequence: u64,
    pub kind: EventKind,
    /// A short, human-readable fact: a branch name, a macro name, an
    /// error message. Never a value dump — a value can be large, and a
    /// trace that grows with the data is a trace that leaks.
    pub detail: String,
    /// Where it happened, when there is a place it happened.
    pub span: Option<Span>,
}

/// The port. An implementation receives events; it decides what to do
/// with them (record, count, stream, refuse).
/// Extension trait for reading back a concrete observer.
///
/// `Observer` itself is deliberately not `Any`: the hot path calls
/// `on_event` and nothing else, and adding a vtable entry every embedder
/// pays for on every event would be a bad trade. A host that wants to
/// read the events back opts in by implementing this, which is what
/// makes the recorded trace available as data instead of a log line.
pub trait Observer: Send + Sync {
    fn on_event(&self, event: &Event);

    /// The events this observer has recorded, if it records any.
    ///
    /// On the trait rather than a subtrait because the evaluator holds a
    /// `&dyn Observer` and a trait object cannot be coerced to a
    /// subtrait — and because the default answer must be "nothing
    /// recorded", not an empty list that reads like a run that happened
    /// and observed nothing.
    fn recorded_recording(&self) -> Option<Vec<Event>> {
        None
    }
}

/// The state the evaluator carries: a sequence counter and the observer,
/// both absent when nobody is watching.
///
/// Attached through `&self` because a context is shared by every form
/// that evaluates in it, and attaching an observer is a decision the
/// embedder makes once, before anything runs.
#[derive(Default)]
pub struct Observation {
    observer: parking_lot::RwLock<Option<Arc<dyn Observer>>>,
    sequence: Arc<AtomicU64>,
    /// Per-port payload count. The global counter above is for a
    /// process-wide view; this one is what a test can measure without a
    /// neighbour's run moving the number underneath it.
    payloads: Arc<AtomicU64>,
    /// Evaluation steps spent, against a ceiling. `0` means unbounded:
    /// a normal script is not a suspect, and a budget nobody asked for
    /// would refuse work that is legitimate. A host that executes code
    /// it did not write sets the ceiling; `(loop [] ...)` then stops
    /// instead of running until the machine does.
    fuel: Arc<AtomicU64>,
    /// Ceiling on `fuel`. `0` = unbounded.
    fuel_ceiling: Arc<AtomicU64>,
}

impl Clone for Observation {
    /// A clone shares the observer and the sequence counter: a second
    /// context observing the same run must not renumber its events.
    fn clone(&self) -> Self {
        Observation {
            observer: parking_lot::RwLock::new(self.observer.read().clone()),
            sequence: Arc::clone(&self.sequence),
            payloads: Arc::clone(&self.payloads),
            fuel: Arc::clone(&self.fuel),
            fuel_ceiling: Arc::clone(&self.fuel_ceiling),
        }
    }
}

impl Observation {
    /// Attach an observer. Called before evaluation starts; the sequence
    /// counter is shared, so two contexts observing the same run still
    /// number their events in one order.
    pub fn attach(&self, observer: Arc<dyn Observer>) {
        *self.observer.write() = Some(observer);
    }

    pub fn is_active(&self) -> bool {
        self.observer.read().is_some()
    }

    /// The events an attached recording observer has collected.
    ///
    /// The observer is held as a trait object, so this reads through
    /// [`ObserverInspect`] rather than downcasting at the call site: a
    /// `&dyn Observer` cannot be coerced to a subtrait, and pretending
    /// the events are available for an observer that never recorded any
    /// would report an empty trace for a run that was not empty.
    pub fn recorded_events(&self) -> Option<Vec<Event>> {
        let guard = self.observer.read();
        let observer = guard.as_ref()?;
        observer.recorded_recording()
    }

    /// Report one thing that happened.
    ///
    /// The first check is the whole point: with no observer there is no
    /// `String` built, no span copied, no counter touched. The caller
    /// checks `is_active` before computing an argument, so the argument
    /// itself is not built either.
    pub fn report(&self, kind: EventKind, detail: impl FnOnce() -> String, span: Option<Span>) {
        // The read is taken by clone so the payload is built outside the
        // lock, and the whole body is skipped when nobody is watching.
        let observer = self.observer.read().clone();
        let Some(observer) = observer else {
            return;
        };
        let event = Event {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            kind,
            detail: detail(),
            span,
        };
        self.payloads.fetch_add(1, Ordering::Relaxed);
        observer.on_event(&event);
    }
}

impl Observation {
    /// Bound this context's evaluation to `ceiling` steps. `0` lifts the
    /// bound. A host that runs code it did not write sets this; the
    /// evaluator enforces it, so the program cannot argue itself more time.
    pub fn set_fuel_ceiling(&self, ceiling: u64) {
        self.fuel_ceiling.store(ceiling, Ordering::Relaxed);
        self.fuel.store(0, Ordering::Relaxed);
    }

    /// Steps spent so far.
    pub fn fuel_spent(&self) -> u64 {
        self.fuel.load(Ordering::Relaxed)
    }

    /// Spend one step. `false` means the ceiling is spent: the caller
    /// must stop. One relaxed atomic add per evaluated node — the check
    /// is one branch on top of work the evaluator already does.
    pub fn spend_fuel(&self) -> bool {
        self.spend_fuel_by(1)
    }

    /// Spend `units` steps in one charge. A compiled loop spends per
    /// instruction, and paying an atomic per instruction there would
    /// make the accounting more expensive than the work. `false` means
    /// the ceiling is spent and the caller must stop.
    pub fn spend_fuel_by(&self, units: u64) -> bool {
        let ceiling = self.fuel_ceiling.load(Ordering::Relaxed);
        if ceiling == 0 {
            return true;
        }
        // A saturating add: a ceiling is a bound, and an overflow would
        // wrap into "spent nothing".
        self.fuel
            .fetch_add(units, Ordering::Relaxed)
            .saturating_add(units)
            <= ceiling
    }

    /// Payloads this port has constructed.
    ///
    /// A test instrument, and a real one: the claim that an unobserved
    /// run costs nothing cannot be argued from the code reading
    /// "correct", so it needs a number that moves only when a payload is
    /// really built. Per-port rather than global, so a measurement is
    /// not perturbed by another context observing at the same time.
    pub fn payloads_built(&self) -> u64 {
        self.payloads.load(Ordering::Relaxed)
    }
}

/// The port used by a runtime that carries no observer of its own.
///
/// It is permanently off, so `report` costs one branch and nothing else:
/// no payload is built, nothing is serialized, no counter is touched.
pub fn inert() -> &'static Observation {
    static INERT: std::sync::OnceLock<Observation> = std::sync::OnceLock::new();
    INERT.get_or_init(Observation::default)
}

/// An observer that keeps every event, for tests and for a host that
/// batches them into an artifact.
#[derive(Default)]
pub struct RecordingObserver {
    events: parking_lot::Mutex<Vec<Event>>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        RecordingObserver::default()
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Observer for RecordingObserver {
    fn on_event(&self, event: &Event) {
        self.events.lock().push(event.clone());
    }

    fn recorded_recording(&self) -> Option<Vec<Event>> {
        Some(self.events())
    }
}
