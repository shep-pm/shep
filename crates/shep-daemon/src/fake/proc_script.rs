use super::scripted_runner::NEVER_MS;
use crate::runner::ExitOutcome;

/// How one scripted process behaves when spawned & waited by [`ScriptedRunner`](crate::fake::ScriptedRunner)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcScript {
    /// Milliseconds after spawn the process exits on its own
    pub delay_ms: u64,
    /// The outcome reported when the natural exit deadline is reached
    pub outcome: ExitOutcome,
    /// Whether the process honors `signal()`/`Shutdown` by exiting early
    pub obeys_signal: bool,
    /// Whether `kill_tree()` resolves its `wait()`. `true` for every ordinary
    /// process (`SIGKILL` cannot be caught), `false` only for
    /// [`ProcScript::never_reports_its_exit`], which models the one child a
    /// kill ladder cannot end.
    pub obeys_kill: bool,
    /// Whether a forked lamb keeps the child's stdout and stderr open past
    /// the child's own exit, so neither stream ever reaches EOF. See
    /// [`ProcScript::with_a_lamb_holding_the_pipe`].
    pub lamb_holds_the_pipe: bool,
    /// Whether a stdin write to this proc is acknowledged. `true` for every
    /// ordinary process, and `false` only for
    /// [`ProcScript::never_reads_its_stdin`], which models an app that has
    /// stopped reading fd 0.
    pub reads_stdin: bool,
}

impl ProcScript {
    /// Exits immediately with `code`
    #[must_use]
    pub fn const_exit(code: i32) -> Self {
        Self::stable_then_exit(0, code)
    }

    /// Exits after `ms` milliseconds with `code`
    #[must_use]
    pub fn stable_then_exit(ms: u64, code: i32) -> Self {
        Self {
            delay_ms: ms,
            outcome: ExitOutcome {
                code: Some(code),
                signal: None,
            },
            obeys_signal: true,
            obeys_kill: true,
            lamb_holds_the_pipe: false,
            reads_stdin: true,
        }
    }

    /// Never exits on its own; still obeys signals
    #[must_use]
    pub fn never_exits() -> Self {
        Self {
            delay_ms: NEVER_MS,
            outcome: ExitOutcome {
                code: None,
                signal: None,
            },
            obeys_signal: true,
            obeys_kill: true,
            lamb_holds_the_pipe: false,
            reads_stdin: true,
        }
    }

    /// Never exits on its own and ignores signals: only `kill_tree` ends it
    #[must_use]
    pub fn ignores_signals() -> Self {
        Self {
            obeys_signal: false,
            ..Self::never_exits()
        }
    }

    /// Never resolves its `wait()` at all: not on a signal, and not on
    /// `kill_tree` either.
    ///
    /// Models the one child a kill ladder cannot end: wedged in
    /// uninterruptible sleep, where `SIGKILL` is delivered and accepted by
    /// the kernel but `wait(2)` never returns. Lets a test see what the
    /// supervisor does when a message it is waiting on never comes.
    ///
    /// The kill is still delivered and counted
    /// ([`ScriptedRunner::kill_counts`](crate::fake::ScriptedRunner::kill_counts)); only the exit is withheld.
    #[must_use]
    pub fn never_reports_its_exit() -> Self {
        Self {
            obeys_kill: false,
            ..Self::ignores_signals()
        }
    }

    /// This script, with a forked lamb holding the child's stdout and stderr
    /// open past the child's own exit.
    ///
    /// A scripted proc's log-control task otherwise ends with the proc: both
    /// streams reach EOF when the child does. A lamb that inherited them
    /// keeps the pump alive on one of its other conditions instead: the
    /// `logs` receiver going away, or the last control sender dropping.
    #[must_use]
    pub fn with_a_lamb_holding_the_pipe(self) -> Self {
        Self {
            lamb_holds_the_pipe: true,
            ..self
        }
    }

    /// With [`SpawnSpec::stdin`](crate::runner::SpawnSpec::stdin) enabled, accepts every stdin write and
    /// answers none of them: models an app that stopped reading fd 0. The
    /// write is delivered and recorded, but the `done` acknowledgement is
    /// withheld. With `stdin` disabled the runner closes the writer instead.
    #[must_use]
    pub fn never_reads_its_stdin() -> Self {
        Self {
            reads_stdin: false,
            ..Self::never_exits()
        }
    }
}
