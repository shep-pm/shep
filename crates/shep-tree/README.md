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

shep is pre-release. Anything public here can change before 1.0.

## License

MIT OR Apache-2.0, at your option.
