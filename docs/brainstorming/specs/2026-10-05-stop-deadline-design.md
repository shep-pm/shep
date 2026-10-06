# A stop gets a deadline it can finish in (#690 item 1)

Approved 2026-10-05 (delegate mode).

## Problem

`shep stop` and `shep delete` send no deadline, so the client applies its
5s `DEFAULT_DEADLINE` and the daemon the matching `DEFAULT_DEADLINE_MS`. A
stop runs the kill ladder (up to `kill_timeout`) and then the lamb sweep
(up to another `kill_timeout`), so at `kill_timeout` 2.5s or more the reply
is `DeadlineExceeded` while the stop still finishes. Before the sweep this
already happened at 5s.

The same 5s default reaches four more senders: the foreground teardown
(`commands/foreground.rs`, stop and delete of all), whistle's `stop_sheep`
(`whistle/control.rs`), and lookout's generic `send`
(`lookout/source.rs`), which carries lookout's stop, restart and reload. Its
comment claims the deadline `commands::lifecycle` uses; restart there sends
`START_DEADLINE`.

## Design

- `shep-client` gains `pub const STOP_DEADLINE: Duration` of 60s, the
  daemon's `MAX_DEADLINE_MS`, with the sibling constants. The doc says why
  60 and not a `kill_timeout`-derived value: the CLI would need a round trip
  and still race the actor (the reason `restart` already gives), and a
  selector can match many sheep. A ladder plus sweep past about 58s still
  meets the daemon's clamp.
- A crate-private `deadline_for(&Request) -> Option<Duration>` in
  `shep-cli` (next to `commands::rpc`) names each verb's budget: Stop and
  Delete `STOP_DEADLINE`, Start and Restart `START_DEADLINE`, Reload
  `RELOAD_DEADLINE`, Trigger `TRIGGER_DEADLINE`, Reopen and Flush
  `LOG_PLANE_DEADLINE`, anything else `None` (the default).
- `stop`, `delete`, the foreground teardown, whistle's shepherd call and
  lookout's `send` use it. Existing explicit deadlines in
  `commands/lifecycle` may move to it where the value is the same; none
  changes value.

## Tests

- A table test pins `deadline_for` for every lifecycle verb.
- One test through the door the caller uses: a stop whose ladder outlasts
  5s on a paused clock answers `Stopped`, not `DeadlineExceeded`, when sent
  the way `commands::lifecycle::stop` sends it.

## Assumptions (approved)

1. Off `main`, separate from the sweep follow-ups: a pre-existing bug that
   touches no sweep code.
2. 60s rather than `START_DEADLINE`'s 30s: a longer deadline only waits
   longer on a stop that really is slow.
3. A new public constant is a minor change (`feat(shep-client)`), no `!`.
4. lookout's and whistle's restart and reload are fixed here too: same bug
   class, same code path.
