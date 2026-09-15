//! The verb table: one `clap::Subcommand` enum wiring every command to the
//! argument struct its own module owns.
//!
//! Nothing here parses a value. What the enum holds besides the wiring is
//! the `visible_alias` and `hide` attributes that decide which verbs
//! `shep --help` lists and under what spellings, so the tests at the bottom
//! are alias and visibility pins rather than grammar ones.

use super::{
    AdoptArgs, BarksArgs, BleatsArgs, CompletionArgs, DaemonArgs, DevArgs, DogArgs, DogsArgs,
    EnableArgs, FlockArgs, FlushArgs, FoldArgs, ImportArgs, InitArgs, KvGetArgs, KvSetArgs,
    KvUnsetArgs, LookoutArgs, ReopenArgs, RuntimeArgs, SecretArgs, SelectorArgs, ServeArgs,
    SignalArgs, StartArgs, StartupArgs, StockArgs, StyleArgs, TriggerArgs, WhisperArgs,
};

/// Every verb the binary understands.
#[derive(Debug, clap::Subcommand)]
pub enum Commands {
    /// Start a sheep from a script, a Flockfile, or stdin.
    Start(StartArgs),
    /// Register a sheep without starting it.
    ///
    /// The same targets `shep start` takes and the same load path. The one
    /// difference is that nothing spawns: the app lands registered and
    /// stopped, and `shep start <name>` is what brings it up.
    ///
    /// It is here for the Flockfile a project commits with its secrets left
    /// blank: `env = { DB_HOST = "", DB_PASSWORD = "" }`, the
    /// `.env.example` convention. Starting that file spawns a process
    /// against an empty database URL, which crashes, spends its restart
    /// budget, and has to be stopped before anyone can configure it. This
    /// registers it instead, so the order becomes register, fill in, start.
    ///
    /// An app the flock already has is merged into and left exactly as it
    /// is, running or not.
    Add(StartArgs),
    /// Serve a directory over plain HTTP, as a managed sheep.
    ///
    /// Registers a sheep whose command line is this invocation, canonicalized
    /// and with `--foreground` appended — the same worker that answers every
    /// request also IS the registered sheep, so `shep describe` shows exactly
    /// what will run again on a restart.
    ///
    /// Binds loopback (`127.0.0.1:8080`) by default. A wider `--bind` is
    /// allowed, not refused, and gets a stderr notice naming what it exposes
    /// — the docroot is published to anything that can reach the port, and
    /// unencrypted unless the operator puts a proxy in front. `--auth`
    /// narrows that to anyone who also has the password, still sent as plain
    /// HTTP basic auth.
    ///
    /// Dotfiles, directory listings, and any symlink under the docroot are
    /// all refused by default — `--hidden`, `--listing`, and
    /// `--follow-symlinks` opt back in, each with its own reason a repo
    /// checkout or a deploy layout might need it.
    ///
    /// `--foreground` runs the worker directly in this terminal instead of
    /// registering a sheep — also how the registered sheep runs; the flag on
    /// the end of its own command line is the only difference.
    Serve(ServeArgs),
    /// Stop one or more sheep.
    Stop(SelectorArgs),
    /// Restart one or more sheep.
    ///
    /// The running process is killed and a new one spawned in its place, so
    /// there is a window with nothing serving. `shep reload` is the verb
    /// that closes that window, at the cost of caring how the app is
    /// configured.
    ///
    /// A selector matching one sheep is answered as soon as the respawn is
    /// issued, printing the flock as it stood at that moment.
    ///
    /// A selector matching two or more is walked in dependency order
    /// instead, and the reply waits: each stage is held until the apps a
    /// later stage depends on are back, so a fold comes back in the order
    /// its depends_on lines describe rather than all at once. The wait is
    /// the sum of the stages.
    ///
    /// That walk asks the shepherd once per app, so an app it could not
    /// restart is refused on its own and the rest of the fold restarts
    /// anyway. The rows printed are what the walk finished with, the app it
    /// went around is named on stderr, and the exit is non-zero. Under
    /// `--format json` the same names come back under refused in the one
    /// envelope.
    ///
    /// A sheep that came back errored is a different failure and is reported
    /// separately: that one was reached, and it is the child that could not
    /// start. That failure empties stdout, the way every verb's does, so a
    /// restart that BOTH refused an app and brought one back errored prints
    /// no envelope at all. The refused names ride the stderr sentence in
    /// that case, which is the only place left for them.
    Restart(SelectorArgs),
    /// Reload one or more sheep, one instance at a time.
    ///
    /// Each instance is replaced by a fresh one that has to become ready
    /// before the reload moves on, so a release that never comes up is
    /// reported as a failure rather than as a success.
    ///
    /// What that failure COSTS depends on the order, and the order depends on
    /// the app.
    ///
    /// An app with a readiness_probe and no reuse_port is replaced serially:
    /// the old instance drains first, then the new one starts in its place. A probe asks an
    /// address, and an address cannot say which process answered it. Run both
    /// at once and the outgoing instance answers for the incoming one, so
    /// shep would call a release ready that never bound anything.
    ///
    /// The old instance is gone by the time the new one is judged, so there is
    /// nothing to go back to. A replacement that never becomes ready is left
    /// running and NOT marked online, and the reload is abandoned, which for a
    /// one-instance app means the gap lasts until you act on it. A rollback
    /// that points the app back at working code and reloads again is what ends
    /// it; that reload can reach the instance this one left behind.
    ///
    /// Everything else overlaps, old and new running together: an app with no
    /// probe, an app using wait_ready (its channel belongs to one instance,
    /// so nothing else can answer it), and a probed app that sets reuse_port.
    ///
    /// An overlap asks the same thing of all three. Both instances are bound
    /// at once, so an app that binds an address has to share the socket
    /// itself, with SO_REUSEPORT set before it binds; shep binds nothing and
    /// cannot set it on the app's behalf. Without that the replacement takes
    /// EADDRINUSE on every reload, and the reload is abandoned with the old
    /// instance left serving. This command has already exited 0 by then, so
    /// process.reload_abandoned on the bus is the only report of it.
    ///
    /// reuse_port neither creates that requirement nor satisfies it. It is how
    /// a probed app says it is already handling the sharing, and so asks for
    /// the overlap back.
    ///
    /// An overlap is not zero downtime either. The old listener's queue of
    /// connections it has not accepted yet is dropped when it closes, so an
    /// app that does not stop accepting and finish what it has in hand
    /// before graceful_timeout runs out loses whatever was waiting there.
    ///
    /// A selector matching one app exits as soon as the shepherd accepts the
    /// reload, printing the flock as it stood at that moment. A clustered app
    /// takes longer to swap than any answer can wait for.
    ///
    /// A selector matching two or more apps is walked in dependency order
    /// instead, and the reply waits: each stage is held until the apps a
    /// later stage depends on are back and ready, so the wait is the sum of
    /// the stages rather than the longest swap in the fold. The rows printed
    /// are what the walk finished with, and an app it could not reload is
    /// named, with a non-zero exit.
    ///
    /// The swaps themselves are reported on the bus either way, under
    /// process.reload, process.reloaded and process.reload_abandoned.
    Reload(SelectorArgs),
    /// Delete one or more sheep from the flock.
    Delete(SelectorArgs),
    /// Set how many instances one app runs — the stocking rate.
    ///
    /// An absolute count, not a change: `shep stock web 4` means web has four
    /// instances afterwards, whatever it had before. There is no +N/-N form —
    /// run it twice and get the same flock.
    ///
    /// Stocking up fills the lowest free instance slots; stocking down releases
    /// the highest, so stocking out and back returns the same slot numbers, the
    /// same SHEP_INSTANCE values and the same log files it started with.
    ///
    /// Exits as soon as the shepherd accepts, printing the instances that
    /// remain. On a stock-down the departing instances are still running their
    /// stop ladders at that point; they report themselves on the bus, under
    /// process.delete.
    ///
    /// The new count is written to the muster roll, so `shep save` and a
    /// reboot keep it.
    #[command(visible_alias = "scale")]
    Stock(StockArgs),
    /// List the flock.
    #[command(visible_aliases = ["list", "ls"])]
    Flock(FlockArgs),
    /// List the dogs, and nothing else. `--available` lists the community
    /// index of dogs you could adopt instead of the ones this shepherd is
    /// running.
    Dogs(DogsArgs),
    /// Turn on a registered dog: writes `[daemon] enabled_dogs` in
    /// `shep.toml`, and starts it now if a shepherd is running.
    ///
    /// Writes the config either way and exits 0 even with no shepherd
    /// running — the dog comes up with the next one. `shep muster` is the
    /// only verb that autostarts a shepherd; this is not it.
    Enable(EnableArgs),
    /// Turn off a registered dog: removes it from `[daemon] enabled_dogs`,
    /// and stops it now if a shepherd is running.
    ///
    /// Leaves `[<name>]` in `dogs.toml` in place: the dog's own
    /// configuration survives a disable/enable cycle. `shep rehome` leaves
    /// it too, and forgets the adoption as well.
    Disable(DogArgs),
    /// Vet a binary shep has never seen and register it as a dog: writes
    /// `[daemon] adopted_dogs` and `[daemon] enabled_dogs` in `shep.toml`,
    /// and starts it now if a shepherd is running.
    ///
    /// The path can be given as-is, with a leading `~/`, or as a bare name
    /// already on `$PATH` (`cargo install` puts one there). Refuses, before
    /// touching the config at all, a path that resolves to nothing that
    /// exists, is not a file, has no execute bit set, or that this kernel
    /// will not exec — and refuses a name that already names a built-in
    /// verb or alias, since such a dog could never be reached. An adopted
    /// dog runs at the shepherd's own trust level, with no sandboxing
    /// beyond it. Once adopted, `shep <name> [args...]` runs it directly,
    /// passing `args` through untouched — a second invocation mode from
    /// the one the shepherd itself uses to supervise it.
    Adopt(AdoptArgs),
    /// Forget where an adopted dog's binary lived: stops it if a shepherd
    /// is running, and removes it from `[daemon] enabled_dogs` and
    /// `[daemon] adopted_dogs`.
    ///
    /// Its `[<name>]` table in `dogs.toml` stays, as it does through a
    /// `shep disable`: those settings are the operator's, so adopting the
    /// same dog again finds them waiting. Delete that table by hand to be
    /// rid of them. Only the adoption differs from `disable`, so recovery
    /// is a fresh `shep adopt <path>` rather than an `enable`.
    Rehome(DogArgs),
    /// Describe one sheep in detail.
    ///
    /// Includes the sheep's lambs: the processes the OS reports as
    /// descendants of its pid. That is not the same set the stop ladder
    /// kills, which acts on the process group — a double-forked descendant
    /// leaves this list and is still killed, and a setsid() one stays in it
    /// and survives.
    ///
    /// Lamb names are executable names, never command lines.
    Describe(SelectorArgs),
    /// Send a named action to matched sheep and report what each app
    /// answers.
    ///
    /// Reaches an app over its shepherd channel — the fd-3 pipe the daemon
    /// opens when the app's Flockfile sets `channel = true`. `wait_ready`
    /// and `shutdown_with_message` both imply the same channel, so either
    /// one of the three is enough; a sheep with none of them answers a
    /// `no_channel` row instead of a reply, naming the same fields.
    ///
    /// `action` and any `params` are free-form and unvalidated here — sent
    /// to the app verbatim, on its own shepherd-channel wire, for the app
    /// itself to recognize or refuse.
    Trigger(TriggerArgs),
    /// Send a unix signal to matched sheep.
    ///
    /// Delivered to each sheep's own process, not to its process group — the
    /// lambs it forked are not signalled. This is a nudge to the application
    /// (SIGHUP to re-read config, SIGUSR1 to dump state); `shep stop` is what
    /// runs the stop ladder, and `shep reload` is what swaps instances.
    ///
    /// Accepted: SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2, SIGWINCH,
    /// SIGCONT, SIGKILL. The SIG prefix and the case are both optional.
    /// SIGSTOP is refused: a stopped sheep still reads online in every listing
    /// shep can produce.
    ///
    /// Delivery is not action. A signal the app blocks or ignores is reported
    /// delivered, because the kernel took it and there is nothing further shep
    /// can see.
    Signal(SignalArgs),
    /// Write one line to matched sheep's stdin.
    ///
    /// Only reaches an app whose Flockfile sets `stdin = true`. Nothing else
    /// implies it — unlike the shepherd channel, which `wait_ready` and
    /// `shutdown_with_message` both turn on — because nothing in shep needs a
    /// sheep's stdin except this verb. A sheep without it answers a `no_stdin`
    /// row naming the field.
    ///
    /// One line, and the terminator is shep's to add: a line containing a
    /// newline or a carriage return is a usage error rather than two commands.
    ///
    /// `sent` means the bytes were written and flushed to the pipe, not that
    /// the app read them. A pipe holds 64 KiB before it blocks, so a short line
    /// to an app that never reads its stdin is still `sent`.
    #[command(visible_alias = "sendline")]
    Whisper(WhisperArgs),
    /// List one fold.
    Fold(FoldArgs),
    /// Show or follow bleats (log output) for one or more sheep.
    #[command(visible_alias = "logs")]
    Bleats(BleatsArgs),
    /// Watch the flock on a live dashboard.
    ///
    /// Reads the shepherd two ways at once: it subscribes to the event bus so
    /// the screen moves as things happen, and it re-lists the flock every two
    /// seconds so a dropped event cannot leave the screen quietly wrong.
    ///
    /// If the shepherd stops answering, lookout re-dials a few times and then
    /// says so and stops updating. The values on screen stay exactly as they
    /// were, and it does not exit — you do.
    ///
    /// Needs a terminal: with stdout redirected it refuses rather than writing
    /// escape sequences into a file.
    #[command(visible_alias = "dash")]
    Lookout(LookoutArgs),
    /// Serve the MCP interface on stdin/stdout for an AI agent.
    ///
    /// Speaks the Model Context Protocol over stdio: an agent host launches
    /// this process and talks JSON-RPC to it on the pipe. It writes nothing
    /// else to stdout, because stdout is the wire.
    ///
    /// Five read-only tools are always offered. The four that act —
    /// start_sheep, stop_sheep, restart_sheep, reload_sheep — exist only when
    /// `[whistle] allow_control = true` in `$SHEP_HOME/shep.toml`.
    ///
    /// That gate is a guard against an agent acting on its own reading of
    /// your flock, not a security boundary: whistle runs as you, so anything
    /// it could do you can already do with `shep stop`. There is deliberately
    /// no flag for it — legibility, not containment: a boolean in
    /// `shep.toml` has a diff and an mtime an operator can audit, and
    /// `--home`/`SHEP_HOME` already choose which `shep.toml` that is, so a
    /// flag would open nothing those don't already.
    Whistle,
    /// Reopen log files after an external rotator has renamed them.
    Reopen(ReopenArgs),
    /// Empty the log files of one or more sheep, or the shepherd's own.
    Flush(FlushArgs),
    /// Show the alert history: `barks.jsonl`, newest last.
    ///
    /// Reads the file directly and never connects to the shepherd — the
    /// history is on disk precisely so it survives the shepherd, and the
    /// case this verb exists for is an operator reading it after a crash.
    /// Same precedent as `shep flush --daemon`, which also works on files
    /// rather than through the socket.
    Barks(BarksArgs),
    /// Store a value in the shepherd's key/value store.
    ///
    /// Reads and writes `$SHEP_HOME/kv.json` directly and never connects to the
    /// shepherd — the store is for ad-hoc notes and dog settings, and it has to
    /// work while nothing is running, exactly as `shep enable` does.
    ///
    /// Keys are flat: letters, digits, `.`, `_` and `-`, up to 128 bytes, not
    /// starting with a dot. A dot is part of the name — `bark.cooldown` is one
    /// key, not a path into anything.
    Set(KvSetArgs),
    /// Read one value from the store, or list the whole store with no key.
    Get(KvGetArgs),
    /// Remove one key from the store, or every key with --all.
    Unset(KvUnsetArgs),
    /// Store the values a Flockfile refers to and never carries.
    ///
    /// Reads and writes `$SHEP_HOME/secrets.json` directly and never
    /// connects to the shepherd, exactly as `shep set` does: filling the
    /// store before the first `shep start` is the ordinary first run.
    ///
    /// A key holds one value per environment, so production and staging
    /// differ without two config files. A `{{secret:NAME}}` reference in a
    /// Flockfile resolves against the sheep's own environment and then the
    /// `all` slot, never another named environment.
    ///
    /// Keys and environment names are letters, digits, `.`, `_` and `-`, up
    /// to 128 bytes, not starting with a dot.
    ///
    /// Reading a value back is off until `[secrets] allow_read = true` is in
    /// `$SHEP_HOME/shep.toml`. `secret list` needs nothing: it names keys and
    /// the environments each has a value for, never a value.
    Secret(SecretArgs),
    /// Check whether the shepherd answers.
    Ping,
    /// Shut the shepherd down.
    Kill,
    /// Write the muster roll now, so a reboot can bring this flock back.
    Save,
    /// Assemble the flock from the muster roll `save` wrote, starting the
    /// shepherd first if none is running.
    // Hidden alias `resurrect` (pm2's own word for this), so the muscle
    // memory carries over: `alias`, not `visible_aliases`, so it stays out
    // of `--help` rather than being taught by it. A plain `//` comment
    // rather than `///` on purpose — the paragraph above already becomes
    // this subcommand's own `--help` text, and naming the alias there would
    // defeat the point of keeping it hidden.
    #[command(alias = "resurrect")]
    Muster,
    /// Write a commented Flockfile to start from
    Init(InitArgs),
    /// Boot a shepherd in this process, run one Flockfile's flock in the
    /// foreground, and exit once nothing is left online.
    ///
    /// Meant for a container: no daemonization, no re-exec, no saved muster
    /// roll — `--no-restore` is always on, because a container starts from
    /// its Flockfile every time, never from a roll left on the image by a
    /// previous run.
    ///
    /// Bleats stream to this process's own stdout/stderr while it runs, so
    /// `docker logs` is the flock's log without any extra plumbing. The
    /// shepherd is still reachable over its own socket the whole time —
    /// `shep flock` from a second terminal, or `docker exec`, works exactly
    /// as it would against a daemonized one.
    ///
    /// Exits 0 once the flock has been empty and clean (every sheep
    /// `stopped`, none `errored`) for three consecutive two-second polls —
    /// a batch job finishing its work. Exits 11 (`flock_empty`) instead when
    /// the flock emptied with at least one sheep `errored` — a restart
    /// budget exhausted, or a spawn that never came up — so an orchestrator
    /// reading the exit status can tell "finished" from "died" and restart
    /// the container only for the second.
    Runtime(RuntimeArgs),
    /// Run one Flockfile's flock in an isolated, throwaway foreground
    /// session: `$SHEP_DEV_HOME` (default `~/.shep-dev`), forced `watch =
    /// true` on every app, and a full stop-and-delete teardown when it ends.
    ///
    /// **`--home` and `$SHEP_HOME` are ignored.** Isolation is the whole
    /// feature: an operator who exports `$SHEP_HOME` for their real flock
    /// gets a stderr notice rather than a `dev` session that shares it and
    /// silently forces `watch = true` onto production apps.
    ///
    /// Ends the moment the flock empties or this process is signalled —
    /// whichever comes first — and either way leaves nothing running and no
    /// shepherd behind. A `shep dev` that leaked a supervisor would stop
    /// being trusted.
    Dev(DevArgs),
    /// Read somebody else's config into shep: a pm2 dump, or a `.env`.
    Import(ImportArgs),
    /// Install an init unit so the shepherd starts at boot.
    ///
    /// Writes an init unit for the target user — a systemd unit
    /// (`Type=notify`), a launchd plist, an openrc script, or a FreeBSD or
    /// OpenBSD `rc.d` script, picked automatically for the running target
    /// or named explicitly with `--init` below. Every unit carries this
    /// binary's own path, that user's $SHEP_HOME, and the PATH of this
    /// invocation — which is what makes an interpreter installed under
    /// ~/.bun or ~/.cargo findable after a reboot.
    ///
    /// The openrc and BSD scripts are rendered and pinned by exact-string
    /// tests; nobody on this project has run them on their own init system.
    ///
    /// Needs root, and never asks for it: without it this prints the exact
    /// command to run and exits non-zero, so a script notices. Under sudo
    /// the unit is built for $SUDO_USER rather than root, so it supervises
    /// the flock the operator actually has.
    ///
    /// Under sudo this also warns that PATH may have been replaced by
    /// sudo's own secure_path before shep ever saw it, and shows the exact
    /// PATH about to go into the unit so you can check it yourself.
    Startup(StartupArgs),
    /// Disable and remove whichever unit `startup` installed — systemd,
    /// openrc, launchd, or a BSD `rc.d` script.
    ///
    /// Needs root under the same rule: without it, prints the command to
    /// run and exits non-zero. A unit that is not there is reported absent
    /// rather than failing.
    Unstartup(StartupArgs),
    /// Print a shell completion script.
    ///
    /// Static only: sheep names, fold names and other daemon-side
    /// identifiers are never completed.
    Completions(CompletionArgs),
    /// Print the welcome: the sheep, and the five commands worth knowing.
    ///
    /// The same text a fresh `$SHEP_HOME` prints once on its own. Here it is
    /// the command's output rather than a diagnostic, so it goes to stdout.
    Welcome,
    /// Show or set how much shep dresses up its output
    ///
    /// `full` is sheep, boxes and colour; `plain` drops the sheep; `bare` is
    /// plain text. With no level, prints the one in force and where it came
    /// from.
    Style(StyleArgs),
    /// Graceful stop. Easter-egg alias for `stop`.
    #[command(hide = true)]
    Thatlldo(SelectorArgs),
    /// Run the supervisor in the foreground. Spawned by the CLI; not for direct use.
    #[command(hide = true)]
    Daemon(DaemonArgs),
    /// Run one built-in dog in the foreground. Spawned by the shepherd as
    /// `<this binary> dog <name>`; not for direct use.
    #[command(hide = true)]
    Dog(DogArgs),
    /// Print the Flockfile JSON Schema. Hidden: the schema is committed at
    /// `crates/shep-core/assets/flockfile.schema.json`, and this is how it is
    /// regenerated.
    #[command(hide = true)]
    Schema,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;

    #[test]
    fn list_and_ls_both_reach_flock() {
        use clap::Parser;
        for argv in [["shep", "flock"], ["shep", "list"], ["shep", "ls"]] {
            assert!(matches!(
                Cli::try_parse_from(argv).unwrap().command,
                Commands::Flock(_)
            ));
        }
    }

    #[test]
    fn logs_reaches_bleats() {
        use clap::Parser;
        assert!(matches!(
            Cli::try_parse_from(["shep", "logs"]).unwrap().command,
            Commands::Bleats(_)
        ));
    }

    /// Precedent: [`list_and_ls_both_reach_flock`]. Fails if `scale` stops
    /// being a visible alias for `stock`, or if either spelling stops
    /// reaching [`Commands::Stock`].
    #[test]
    fn stock_and_scale_both_reach_stock() {
        use clap::Parser;
        for argv in [["shep", "stock", "web", "3"], ["shep", "scale", "web", "3"]] {
            assert!(matches!(
                Cli::try_parse_from(argv).unwrap().command,
                Commands::Stock(_)
            ));
        }
    }

    /// Pins `Flock`'s, `Bleats`'s, `Stock`'s and `Whisper`'s visible
    /// aliases, and that the hidden verbs (`thatlldo`, the internal `daemon`
    /// re-exec target) stay hidden from `--help`. A
    /// `visible_aliases`/`aliases` swap, or a dropped `hide = true`, passes
    /// every other test in this module but changes user-facing behavior
    /// silently.
    #[test]
    fn alias_visibility_and_hiding_are_pinned() {
        use clap::CommandFactory;
        let cmd = Cli::command();

        let flock = cmd.find_subcommand("flock").unwrap();
        assert_eq!(
            flock.get_visible_aliases().collect::<Vec<_>>(),
            ["list", "ls"]
        );

        let bleats = cmd.find_subcommand("bleats").unwrap();
        assert_eq!(bleats.get_visible_aliases().collect::<Vec<_>>(), ["logs"]);

        let stock = cmd.find_subcommand("stock").unwrap();
        assert_eq!(stock.get_visible_aliases().collect::<Vec<_>>(), ["scale"]);

        let whisper = cmd.find_subcommand("whisper").unwrap();
        assert_eq!(
            whisper.get_visible_aliases().collect::<Vec<_>>(),
            ["sendline"]
        );

        let lookout = cmd.find_subcommand("lookout").unwrap();
        assert_eq!(lookout.get_visible_aliases().collect::<Vec<_>>(), ["dash"]);

        for hidden in ["thatlldo", "daemon", "dog"] {
            assert!(
                cmd.find_subcommand(hidden).unwrap().is_hide_set(),
                "{hidden} must stay hidden from --help"
            );
        }
        for visible in [
            "start",
            "flock",
            "bleats",
            "lookout",
            "reload",
            "reopen",
            "flush",
            "barks",
            "trigger",
            "stock",
            "whisper",
            "enable",
            "disable",
            "adopt",
            "rehome",
            "ping",
            "kill",
            "save",
            "muster",
            "startup",
            "unstartup",
            "completions",
        ] {
            assert!(
                !cmd.find_subcommand(visible).unwrap().is_hide_set(),
                "{visible} must stay visible in --help"
            );
        }
    }

    /// Pins the spelling spec §9 gives — `sendline`, one word, never
    /// `send-line` — now carried as a literal `visible_alias` rather than a
    /// kebab-cased derive, so nothing wires up the two-word spelling by
    /// accident.
    #[test]
    fn sendline_is_spelled_one_word() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "sendline", "web", "gc"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "send-line", "web", "gc"]).is_err());
    }

    /// Precedent: [`logs_reaches_bleats`]. Fails if `sendline` stops being a
    /// visible alias for `whisper`, or if either spelling stops reaching
    /// [`Commands::Whisper`].
    #[test]
    fn sendline_reaches_whisper() {
        use clap::Parser;
        for argv in [
            ["shep", "whisper", "web", "gc"],
            ["shep", "sendline", "web", "gc"],
        ] {
            assert!(matches!(
                Cli::try_parse_from(argv).unwrap().command,
                Commands::Whisper(_)
            ));
        }
    }

    /// fails if `dash` stops reaching the same verb as `lookout`.
    ///
    /// **A resolution claim, and only that.** `try_parse_from` answers
    /// identically whether the attribute is `visible_alias` or the hidden
    /// `alias`, so this test cannot see the difference and must not claim to:
    /// the visibility pin belongs in
    /// `alias_visibility_and_hiding_are_pinned`, which already owns that job
    /// for `flock`/`bleats`/`stock`/`whisper` and is extended below.
    #[test]
    fn dash_and_lookout_resolve_to_the_same_verb() {
        use clap::Parser;
        assert!(matches!(
            Cli::try_parse_from(["shep", "dash"]).unwrap().command,
            Commands::Lookout(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["shep", "lookout"]).unwrap().command,
            Commands::Lookout(_)
        ));
    }

    /// fails if `shep whistle` stops parsing, or grows an argument. The
    /// absence of `--allow-control` is a decision (spec §14.7), so it is
    /// asserted rather than left to be noticed.
    #[test]
    fn whistle_takes_no_arguments_and_has_no_control_flag() {
        use clap::Parser;
        assert!(matches!(
            Cli::try_parse_from(["shep", "whistle"]).unwrap().command,
            Commands::Whistle
        ));
        assert!(
            Cli::try_parse_from(["shep", "whistle", "--allow-control"]).is_err(),
            "whistle's gate is `[whistle] allow_control` in shep.toml, and a flag would \
             let an agent host's own config open it in the same line that adds the server"
        );
    }
}
