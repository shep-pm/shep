# Docs moves Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move four blocks of prose to the chapters they belong in, and write the two chapters that receive them, so that every page becomes self-contained and the rewrite phase can run one agent per page with nothing crossing a file boundary.

**Architecture:** Relocation, not rewriting. Prose moves verbatim apart from the joins at each end: a lede on a new page, a handoff sentence where a block left. The corpus total therefore barely moves, and each page's count shifts by a number this plan states in advance. A move that loses a paragraph shows up as arithmetic that does not add up.

**Tech Stack:** Astro 7 (static, no MDX), plain CSS, Node's built-in test runner. No new dependencies.

**Spec:** `docs/brainstorming/specs/2026-09-14-docs-verbosity-structure-design.md`

**Previous plans:** `2026-09-14-docs-shell.md` and `2026-09-14-docs-book.md`, both complete.

## Global Constraints

- **Run every command from `web/`.** The worktree root has no `package.json`.
- **No new npm dependencies.**
- **`npm run build` and `npx astro check`, every time.** The build runs four check scripts. `astro build` does not typecheck, and a component handed a prop it does not have builds clean and renders wrong.
- **Prose moves, it does not get rewritten.** Copy the block across whole. The only new sentences in this phase are each new page's lede, each handoff line, and the joins listed per task. If a diff reworks a paragraph, that belongs to the rewrite phase and is out of scope here.
- **The corpus total is the check.** It is 44,559 today. After this plan it should be within about 60 words of that, all of it accounted for by ledes and handoffs. A larger drop means prose was lost.
- **Two new pages need a page each.** `upgrading` and `writing-a-dog` are already in `docs-nav.ts` as `built: false` at chapters 10 and 18. Writing the page and flipping the flag happen in the same commit, or `verify-docs-nav.ts` fails: it refuses an unbuilt chapter that has grown a page, and a built chapter with no page.
- **Every heading on a new page needs a unique `id`**, and each new page joins `ENFORCED` in `verify-heading-anchors.ts` in the commit that creates it.
- **New pages pass `description` and `activeSlug` only.** `DocsLayout` derives the title and the crumb from the nav. Passing `title` or `crumb` is a type error.
- **Conventional commit subjects**, `type(scope): summary`. Accepted here: `feat`, `fix`, `refactor`, `docs`, `chore`.

## What moves

| block | from | to | prose words |
| --- | --- | --- | --- |
| `<h3>Upgrading later</h3>` and everything under it | `getting-started` 66-131 | new `upgrading` | 496 |
| The runbook's boot half, plus Rolling back | `from-pm2` 522-559 | `startup` | 157 |
| `Writing your own` through the end of `Answering --schema` | `dogs` 427-1010 | new `writing-a-dog` | 4,008 |
| `A Flockfile is a template, not live config` | `first-flockfile` 171-205 | `overrides` | 259 |

The two-field Flockfile minimum does **not** move. `getting-started` already carries it at line 134, and `first-flockfile`'s own copy is a duplicate the rewrite phase drops. The spec describes the end state there, not a relocation.

## Expected counts

| page | now | after | why |
| --- | --- | --- | --- |
| `getting-started` | 1,244 | ~755 | minus 496, plus a handoff line |
| `upgrading` | none | ~540 | 496 plus a lede |
| `from-pm2` | 1,952 | ~1,810 | minus 157, plus a handoff line |
| `startup` | 1,284 | ~1,450 | plus 157, plus a join |
| `dogs` | 6,833 | ~2,850 | minus 4,008, plus a handoff line |
| `writing-a-dog` | none | ~4,060 | 4,008 plus a lede |
| `first-flockfile` | 2,563 | ~2,315 | minus 259, plus a pointer |
| `overrides` | 3,164 | ~3,435 | plus 259, plus a join |

---

### Task 1: The upgrading chapter

`getting-started` opens with `cargo install shep`, then spends 496 words on daemon reloads, dogs crossing a handover, log-pump failures, the 0.1.17 upgrade path and the protocol version having moved twice. All of it is correct and none of it belongs in front of someone who has not yet installed anything. It is 2.2 times the length of that page's entire happy path.

**Files:**
- Create: `web/src/pages/docs/upgrading.astro`
- Modify: `web/src/pages/docs/getting-started.astro` (remove lines 66-131, add a handoff)
- Modify: `web/src/data/docs-nav.ts` (`upgrading` becomes `built: true`)
- Modify: `web/scripts/verify-heading-anchors.ts` (`ENFORCED` gains `upgrading`)

**Interfaces:**
- Consumes: `DocsLayout`, which takes `description` and `activeSlug` only.
- Produces: `/docs/upgrading`, chapter 10, with anchors `#replacing-the-binary`, `#what-carries-across`, `#when-it-cannot-carry`, `#version-skew`.

- [x] **Step 1: Record the before state**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E 'getting-started|TOTAL'
```

Expected: `1244  getting-started` and `44559  TOTAL across 25 pages`. Keep both numbers; Step 7 checks against them.

- [x] **Step 2: Create the page with the block moved into it**

Create `web/src/pages/docs/upgrading.astro`. Take `getting-started.astro` lines 66 to 131 verbatim: the `<h3>Upgrading later</h3>` heading, its seven paragraphs, the `<div class="terminal">` block showing `cargo install` and `shep daemon reload`, the `<Callout variant="careful">` about version skew, and the `<p class="fine">` about the older reload path and protocol version 5.

Promote the `h3` and the paragraph groupings under it into four `h2` sections, splitting where the existing prose already changes subject. Do not rewrite the paragraphs; only the heading level and the section boundaries change.

```astro
---
/*
 * Upgrading (chapter 10). The whole of this page was an <h3> called
 * "Upgrading later" inside step 1 of Getting started until 2026-09-14,
 * where it sat between `cargo install shep` and writing a Flockfile: 496
 * words on daemon reloads, handover and protocol versions, in front of a
 * reader who had not yet installed anything. It is 2.2 times the length of
 * that page's entire happy path.
 *
 * Nothing here is rewritten, only moved and given its own headings.
 */
import DocsLayout from "../../layouts/DocsLayout.astro";
import ReferencePills from "../../components/docs/ReferencePills.astro";
import Callout from "../../components/docs/Callout.astro";
---

<DocsLayout
  description="cargo install replaces the binary on disk and changes nothing that is running. shep daemon reload is what moves a live flock onto it."
  activeSlug="upgrading"
>
  <article>
    <h1>Upgrading</h1>
    <ReferencePills slug="upgrading" />
    <p class="lede">
      <code>cargo install shep</code> replaces the binary on disk and
      changes nothing that is already running. <code>shep daemon reload</code>
      is what moves a live flock onto it.
    </p>

    <h2 id="replacing-the-binary">Replacing the binary</h2>
    ...
    <h2 id="what-carries-across">What carries across</h2>
    ...
    <h2 id="when-it-cannot-carry">When it cannot carry</h2>
    ...
    <h2 id="version-skew">Version skew</h2>
    ...
  </article>
</DocsLayout>
```

The lede is new. Everything under the four headings is the moved prose.

- [x] **Step 3: Cut the block from getting-started and leave a handoff**

Delete lines 66 to 131 of `web/src/pages/docs/getting-started.astro`, the whole `<h3>Upgrading later</h3>` section up to but not including `<h2>2. Write a Flockfile</h2>`.

In its place, one sentence:

```astro
    <p class="fine">
      Upgrading later is its own thing, and it is not part of installing:
      a new binary on disk changes nothing that is already running. See{" "}
      <a href="/docs/upgrading">Upgrading</a> when you get there.
    </p>
```

- [x] **Step 4: Flip the nav entry**

In `web/src/data/docs-nav.ts`, change `upgrading`'s `built: false` to `built: true`.

- [x] **Step 5: Enforce its anchors**

In `web/scripts/verify-heading-anchors.ts`, add `"upgrading"` to `ENFORCED`.

- [x] **Step 6: Build**

```bash
npm run build
```

```bash
npx astro check
```

Expected: both clean. `verify-docs-nav.ts` passes only if Steps 2 and 4 happened together.

- [x] **Step 7: Check the arithmetic**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E 'getting-started|upgrading|TOTAL'
```

Expected: `getting-started` near 755, `upgrading` near 540, and the total within about 40 words of 44,559. A total that dropped by hundreds means prose was cut rather than moved.

- [x] **Step 8: Look at both pages**

Load `http://localhost:4421/docs/getting-started` and confirm step 1 now runs install then straight into step 2, with the handoff line and no upgrade material.

Load `http://localhost:4421/docs/upgrading` and confirm the four sections render, the sidebar shows chapter 10 as a link rather than `soon`, the chapter bar reads `Previous 9. Talking to a sheep` and `Next 11. The lookout`, and each heading has a hover anchor.

- [x] **Step 9: Commit**

```bash
git add web/src/pages/docs/upgrading.astro web/src/pages/docs/getting-started.astro web/src/data/docs-nav.ts web/scripts/verify-heading-anchors.ts
git commit -m "feat(web): upgrading becomes its own chapter"
```

---

### Task 2: The boot half of the pm2 runbook moves to Surviving a reboot

The migration page's runbook is two runbooks. Steps 1 to 6 need nothing but pm2 knowledge. Steps 7 to 11 need sudo, systemd, unit naming and the meaning of `Type=notify`, and they are followed by a Rolling back section about `shep unstartup`. The reboot chapter, which is where a reader would look for all of that, has no runbook at all.

**Files:**
- Modify: `web/src/pages/docs/from-pm2.astro` (runbook shortens, Rolling back leaves, handoff added)
- Modify: `web/src/pages/docs/startup.astro` (gains the boot runbook and Rolling back)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing other tasks read.

- [x] **Step 1: Shorten the runbook on the migration page**

In `web/src/pages/docs/from-pm2.astro`, the `<h2>The runbook</h2>` `CodeBlock` currently lists eleven steps. Keep the first six and drop steps 7 to 11. Drop `--user <you>` from nothing here, because it does not appear in steps 1 to 6.

```
1.  shep import pm2 --dry-run       # read the Flockfile before it is written
2.  shep import pm2                 # writes ./Flockfile.toml; starts nothing
3.  pm2 delete all && pm2 kill      # the one destructive step, and it is pm2's
4.  shep start ./Flockfile.toml     # the flock comes up under shep
5.  shep flock                      # every app online, CPU and MEM populated
6.  shep save                       # names the roll it wrote and the app count
```

Update the paragraph above it, which currently reads "Import, save, install the boot unit, then reboot and check the flock came back". It now describes six steps ending at a saved flock. Keep its existing note that step 3 is the only step touching pm2 and the only irreversible one.

- [x] **Step 2: Add the handoff**

Immediately after the shortened `CodeBlock`, before the next heading:

```astro
    <Callout variant="note">
      The flock is running and its roll is saved, but nothing yet starts it
      after a reboot. That is <code>pm2 startup</code>'s counterpart and it
      lives on its own page:{" "}
      <a href="/docs/startup">Surviving a reboot</a>.
    </Callout>
```

This sentence is the reason the block is allowed to leave. Without it the migration reads as finished at step 6, and a reader loses their flock at the next reboot.

- [x] **Step 3: Move the Type=notify callout and Rolling back out**

Delete from `from-pm2.astro` the `<Callout variant="note">` explaining what `active (running)` means at step 8, and the whole `<h2>Rolling back</h2>` section. Both move to `startup.astro` in the next step, verbatim.

- [x] **Step 4: Give Surviving a reboot the runbook**

In `web/src/pages/docs/startup.astro`, add a new section as the page's second `h2`, after its opening material and before `Never escalates its own privilege`:

```astro
    <h2 id="the-runbook">The runbook</h2>
    <p>
      From a flock that is running and saved to one that comes back on its
      own. <code>shep save</code> is the hinge and appears in the migration
      runbook too: a roll has to exist before there is anything to restore.
    </p>
    <CodeBlock>{`1. shep save                     # if you have not already
2. sudo shep startup             # writes and enables the unit
3. systemctl status shep-<you>   # active (running), and green
4. reboot
5. systemctl status shep-<you>   # active (running) WITHOUT anyone logging in
6. shep flock                    # the same apps, new pids, uptime near zero`}</CodeBlock>
```

Then the moved `Type=notify` callout, then the moved `Rolling back` section as an `h2` with `id="rolling-back"`.

**`sudo shep startup` takes no `--user`.** The old runbook wrote `sudo shep startup --user <you>`, which teaches a flag nobody needs: `StartupArgs::user` defaults to `$SUDO_USER` and falls back to the invoking user, read in `crates/shep-cli/src/commands/startup/mod.rs` and covered by a test in `crates/shep-cli/tests/cli_e2e.rs`. Dropping it removes the one token in the sequence that implies the reader must understand Linux users.

- [x] **Step 5: Give both pages' new headings ids**

`startup.astro`'s existing headings have no ids. Add one to every `h2` and `h3` on the page, not only the new ones, and add `"startup"` to `ENFORCED` in `web/scripts/verify-heading-anchors.ts`.

- [x] **Step 6: Build**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 7: Check the arithmetic**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E 'from-pm2|startup|TOTAL'
```

Expected: `from-pm2` near 1,810, `startup` near 1,450, total within about 60 of 44,559.

- [x] **Step 8: Look at both pages**

On `/docs/from-pm2`, confirm the runbook is six steps and the callout points at the reboot page. On `/docs/startup`, confirm the runbook renders, `--user` appears nowhere, and every heading has a hover anchor.

- [x] **Step 9: Commit**

```bash
git add web/src/pages/docs/from-pm2.astro web/src/pages/docs/startup.astro web/scripts/verify-heading-anchors.ts
git commit -m "refactor(web): the pm2 runbook ends at the save, and reboots get their own"
```

---

### Task 3: The writing-a-dog chapter

`dogs` is 6,833 prose words, the longest page on the site. 4,008 of them, 59 percent, are about writing a dog: the plugin protocol, answering `--version`, answering `--schema`, and what a dog binary must do. An operator enables a dog and never reads any of it, and dog authors were not chosen as an audience for these docs, so the material stays but stops sitting in the middle of the page operators do read.

**Files:**
- Create: `web/src/pages/docs/writing-a-dog.astro`
- Modify: `web/src/pages/docs/dogs.astro` (remove lines 427-1010, add a handoff)
- Modify: `web/src/data/docs-nav.ts` (`writing-a-dog` becomes `built: true`)
- Modify: `web/scripts/verify-heading-anchors.ts` (`ENFORCED` gains `writing-a-dog`)

**Interfaces:**
- Consumes: `DocsLayout`, `ReferencePills`, and whichever of `Callout`, `CodeBlock` and `VerbSignature` the moved sections already use.
- Produces: `/docs/writing-a-dog`, chapter 18.

- [x] **Step 1: Record the before state**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E '\bdogs\b|TOTAL'
```

Expected: `6833  dogs`.

- [x] **Step 2: Create the page**

Create `web/src/pages/docs/writing-a-dog.astro` holding `dogs.astro` lines 427 to 1010 verbatim: `<h2>Writing your own</h2>`, `<h2>Answering <code>--version</code></h2>` with its five `h3` subsections, and `<h2>Answering <code>--schema</code></h2>`.

Keep every heading's text. Give each a unique `id`. Copy the import list from `dogs.astro` and drop whatever the moved sections do not use; `astro check` will not catch an unused import, but a missing one fails the build.

The page needs an `h1`, a `ReferencePills` and a new lede. Everything else is moved prose.

- [x] **Step 3: Cut from dogs and leave a handoff**

Delete lines 427 to 1010 from `web/src/pages/docs/dogs.astro`. In their place:

```astro
    <h2 id="writing-your-own">Writing your own</h2>
    <p>
      A dog is an ordinary binary that answers a few questions on stdout and
      speaks the dog protocol on a socket the shepherd hands it. The whole
      contract, including what <code>--version</code> and{" "}
      <code>--schema</code> have to answer and why the binary is the only
      thing that can answer them, is its own chapter:{" "}
      <a href="/docs/writing-a-dog">Writing a dog</a>.
    </p>
```

- [x] **Step 4: Flip the nav entry and enforce anchors**

`writing-a-dog` becomes `built: true` in `web/src/data/docs-nav.ts`, and `"writing-a-dog"` joins `ENFORCED` in `web/scripts/verify-heading-anchors.ts`.

- [x] **Step 5: Build**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 6: Check the arithmetic**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E '\bdogs\b|writing-a-dog|TOTAL'
```

Expected: `dogs` near 2,850, `writing-a-dog` near 4,060, total within about 40 of 44,559.

- [x] **Step 7: Look at both pages**

On `/docs/dogs`, confirm the page now runs from what a dog is, through turning one on and the built-in dogs, to the handoff, and that nothing referenced below the cut is now dangling. Read the sections either side of the join specifically: a paragraph that said "as described above" may now point at nothing.

On `/docs/writing-a-dog`, confirm every code block and callout survived, the sidebar shows chapter 18 as a link, and the chapter bar reads `Previous 17. Dogs` and `Next 19. Community dogs`.

- [x] **Step 8: Commit**

```bash
git add web/src/pages/docs/writing-a-dog.astro web/src/pages/docs/dogs.astro web/src/data/docs-nav.ts web/scripts/verify-heading-anchors.ts
git commit -m "feat(web): writing a dog becomes its own chapter"
```

---

### Task 4: The Flockfile-is-a-template rule moves to Changing a setting

`first-flockfile` heading 2 of 12 is `A Flockfile is a template, not live config`, 259 words. It is the largest mental-model shift in the migration: a pm2 user edits `ecosystem.config.js`, and that file *is* the config in their head. On a page titled "Your first Flockfile" it reads as trivia about a file. On the page about changing a setting it is the rule.

**Files:**
- Modify: `web/src/pages/docs/first-flockfile.astro` (remove lines 171-205, add a pointer)
- Modify: `web/src/pages/docs/overrides.astro` (gains the section as its opening)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing other tasks read.

- [x] **Step 1: Move the section**

Take `first-flockfile.astro` lines 171 to 205 verbatim and place them in `web/src/pages/docs/overrides.astro` as its first `h2`, before the existing `Why a template and not just config`. The two overlap in subject, which is the point: read together they are the rule and its reasoning, and the rewrite phase will merge them. Do not merge them here.

Give the moved heading `id="a-flockfile-is-a-template"`.

- [x] **Step 2: Leave a pointer**

In `first-flockfile.astro`, in place of the removed section:

```astro
    <p class="fine">
      A Flockfile is a template your project commits, not live configuration.
      shep never writes to one, and an operator changing a setting on a
      running flock is not editing this file:{" "}
      <a href="/docs/overrides">Changing a setting</a> covers where those
      changes live.
    </p>
```

- [x] **Step 3: Build and check the arithmetic**

```bash
npm run build
```

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E 'first-flockfile|overrides|TOTAL'
```

Expected: `first-flockfile` near 2,315, `overrides` near 3,435, total within about 40 of 44,559.

- [x] **Step 4: Look at both pages**

Confirm `/docs/overrides` opens with the rule and reads into its existing first section without a jolt, and that `/docs/first-flockfile` still makes sense where the section was removed.

- [x] **Step 5: Commit**

```bash
git add web/src/pages/docs/first-flockfile.astro web/src/pages/docs/overrides.astro
git commit -m "refactor(web): the template rule moves to the page about changing a setting"
```

---

### Task 5: Check that the moved prose actually landed

Four blocks moved between six pages. The way this fails quietly is a block that was cut and never pasted, which reads as a successful cut and a smaller page.

**Files:**
- Modify: `web/scripts/verify-prose-budget.ts` (record the post-move counts as budgets)

**Interfaces:**
- Consumes: `BUDGETS`.
- Produces: budgets for the eight pages this plan touched, which the rewrite phase then tightens.

- [x] **Step 1: Confirm the corpus total**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep TOTAL
```

Expected: within about 60 words of 44,559, the difference being four ledes and four handoffs. If it dropped by more than that, a block was cut rather than moved. Find it by comparing each page against the "Expected counts" table above rather than by rereading diffs.

- [x] **Step 2: Confirm nothing points at a section that left**

```bash
cd web && grep -rn 'as described above\|see above\|below\|earlier on this page' src/pages/docs/dogs.astro src/pages/docs/getting-started.astro src/pages/docs/from-pm2.astro src/pages/docs/first-flockfile.astro
```

Read each hit. A cross-reference that used to mean "further down this page" may now mean nothing. Fix any that dangle, and only those.

- [x] **Step 3: Seed budgets at the post-move counts**

In `web/scripts/verify-prose-budget.ts`, add the eight pages with their measured counts rounded up slightly. These are not rewrite targets, they are a floor that stops a page growing back before the rewrite phase sets real numbers.

- [x] **Step 4: Build**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 5: Commit**

```bash
git add web/scripts/verify-prose-budget.ts
git commit -m "chore(web): budget the eight pages the moves touched"
```

---

## Done when

- `/docs/upgrading` and `/docs/writing-a-dog` exist, are `built: true`, appear as links in the sidebar at chapters 10 and 18, and every heading on each has a unique id.
- No chapter bar links to an unbuilt page, and `verify-docs-nav.ts` passes.
- The pm2 runbook is six steps and ends with a callout pointing at the reboot chapter.
- `/docs/startup` carries the boot runbook, and `--user` appears nowhere on the site's runbooks.
- `dogs` is near 2,850 prose words rather than 6,833.
- The corpus total is within about 60 words of 44,559, and every page's count matches the Expected counts table.
- No page references a section that has moved to another page.
- `npm run build` passes with four check scripts, and `npx astro check` is clean.

## What was learned doing it

**Three .astro parsing defects, all the same shape.** The prose counter
mis-read a tag with a quoted `<`, the anchor check counted a heading
mentioned inside a doc comment, and extracting a frontmatter const by
scanning to the next backtick truncated two of them because they contain
escaped backticks. Documentation is content about markup and code, so its
content keeps looking like the syntax being matched. Prefer an extractor
that respects escapes and strips comments, and expect a fourth.

**Moving markup means moving its frontmatter.** The dogs split failed on
`probeSnippet is not defined` because three template literals referenced
from the moved markup live in frontmatter. Checking the component imports
is not enough; check every binding the block names.

**The tolerance was guessed, not computed.** Two tasks predicted "within
about 40" and "within about 60" and the plan landed at +261, fully
accounted for by ledes, handoffs and headings. Every individual page came
within 55 words of its prediction, so the per-page numbers were sound and
only the corpus-level tolerance was invented. State a range only when it
is derived.

## What this plan deliberately does not do

Every page is still too long and still in its original order. That is the next phase, and it can now run one agent per page, because after this plan no page's content depends on another page's.
