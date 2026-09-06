//! Turning one shepherd message into the reply that has to go back.
//!
//! An app must reply even to an action name it does not recognise. A slow
//! handler and a confused app both look like silence from the shepherd's
//! side. Only `action_timeout` tells them apart.
//!
//! Looking a handler up ([`Dispatch::resolve`]) and running it ([`run`])
//! are separate steps. That way the registry's lock is dropped before app
//! code runs. A handler that re-registers itself, like a `reload` action,
//! would otherwise deadlock on the same lock.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::{ChildMessage, ShepherdMessage};

/// What an action handler is: params, then the action's own name. Returns
/// the reply body the operator reads.
pub type ActionHandler = Box<dyn Fn(Option<&str>, &str) -> String + Send + Sync + 'static>;

/// What a shutdown handler is.
pub type ShutdownHandler = Box<dyn Fn() + Send + Sync + 'static>;

/// The registry's storage for an action handler: an `Arc`, not the `Box`
/// callers register with. [`Dispatch::resolve`] clones a handle out and
/// releases the registry's lock before the handler runs. Not part of the
/// public API: `register_action` converts a `Box` on the way in.
type ActionFn = dyn Fn(Option<&str>, &str) -> String + Send + Sync;

/// The shutdown-handler equivalent of [`ActionFn`].
type ShutdownFn = dyn Fn() + Send + Sync;

/// What handling one message produced.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Send this back.
    Reply(ChildMessage),
    /// A shutdown, and a handler ran.
    Handled,
    /// A shutdown, and no handler was registered.
    UnhandledShutdown,
    /// A shutdown, and the registered handler panicked. Carries the panic
    /// text, the same way a panicking action handler's reply does.
    ShutdownFailed(String),
}

/// What resolving one message against the registry found, before anything
/// has run. Carries a handle to whatever handler applies, or the context
/// to build a reply without one. That keeps the registry's lock free by
/// the time [`run`] calls into it.
pub(crate) enum Resolved {
    /// A registered action's handler, ready to call.
    Action {
        /// The handler to run.
        handler: Arc<ActionFn>,
        /// The action's own name, echoed into the reply.
        name: String,
        /// The action's argument text, if the trigger carried any.
        params: Option<String>,
        /// This action's correlation id, echoed into the reply.
        id: u64,
    },
    /// No handler is registered under this action name.
    UnknownAction {
        /// The action's own name, echoed into the reply.
        name: String,
        /// This action's correlation id, echoed into the reply.
        id: u64,
    },
    /// A registered shutdown handler, ready to call.
    Shutdown(Arc<ShutdownFn>),
    /// A shutdown with no handler registered.
    UnhandledShutdown,
}

/// The registered handlers.
#[derive(Default)]
pub(crate) struct Dispatch {
    actions: HashMap<String, Arc<ActionFn>>,
    shutdown: Option<Arc<ShutdownFn>>,
}

// Hand-written because a boxed closure is not `Debug` and the workspace
// denies `missing_debug_implementations`. Names what is registered, the
// only part worth seeing, and holds no user data (IR-41).
impl core::fmt::Debug for Dispatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut names: Vec<&str> = self.actions.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("Dispatch")
            .field("actions", &names)
            .field("shutdown", &self.shutdown.is_some())
            .finish()
    }
}

impl Dispatch {
    pub(crate) fn register_action(&mut self, name: String, handler: ActionHandler) {
        self.actions.insert(name, Arc::from(handler));
    }

    pub(crate) fn register_shutdown(&mut self, handler: ShutdownHandler) {
        self.shutdown = Some(Arc::from(handler));
    }

    /// Looks a message up against the registry and clones out whatever it
    /// finds. The only step that touches `self`; see the module doc for why
    /// that split matters.
    pub(crate) fn resolve(&self, message: ShepherdMessage) -> Resolved {
        match message {
            ShepherdMessage::Shutdown => match &self.shutdown {
                Some(handler) => Resolved::Shutdown(Arc::clone(handler)),
                None => Resolved::UnhandledShutdown,
            },
            ShepherdMessage::Action { name, params, id } => match self.actions.get(&name) {
                Some(handler) => Resolved::Action {
                    handler: Arc::clone(handler),
                    name,
                    params,
                    id,
                },
                None => Resolved::UnknownAction { name, id },
            },
        }
    }

    // Test-only: `reader_loop` calls `resolve` and `run` separately so it
    // can drop the registry's lock between them. This keeps them as one
    // call, for tests that exercise both with no lock to drop.
    #[cfg(test)]
    pub(crate) fn handle(&self, message: ShepherdMessage) -> Outcome {
        run(self.resolve(message))
    }
}

/// Runs a resolved message and builds the reply, catching any panic from
/// app code. Takes no lock: whatever produced `resolved` already released
/// the registry before calling this.
pub(crate) fn run(resolved: Resolved) -> Outcome {
    match resolved {
        Resolved::Action {
            handler,
            name,
            params,
            id,
        } => {
            let body = match catch_unwind(AssertUnwindSafe(|| handler(params.as_deref(), &name))) {
                Ok(body) => body,
                Err(payload) => format!("action handler failed: {}", panic_text(&*payload)),
            };
            Outcome::Reply(ChildMessage::ActionReply {
                action: name,
                body,
                id: Some(id),
            })
        }
        Resolved::UnknownAction { name, id } => Outcome::Reply(ChildMessage::ActionReply {
            body: format!("unknown action: {name}"),
            action: name,
            id: Some(id),
        }),
        Resolved::Shutdown(handler) => match catch_unwind(AssertUnwindSafe(handler.as_ref())) {
            Ok(()) => Outcome::Handled,
            Err(payload) => Outcome::ShutdownFailed(panic_text(&*payload)),
        },
        Resolved::UnhandledShutdown => Outcome::UnhandledShutdown,
    }
}

fn panic_text(payload: &(dyn core::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panicked with a non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn action(name: &str, params: Option<&str>, id: u64) -> ShepherdMessage {
        ShepherdMessage::Action {
            name: name.to_string(),
            params: params.map(str::to_string),
            id,
        }
    }

    fn reply_of(outcome: Outcome) -> (String, String, Option<u64>) {
        match outcome {
            Outcome::Reply(ChildMessage::ActionReply { action, body, id }) => (action, body, id),
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[test]
    fn a_registered_action_gets_its_handler_and_echoes_the_id() {
        let mut dispatch = Dispatch::default();
        dispatch.register_action(
            "gc".to_string(),
            Box::new(|params, name| format!("{name} ran with {params:?}")),
        );

        let (action, body, id) = reply_of(dispatch.handle(action("gc", Some("now"), 7)));
        assert_eq!(action, "gc");
        assert_eq!(body, "gc ran with Some(\"now\")");
        assert_eq!(
            id,
            Some(7),
            "the id must be echoed or the reply races the timeout"
        );
    }

    /// The contract calls this out: the operator waits out `action_timeout`
    /// for a typo.
    #[test]
    fn an_unregistered_action_still_gets_a_reply() {
        let dispatch = Dispatch::default();
        let (action, body, id) = reply_of(dispatch.handle(action("reload-config", None, 3)));
        assert_eq!(action, "reload-config");
        assert_eq!(body, "unknown action: reload-config");
        assert_eq!(id, Some(3));
    }

    /// An app that panics should not cost the operator a timeout too.
    #[test]
    fn a_panicking_handler_replies_with_the_panic_message() {
        let mut dispatch = Dispatch::default();
        dispatch.register_action("boom".to_string(), Box::new(|_, _| panic!("no such state")));

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = dispatch.handle(action("boom", None, 11));
        std::panic::set_hook(previous);

        let (action, body, id) = reply_of(outcome);
        assert_eq!(action, "boom");
        assert_eq!(body, "action handler failed: no such state");
        assert_eq!(id, Some(11));
    }

    #[test]
    fn a_shutdown_runs_its_handler_exactly_once() {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let mut dispatch = Dispatch::default();
        dispatch.register_shutdown(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        assert!(matches!(
            dispatch.handle(ShepherdMessage::Shutdown),
            Outcome::Handled
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// The library never stops the app, so this must be distinguishable
    /// before `kill_timeout`.
    #[test]
    fn a_shutdown_with_no_handler_is_reported_rather_than_ignored() {
        let dispatch = Dispatch::default();
        assert!(matches!(
            dispatch.handle(ShepherdMessage::Shutdown),
            Outcome::UnhandledShutdown
        ));
    }

    /// An unwind here would skip the reader's own `close()` and hang the
    /// writer in `pop()` forever.
    #[test]
    fn a_panicking_shutdown_handler_is_reported_rather_than_taking_the_reader_down() {
        let mut dispatch = Dispatch::default();
        dispatch.register_shutdown(Box::new(|| panic!("no such state")));

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = dispatch.handle(ShepherdMessage::Shutdown);
        std::panic::set_hook(previous);

        match outcome {
            Outcome::ShutdownFailed(message) => assert_eq!(message, "no such state"),
            other => panic!("expected ShutdownFailed, got {other:?}"),
        }
    }

    /// IR-41: the Debug is a decision, not a derive.
    #[test]
    fn debug_names_the_registered_actions_and_nothing_else() {
        let mut dispatch = Dispatch::default();
        dispatch.register_action("gc".to_string(), Box::new(|_, _| String::new()));
        dispatch.register_action("dump".to_string(), Box::new(|_, _| String::new()));
        assert_eq!(
            format!("{dispatch:?}"),
            "Dispatch { actions: [\"dump\", \"gc\"], shutdown: false }"
        );
    }
}
