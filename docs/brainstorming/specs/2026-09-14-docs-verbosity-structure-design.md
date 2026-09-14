# Docs: reader paths, book order, page shape

Beta testers say the docs site is too verbose, the pm2 migration page
included. One of them runs roughly ten times the production this project's
maintainer does, and reads carefully rather than skimming. Two things he
said, paraphrased:

- Half an hour of reading before getting anywhere, and the writing reads
  like an attempt to conquer the world rather than to get him started.
- What he wanted instead: a quick setup guide. Install, a config example,
  start it, and the small satisfaction of having something running. Details
  afterwards, for whoever wants to read up.

The second one is a specification. He is not asking for fewer words, since
details afterwards keeps the depth. He is asking for a fast route to a
working flock, with everything else behind it.

That distinction governs this whole document. Deleting depth would fail him
as badly as the current pages do.

## The signal is buried, not missing

Measured 2026-09-14 against the site at commit 99c0da73.

44,591 words of prose across 25 pages, excluding code blocks, styles and
frontmatter.

**Getting started.** The path a reader came for, meaning install, write a
Flockfile, start it, is 221 words.
Everything wrapped around it is 891. The largest single block is a 496-word
`h3` called "Upgrading later", sitting inside step 1, covering daemon
reloads, dogs crossing a handover, log-pump failures, the 0.1.17 upgrade
path and the protocol version having moved twice. A reader meets it before
typing `cargo install`.

Every one of those paragraphs is correct and worth keeping. That is what
makes the fault hard to spot from the inside: reviewing any one of them,
you would keep it. The defect only shows up when you measure position.

**The pm2 page buries what a pm2 user came for.** The verb-by-verb table is
heading 11 of 16. The runbook is heading 14. Readers meet importer
internals, cluster-mode socket semantics and a benchmark table first.

**Headings are not addressable.** 13 of 208 headings carry an `id`, across
five pages. No section can be linked, bookmarked or pasted into a message.

**The sidebar cannot scroll.** `DocsSidebar.astro` sets `position: sticky`
with `min-height: calc(100vh - 64px)` and no `overflow`. Measured live: the
element is 1492px against a 768px viewport, `overflow-y: visible`, so it
cannot scroll independently of the page.

**Section spacing is inconsistent because it is defined 24 times.** `h2`
is declared in 24 separate page `<style>` blocks and has drifted into nine
variants, from 26px/44/14 to 29px/40/18. `output.astro` declares none, and
global.css sets `h1, h2, h3 { margin: 0 }`, so every section heading on
`/docs/output` renders at browser-default 24px with no gap above it.
Confirmed in a browser, not inferred: `astro build` and `astro check` are
both green on that page.

## Three readers, and one of them is not a person

**Primary: the pm2 refugee.** Already runs pm2, usually alone, usually on
one VPS. Chose pm2 to stop thinking about process management. Assume no
systemd fluency, no service-account conventions, no `journalctl`. Where
admin knowledge is genuinely required, give the exact command rather than
the concept behind it.

**Secondary: the greenfield developer.** Never used pm2. Needs the reason a
process manager exists before the mechanics.

**Secondary: the agent operator.** A person driving shep from an AI agent.
Wants MCP, JSON and the channel grouped, and wants to skip everything
written for someone at a terminal.

**Machine readers** get `/llms.txt`. They are a delivery format, not a
persona.

Deliberately not written for: platform engineers evaluating shep against
systemd, and dog authors. Their material survives where it also serves
someone above.

## User stories

### S1 - Decide whether to switch
As someone already running pm2 in production, I want to know within two
minutes whether shep is worth my afternoon, so I do not read a manual for a
tool I will reject.

1. The switching case (what is better, what is worse, what breaks) is
   reachable without scrolling past install instructions.
2. The cost numbers are one click from the landing page.
3. What pm2 does that shep does not is stated in the same place.

### S2 - Migrate an existing setup
As a pm2 user with a live `dump.pm2`, I want my apps running under shep, so
I can judge it against real work rather than a toy.

1. The runbook appears above the fold on the migration page.
2. Rolling back is named on the same screen as the migration.
3. Cluster sockets and inherited environment are flagged at the top and
   explained below, not both at once.
4. The runbook ends at a running, saved flock, and says in one line that
   reboot survival is a separate step, with a link.

### S3 - Translate a verb
As a pm2 user mid-task, I want to look up the shep word for `pm2 reload`,
so I need not re-read a page I have already read.

1. The verb table has a stable anchor that can be bookmarked and pasted.
2. It is one click from the sidebar, not only from inside the migration
   narrative.
3. Rows with a behavioural difference link to the page explaining it.

### S4 - First win
As a developer who has never used pm2, I want my app running and surviving
a crash, so I can feel the thing work before deciding to learn it.

1. Install, config and start is under 300 words with no detours.
2. Nothing on that path mentions daemon reloads, protocol versions or
   upgrade paths.
3. The page ends at the win.

### S5 - Take it to production
As a developer whose app runs locally, I want it to survive a reboot and
write logs somewhere sane, so I can put it on a VPS.

1. A named sequence runs from laptop to server-after-reboot.
2. It is one page, or an explicitly ordered run of pages.

### S6 - Wire an agent to the flock
As someone driving shep from an agent, I want the machine surfaces
grouped, so I can skip what is written for a human.

1. Whistle, JSON output and the channel sit together under one heading.
2. The channel's summary is first on its page, not seventh of eight.
3. Each page states its stability guarantee.

### S7 - Read the docs as an agent
As an agent asked about shep, I want a machine-readable index, so I fetch
one page instead of scraping the site.

1. `/llms.txt` lists every page with title, one-line summary and absolute
   URL.
2. It is generated from `docsNav.ts`.
3. The build fails when a nav entry has no summary.

None of the seven is met today. Six fail on ordering and addressing rather
than on missing content, which is the encouraging reading: the writing is
done, it is filed wrong.

## The book, ordered by when rather than by what

Seven parts, twenty-seven chapters, ordered by when a reader needs a thing
rather than by what kind of thing it is.

```
PART I - GET IT RUNNING
  1  Quickstart              /docs/getting-started   retitled
  2  Coming from pm2         /docs/from-pm2
  3  Surviving a reboot      /docs/startup           retitled
  4  Examples                /docs/examples
  5  The words               /docs/terminology       retitled

PART II - DAY TO DAY
  6  Reading logs            /docs/logs
  7  Changing a setting      /docs/overrides         retitled
  8  Stopping and replacing  /docs/lifecycle
  9  Talking to a sheep      /docs/talking-to-a-sheep
 10  Upgrading               /docs/upgrading         NEW
 11  The lookout             /docs/lookout
 12  Terminal output         /docs/output

PART III - CONFIGURATION
 13  Folds                   14  Boot order
 15  Secrets                 16  The KV store

PART IV - DOGS
 17  Dogs                    18  Writing a dog       NEW
 19  Community dogs

PART V - MACHINE SURFACES
 20  Whistle (MCP)           21  JSON output
 22  The shepherd channel

PART VI - OTHER PLACES IT RUNS
 23  Containers              24  Serve

PART VII - REFERENCE
 25  CLI                     26  Flockfile reference /docs/first-flockfile
 27  What's not built
```

Two pages are added. None is deleted, and no existing slug moves, so the
landing page's call to action, every inbound link and every message already
sent keep resolving. No redirects.

### Why each move happens

**Startup joins Part I.** A pm2 user's muscle memory is `pm2 start`, then
`pm2 startup && pm2 save`, then stop thinking. Reboot survival is not
deployment to them, it is step two. Part I now covers the whole solo
operator journey: from nothing to a flock that stays up.

**Terminology closes Part I as "The words".** Nobody reads a glossary as
chapter two, but `getting-started` already points at it first, with the
line "Learn it once and the CLI explains itself". It was filed under
Concepts; 176 words of prose, the highest leverage per word on the site.

**Concepts dissolves.** Eleven items grouped by what they are, redistributed
by when a reader needs them. Nothing is lost.

**Lookout and whistle separate.** They shared a group called Interfaces
because neither is the CLI, which groups them by what they are not. A terminal
dashboard is a daily tool; an MCP server is a machine surface.

**Quickstart carries the pm2 router on line one**: "Coming from pm2? Start
there instead." One line of scan for the greenfield reader, one signpost
for the refugee, cheaper than a chooser page that taxes everyone with a
click.

### The Flockfile reclassification

A Flockfile is a project template committed by a repo owner. Operator
tuning lives in `$SHEP_HOME/overrides.json`, with three doors into that
store: a Flockfile load spends an override, the command line sets one, and
a lookout pane sets one. Since `lookout/field.rs` builds its config form
from the Flockfile JSON Schema itself, every schema field is already
editable in the TUI, with nested objects the one read-only case.

So "Your first Flockfile" is a wrong title for the page under it. It splits
three ways:

- The two-field minimum moves into Quickstart, inline, about 40 words.
- The field reference, formats, discovery, templating and schema become
  chapter 26, Flockfile reference.
- "A Flockfile is a template, not live config" moves to chapter 7, Changing
  a setting, where it is aimed at the operator rather than the repo owner.

That last move matters more than it looks. A pm2 user edits
`ecosystem.config.js`; that file *is* the config in their head. The rule
about where their changes live is the largest mental-model shift in the
migration, and it currently appears as heading 2 of 12 on a page whose
title frames it as trivia about a file.

### The runbook splits at the save

The eleven-step runbook on the pm2 page is two runbooks. Steps 1 to 6 need
nothing but pm2 knowledge. Steps 7 to 11 need sudo, systemd, unit naming
and the meaning of `Type=notify`.

```
Coming from pm2 - six steps
  1  shep import pm2 --dry-run
  2  shep import pm2
  3  pm2 delete all && pm2 kill      the one destructive step, and it is pm2's
  4  shep start ./Flockfile.toml
  5  shep flock
  6  shep save
     -> "Running, but it will not survive a reboot yet."

Surviving a reboot - picks up from the saved roll
     sudo shep startup
     systemctl status shep-<you>
     reboot
     systemctl status shep-<you>
     shep flock
     plus the Type=notify explanation, which belongs here and only here
```

`shep save` appears in both. It is the hinge between the two, and a reader
who lands on either page needs it.

Above the runbook, the migration page gains a side-by-side comparison: one
app written as `ecosystem.config.js` and the same app written as
`Flockfile.toml`, adjacent. The page currently shows what the importer
writes but never the pm2 input beside it, so a reader cannot see their own
file translated. The field table further down stays, and answers a
different question: the comparison shows the shape, the table answers what
one field became.

Dropping boot survival from the migration page entirely was considered and
rejected. `pm2 startup` and `pm2 save` are in pm2's own quick start, so a
refugee's apps come back after a reboot today. Removing the subject without
a trace makes the migration read as finished at step 6, and the reader
loses everything a fortnight later. The handoff line is what prevents that.

`sudo shep startup --user <you>` in the current runbook is wrong to teach.
`StartupArgs::user` defaults to `$SUDO_USER` and falls back to the invoking
user; `startup/mod.rs` reads it and `cli_e2e.rs` tests it. `sudo shep
startup` is the whole command, and dropping the flag removes the one token
in the runbook that implies the reader must understand Linux users.

## Every page takes the same shape

### The contract

```
H1
lede                    one sentence: what you can do after this page
+---------------------------------------+
| THE SHORT VERSION                     |   the commands, or the answer
|                                       |   no caveats, no exceptions
+---------------------------------------+   above the fold, always
-----------------------------------------   visible rule
H2  depth section                           every heading carries an id
    > edge case                             <details>, closed by default
H2  depth section
-----------------------------------------
Next -> chapter N+1                         then related links
```

1. Nothing sits between the lede and the short version. No pre-release
   warning, no platform caveat.
2. No disclosure inside the short version.
3. Each numbered step in a short version ends with its own pointer, in the
   form "Full reference: X". A reader who needs depth at step 2 gets it at
   step 2 rather than at the bottom of the page.
4. "Where to go next" becomes "Next", and the real next chapter is first.
   Today `from-pm2` points at `getting-started`, which is backwards.
5. Chapter 1's lede states how long the page takes, so the contract the
   reader is being offered is written down.
6. Every page ends with the same escape hatch: a line offering the issue
   tracker to anyone the page did not answer. One component, rendered by
   `DocsLayout`, so no page can omit it. This is the part of the site that
   replaces a private message.

`shepherd-channel` already carries a section called "Summary for the
impatient" at position 7 of 8. The pattern is right and the position is
inverted; moving it to position 1 is the whole change on that page.

### Move, collapse, cut

Three operations, kept separate so this does not become deletion.

- **Move** when the material belongs to another chapter.
- **Collapse** when it belongs here but only some readers need it.
- **Cut** when something else that ships already says it.

Only cut loses words.

| Page | Prose now | Operation | Target |
| --- | --- | --- | --- |
| dogs | 6,838 | Move "Writing your own" and both "Answering" sections to chapter 18 | ~4,300 |
| lookout | 6,039 | Cut the keymap section, which v0.8.0's in-app overlay already shows; collapse the pane walkthrough | ~3,000 |
| overrides | 3,162 | Stays one page. Rule and three doors above, edge cases below | ~3,000 |
| from-pm2 | 1,952 | Move boot steps to chapter 3; verb table and runbook to the top | ~1,600 |
| getting-started | 1,252 | Move "Upgrading later" to chapter 10 | ~350 |

Every other page keeps its prose and gains the contract and its anchors.
Most pages are not the problem: `terminology` is 176 prose words and `cli`
is 355.

### Anchors

Hand-written `id` on every H2 and H3, following the precedent already on
`output`: `grouped-instances`, `the-dogs-table`. Hand-written rather than
derived, because a derived slug changes the moment a heading is reworded,
and S3 wants a link that survives being pasted into a message.

195 headings to add. Global CSS gives `h2[id]` a hover marker and a short
script in `DocsLayout` makes it clickable; deep links work without the
script. `scripts/check-heading-anchors.mjs` runs in `npm run build` and
fails when a heading has no id or a page has a duplicate.

### Spacing, and the stylesheet behind it

Section spacing cannot be tuned while `h2` is declared in 24 files in nine
variants. So the shared page chrome (`h1`, `h2`, `h3`, `.lede`, `.fine`,
`.next-grid`) moves into one stylesheet imported by `DocsLayout`. Pages
keep only what is genuinely theirs: the field tables, the alias grid, the
JSON block.

One scale, taken from the majority: `h2` at 29px, `margin: 48px 0 10px`.
The extra 8px above today's 40px is the white-space fix. `/docs/output`
gets styled for the first time.

This is a DRY change that would normally be deferred. It is not deferrable
here: better section breaks are unachievable across 24 divergent copies.

### Cross-references

1. Every page's Next points at its actual next chapter.
2. Every term from chapter 5 is linked on first use per page, once.
3. Verb-table rows with behavioural differences link to the page that
   explains them, replacing prose like "see below", which breaks the moment
   a section moves.

### The sidebar

```css
.docs-sidebar {
  position: sticky;
  top: 64px;
- min-height: calc(100vh - 64px);
+ max-height: calc(100vh - 64px);
+ overflow-y: auto;
+ overscroll-behavior: contain;
}
```

`min-height` to `max-height` is the fix; the other two lines stop the
sidebar handing its scroll back to the page at either end. The
`border-right` currently spans the full column and an overflow box ends at
the viewport, so if the rule visibly stops short it moves to the grid
container instead.

Parts render as labelled groups with numbered chapters, all open. A reader
sees the whole book and where they are in it, which is what a table of
contents is for. Collapsing all but the current part was considered and
rejected: it hides the shape, and the scroll fix removes the reason to
reach for it.

## The benchmark table is stale and has to be re-measured

`from-pm2`'s cost table was measured on 2026-08-29 against shep 0.1.12 and
pm2 7.0.4. The workspace is on 0.8.0. The numbers are presented with the
date and the versions attached, so nothing on the page is dishonest, but
they describe a build eight minor versions old and one of the rows moved
by a factor of fifteen in the days before it was taken.

Re-run the comparison against current shep and current pm2, and update the
table, the date and the versions. Keep the methodology prose as it stands.
It names exact versions, says to read the ratios rather than the absolute
figures, admits the box was not idle and changed power state partway
through, reports that the two shep rounds agreed within 2.6 percent, says
plainly that the idle CPU difference is not a result, and volunteers that
shep was eight times behind pm2 on log-plane cost the day before the run.
That is a stronger disclosure than the comparable page on any competitor
reviewed for this spec, and it is the reason the numbers can be trusted.

Automating the run in CI was considered and rejected for this pass. Shared
runners are noisy enough that a hand measurement on a known box, with the
drift check above, is the more honest artifact.

## llms.txt

`/llms.txt`, generated from `docsNav.ts`: one line per page with title,
one-line summary and absolute URL, grouped by part. A nav entry without a
summary fails the build.

Per-page `.md` and `/llms-full.txt` are deliberately deferred. The pages
are hand-written `.astro` with components, so serving markdown means either
converting all 27 to MDX or maintaining a second copy that drifts. Doing
the format migration inside the prose rewrite means two kinds of change in
one diff, and a format fault fails silently on a prop nobody can see --
the same shape as the `Callout kind=` against `variant=` bug that shipped
in August with a green build. It is also cheaper afterwards, because a
shorter page converts more easily.

## What has to be true before it ships

The site ships when:

1. Every acceptance criterion under S1 to S7 holds.
2. `npm run build` passes, including the new anchor check.
3. `npx astro check` is clean. It catches wrong props that `astro build`
   does not.
4. `web/scripts/generate-cli-reference.sh` has been re-run and its diff is
   empty or explained.
5. Every H2 and H3 on every page has a unique id.
5a. Every page renders the escape hatch, because `DocsLayout` renders it
    rather than each page.
5b. The migration page shows a pm2 config and its Flockfile equivalent
    side by side, above the runbook.
5c. The benchmark table names a date and versions no older than the
    release current when it ships.
6. `h2` is declared once, in the shared stylesheet.
7. The sidebar scrolls independently at a 768px viewport.
8. The DM test passes: take the last thing a beta tester was sent
   privately, and find it on the site in under thirty seconds. Anything
   that fails is a page bug with a name.

## What a competitor's docs site settled, and what it did not

`oxmgr.empellio.com/docs` was reviewed on 2026-09-14. It is another Rust
process manager aimed at the same pm2 user, so its docs answer the same
question this spec does, with different choices.

Three of its patterns are adopted above: per-step reference pointers, a
stated time promise, and an escape hatch on every page. A fourth, the
side-by-side config comparison, filled a real gap on the migration page.

Four were looked at and left:

- **A flat ten-item sidebar.** It works there because that site documents
  ten pages. shep documents twenty-seven, and a flat list of twenty-seven
  is the problem rather than the fix.
- **Quick Start living at `/docs` itself, ending in a grid of every
  chapter.** Tempting, and rejected to keep `/docs` a redirect and every
  existing slug in place.
- **A feature-comparison table with no row where pm2 wins.** shep's cost
  table has one, reported as a tie. It stays. A comparison that never
  concedes anything reads as marketing.
- **Benchmarks regenerated by CI on every push.** See the section above.

Two things that site does worse are worth naming, because they are easy to
assume a competitor got right. Not one heading on any page checked carries
an `id`, so nothing there can be linked to. There is no search of any kind.
This spec's anchor work and the site's existing Pagefind index both stand.

## Still open

- Whether chapter 4, Examples, stays in Part I. It is the fastest route to
  a win for anyone whose app is not a plain binary, and the largest page in
  a part meant to be quick. Decided for Part I; revisit if Part I reads
  heavy once the prose is cut.
