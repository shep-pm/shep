# shep-tree

Signal a process group, own a Windows job and stop a child and everything it
started. It is for a process and what it started, so a shepherd, a dog or any
other program can use it. It depends on no other shep crate and on no async
runtime.

```rust,no_run
use std::process::Command;
use std::time::Duration;

use shep_tree::{Signal, Tree};

fn main() -> std::io::Result<()> {
    let (tree, mut child) = Tree::spawn(Command::new("sleep").arg("600"))?;
    // SIGTERM to the whole tree, up to 5 seconds to exit, then a kill.
    let status = tree.stop(&mut child, Signal::Term, Duration::from_secs(5))?;
    println!("{status}");
    Ok(())
}
```

- `Tree::spawn` puts the child in a process group of its own (unix) or a job
  object (Windows).
- `Tree::stop` signals the tree, polls for the child to exit within the grace,
  then kills the whole tree, so a descendant that ignored the signal does not
  outlive its leader.
- `group::signal_group` (unix) sends one `Signal` to a whole process group. It
  refuses pids 0 and 1, which `kill(2)` would read as the caller's own group
  and every process.
- `job::Job` (Windows) is the job object itself: `create`, `assign` and
  `terminate`.
- A unix group is left by a descendant that calls `setsid`. A Windows job
  cannot be left.

## Sweeping what a process left running

A group signal misses a child that left the group, and a process that exited
by itself signals nothing at all. `sweep::Sweeper` ends what is left: it
signals the pids a `sweep::LambSnapshot` saw, and whatever those start while
it waits. `sweep::ProcessTable` is the real process table behind it, and it
answers only for a root that descends from the calling process, so a sweep
never signals a tree it did not start.

```rust,no_run
use std::process::Command;
use std::time::Duration;

use shep_tree::sweep::{LambSweep, ProcessTable, Sweeper};
use shep_tree::{Signal, Tree};

fn main() -> std::io::Result<()> {
    let (tree, mut child) = Tree::spawn(Command::new("sleep").arg("600"))?;
    let table = ProcessTable::new();
    // Read the lambs while their root is still alive to walk from.
    let snapshot = table.snapshot(child.id());
    tree.stop(&mut child, Signal::Term, Duration::from_secs(5))?;
    let report = Sweeper::new(snapshot, Duration::from_secs(5)).run_blocking(&table);
    println!("termed {:?}, killed {:?}", report.termed, report.killed);
    Ok(())
}
```

- `Sweeper::run_blocking` drives the sweep on a plain thread, sleeping between
  looks.
- `Sweeper::step` is the same sweep a look at a time: it answers with the
  `Duration` to sleep before the next look, or with the finished
  `SweepReport`, so a caller on an async runtime sleeps with its own timer.
- `SweepReport` names every pid it termed and killed, every signal the OS
  refused with its reason, and every lamb still running after `SIGKILL`.
- `instant::ProcInstant` dates a snapshot, and `instant::read_pids` answers
  whether a pid is still the process a snapshot saw.
- The `sysinfo` feature is on by default and carries `ProcessTable` and the
  process-table reads. Off, the crate pulls no process-table dependency at all.

shep is pre-release. Anything public here can change before 1.0.

## License

MIT OR Apache-2.0, at your option.
