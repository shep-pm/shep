# Docs Part I Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the five chapters of Part I so a pm2 user reaches a running flock without reading for thirty minutes, and give every page the same shape so the rest of the book can follow it.

**Architecture:** One shared component first, then one page per task. After the moves phase no page's content depends on another's, so tasks 3 to 6 touch one file each and can run in parallel. Task 2 creates the component they all use, so it blocks them.

**Tech Stack:** Astro 7 (static, no MDX), plain CSS, Node's built-in test runner. No new dependencies.

**Spec:** `docs/brainstorming/specs/2026-09-14-docs-verbosity-structure-design.md`

**Previous plans:** `2026-09-14-docs-shell.md`, `2026-09-14-docs-book.md`, `2026-09-14-docs-moves.md`, all complete.

## Global Constraints

- **Run every command from `web/`.** The worktree root has no `package.json`.
- **No new npm dependencies.**
- **`npm run build` and `npx astro check`, every time.** The build runs four check scripts.
- **Prose is not code.** Run the `humanizer` skill, then `rin-voice`, over every sentence written or reworked here, before committing. The measured tells: no em dashes anywhere, bold almost never, no three-item parallel lists, no closing paragraph restating what was just said, no "we" in public writing.
- **Cut by deleting, not by compressing.** A sentence that survives should read as it did. Turning three clear sentences into one dense one hits the word count and makes the page harder, which is the opposite of the point.
- **Nothing gets lost to hit a number.** If a page cannot reach its budget without dropping a caveat, a failure mode or a number, stop and say so rather than dropping it. The budget is a ceiling on the reader's time, not a licence to delete what they need.
- **Every H2 and H3 needs a unique `id`**, and every page rewritten here joins `ENFORCED` in `web/scripts/verify-heading-anchors.ts`.
- **Pages pass `description` and `activeSlug` only.** `DocsLayout` derives the title and crumb from `docs-nav.ts`.
- **Do not touch `docs-nav.ts` ordering, any slug, or any other page.** One task, one file, plus the two check scripts.
- **Conventional commit subjects**, `type(scope): summary`.

## The reader

Primary is the pm2 refugee: usually alone, usually on one VPS, who chose pm2 to stop thinking about process management. Assume no systemd fluency and no service-account conventions. Where admin knowledge is genuinely needed, give the exact command rather than the concept behind it.

Secondary is a developer who has never used pm2 and needs the reason a process manager exists before the mechanics.

## Targets

| chapter | now | target | the change |
| --- | --- | --- | --- |
| 1 Quickstart | 778 | 400 | the detours inside each numbered step move below the win |
| 2 Coming from pm2 | 1,865 | 1,600 | reorder: the runbook and the verb table come first |
| 3 Surviving a reboot | 1,423 | 1,200 | short version at the top, edge cases into disclosures |
| 4 Examples | 1,312 | 1,100 | a chooser at the top, the walkthroughs below |
| 5 The words | 177 | 177 | no prose change; shape and anchors only |

---

### Task 1: The short version, as a component

The page contract puts a short version above the fold on every page. It needs one component so twenty-seven pages cannot each invent their own, the way `h2` was invented twenty-four times.

**Files:**
- Create: `web/src/components/docs/ShortVersion.astro`
- Modify: `web/src/styles/docs-page.css`

**Interfaces:**
- Consumes: nothing.
- Produces: `<ShortVersion>`, wrapping whatever a page puts in its default slot, with an optional `label` prop defaulting to "The short version". Every later task imports it.

- [x] **Step 1: Create the component**

```astro
---
/*
 * The short version: the commands, or the answer, above the fold and
 * before any caveat.
 *
 * One component rather than one per page, because the same thing happened
 * to `h2` before this: twenty-four pages each declared it and it drifted
 * into nine variants. A reader learns to look for this box once.
 *
 * `label` exists for the few pages where "the short version" is the wrong
 * words, not so each page can name it whatever it likes.
 */
interface Props {
  label?: string;
}

const { label = "The short version" } = Astro.props;
---

<aside class="short-version">
  <div class="sv-label">{label}</div>
  <div class="sv-body"><slot /></div>
</aside>

<style>
  .short-version {
    border: 3px solid var(--line);
    border-radius: 16px;
    background: var(--paper-2);
    box-shadow: 5px 5px 0 var(--shadow);
    padding: 20px 22px 6px;
    margin: 0 0 34px;
  }

  .sv-label {
    font-family: "Space Mono", monospace;
    font-size: 10.5px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin-bottom: 14px;
  }
</style>
```

- [x] **Step 2: Let page chrome apply inside it**

The shared stylesheet scopes its rules to `.docs-shell main`, which already covers anything inside this component, so `p` and `code` inherit correctly. Add only the spacing fix a boxed block needs, to `web/src/styles/docs-page.css`:

```css
/* The short version box supplies its own bottom padding, so the last
   paragraph inside it must not add another 18px under itself. */
.docs-shell main .sv-body > :last-child {
  margin-bottom: 14px;
}
```

- [x] **Step 3: Check it renders before four tasks depend on it**

Put a `<ShortVersion>` on `terminology.astro` temporarily with two lines in it, run `npx astro dev --port 4421`, and confirm the box draws with its label, the text sits inside it, and the spacing under the last line matches the space above the first. Then revert that page.

- [x] **Step 4: Build**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 5: Commit**

```bash
git add web/src/components/docs/ShortVersion.astro web/src/styles/docs-page.css
git commit -m "feat(web): a short-version box for the top of every docs page"
```

---

### Task 2: Chapter 2, Coming from pm2

This is the page the beta feedback was about, and the moves phase did not fix it. The verb table is still heading 6 of 10 and the runbook is heading 9 of 10. A pm2 user meets importer internals, cluster-mode socket semantics, a field table and a benchmark table before either of the two things they came for.

Target 1,600 prose words from 1,865. Most of the win is order, not deletion.

**Files:**
- Modify: `web/src/pages/docs/from-pm2.astro`
- Modify: `web/scripts/verify-heading-anchors.ts`, `web/scripts/verify-prose-budget.ts`

**Interfaces:**
- Consumes: `ShortVersion` from Task 1.
- Produces: nothing other tasks read.

- [x] **Step 1: Reorder into this sequence**

1. Lede, unchanged in substance.
2. `<ShortVersion>`: the six-step runbook, as it stands, and nothing else. No caveats inside the box.
3. `The verb-by-verb table` (`id="verb-table"`), moved up from position 6. This is the section a reader returns to for months, so it is the one that most needs to be findable and linkable.
4. `Side by side` (`id="side-by-side"`), NEW. One app as `ecosystem.config.js` and the same app as `Flockfile.toml`, adjacent. The page currently shows what the importer writes but never the pm2 input beside it, so a reader cannot see their own file translated. Build it from the existing `dump.pm2.json` fixture the page already documents; do not invent an app.
5. `What doesn't survive the trip`, its three subsections kept, each moved into a `<details>` after a one-line summary of what breaks. The summaries stay visible; the explanations fold away.
6. `Field by field`, unchanged.
7. `What it reads` and `Try it`, merged into one `How the import works` (`id="how-the-import-works"`). These are importer mechanics and belong below what the reader came for.
8. `What it costs to run`, unchanged, including the row where pm2 wins. A comparison that never concedes anything reads as marketing.
9. `shep serve vs. pm2 serve` and `pm2-runtime vs. shep runtime`, unchanged.
10. The handoff callout to `Surviving a reboot`, which currently sits under the runbook, moves to the end of the `ShortVersion`.

- [x] **Step 2: Fix the link text that names an old chapter**

This page says "Getting started" and "Your first Flockfile" in its cards and prose. Those chapters are now Quickstart and Flockfile reference. Change the text, not the href.

- [x] **Step 3: Add anchors and enforce them**

Every H2 and H3 gets a unique id. Add `"from-pm2"` to `ENFORCED`.

- [x] **Step 4: Run the voice skills**

`humanizer`, then `rin-voice`, over every sentence written or reworked. The new side-by-side section is the largest piece of new prose on this page.

- [x] **Step 5: Set the budget**

In `web/scripts/verify-prose-budget.ts`, change `"from-pm2"` from 1950 to 1600.

- [x] **Step 6: Build**

```bash
npm run build
```

```bash
npx astro check
```

Expected: both clean, and `from-pm2` at or under 1,600.

- [x] **Step 7: Read it as the reader**

Load the page and scroll once. The runbook must be visible without scrolling at a 768px viewport, and the verb table within one scroll. Check every `<details>` opens, and that the side-by-side renders as two columns on desktop and stacks below 560px.

- [x] **Step 8: Commit**

```bash
git add web/src/pages/docs/from-pm2.astro web/scripts/verify-heading-anchors.ts web/scripts/verify-prose-budget.ts
git commit -m "refactor(web): the pm2 page leads with the runbook and the verb table"
```

---

### Task 3: Chapter 1, Quickstart

778 words to reach a running flock. The happy path inside it is about 220: install, write two fields, start. Everything else is a detour taken before the reader has a win. The moves phase took out the largest one; what remains is build-from-source, shell completions, `shep welcome`, an interpreters table, and column conventions, each sitting inside the numbered step it interrupts.

Target 400 prose words.

**Files:**
- Modify: `web/src/pages/docs/getting-started.astro`
- Modify: `web/scripts/verify-heading-anchors.ts`, `web/scripts/verify-prose-budget.ts`

**Interfaces:**
- Consumes: `ShortVersion` from Task 1.
- Produces: nothing other tasks read.

- [x] **Step 1: Restructure**

1. Lede, one sentence, stating how long the page takes.
2. A one-line router as the very first thing after it: coming from pm2, start at chapter 2 instead, with the link. One line of scan for a greenfield reader, one signpost for a refugee, cheaper than a chooser page that taxes everyone with a click.
3. `<ShortVersion>`: install, the two-field Flockfile, `shep start`, `shep ls`. Three commands and one file. No caveats, no alternatives, no flags.
4. Then, below the win, in this order: `Other ways to install` (`id="other-installs"`, holding build-from-source and the Rust version note), `Scripts that need an interpreter` (`id="interpreters"`, unchanged), `Reading the flock table` (`id="the-flock-table"`, holding the column conventions), `Piping it somewhere` (`id="json"`).
5. Shell completions and `shep welcome` move into a single `<details>` under `Other ways to install`. Neither is needed to get a process running.
6. The pre-release and Windows callout moves below the short version. A reader who has not started yet cannot act on a Windows caveat.

- [x] **Step 2: Keep the aliases grid**

`shep bleats` / `shep logs` and the other three pairs are four lines and teach the whole naming scheme. Keep them under `Reading the flock table`.

- [x] **Step 3: Fix stale chapter names in link text**

This page points at "Terminology" and "Your first Flockfile". They are The words and Flockfile reference now.

- [x] **Step 4: Anchors, voice, budget**

Ids on every heading, `"getting-started"` into `ENFORCED`, `humanizer` then `rin-voice` over the new sentences, and the budget set to 400.

- [x] **Step 5: Build and read it**

```bash
npm run build
```

```bash
npx astro check
```

At a 768px viewport, the short version must be fully visible without scrolling. Time yourself reading from the top to the point where a flock is running; if it is more than about ninety seconds, it is still too long.

- [x] **Step 6: Commit**

```bash
git add web/src/pages/docs/getting-started.astro web/scripts/verify-heading-anchors.ts web/scripts/verify-prose-budget.ts
git commit -m "refactor(web): quickstart reaches a running flock in three commands"
```

---

### Task 4: Chapter 3, Surviving a reboot

The moves phase gave this page a runbook and put it first, which is most of the work. What it does not have is a short version, and it still opens six sections of systemd detail at a reader whose question is "will my apps come back".

Target 1,200 prose words from 1,423.

**Files:**
- Modify: `web/src/pages/docs/startup.astro`
- Modify: `web/scripts/verify-prose-budget.ts`

**Interfaces:**
- Consumes: `ShortVersion` from Task 1.
- Produces: nothing other tasks read.

- [x] **Step 1: Restructure**

1. Lede.
2. `<ShortVersion>`: the six-step runbook that is already there, moved into the box.
3. `Type=notify` callout stays directly under it, because it explains what step 3's green status proves.
4. `Rolling back`, unchanged.
5. `The muster roll`, unchanged. This is what a reader checks when the flock does not come back.
6. `Is it up, and stopping it on purpose`, unchanged.
7. Into `<details>`, each behind a one-line summary: `Never escalates its own privilege`, `The PATH capture, and its one trap`, `Honestly: openrc and the BSDs are untested`. All three matter and none is what a reader is asking on their first pass.

- [x] **Step 2: Keep the honesty**

The openrc and BSD section says those paths are untested. It goes into a disclosure, not out of the page. Its summary line must say "untested" so a reader on one of those systems sees it without opening anything.

- [x] **Step 3: Anchors, voice, budget**

This page already has ids on every heading and is already in `ENFORCED`. Any new heading needs one too. Budget to 1,200.

- [x] **Step 4: Build and read**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 5: Commit**

```bash
git add web/src/pages/docs/startup.astro web/scripts/verify-prose-budget.ts
git commit -m "refactor(web): the reboot chapter leads with its runbook"
```

---

### Task 5: Chapter 4, Examples

A reader whose app is not a plain binary comes here to find the one example that matches their stack. They currently read about the exec probe and `kill_timeout` first, and the five stack-specific walkthroughs are `h3`s inside the last section.

Target 1,100 prose words from 1,312.

**Files:**
- Modify: `web/src/pages/docs/examples.astro`
- Modify: `web/scripts/verify-heading-anchors.ts`, `web/scripts/verify-prose-budget.ts`

**Interfaces:**
- Consumes: `ShortVersion` from Task 1.
- Produces: nothing other tasks read.

- [x] **Step 1: Restructure**

1. Lede.
2. `<ShortVersion label="Pick yours">`: a list linking straight to each walkthrough's anchor, one line each, saying which stack it is for. Node and Bun in one file, several instances without `reuse_port`, a venv's own python, a build step then no interpreter, and a static directory rather than an app.
3. `Run it`, unchanged.
4. The five walkthroughs promoted from `h3` to `h2`, each with its own id, in the order the chooser lists them. They are what the page is for and they were nested two levels down.
5. `The exec probe, in full` and `kill_timeout, not graceful_timeout` move below the walkthroughs. Both are reference material a reader reaches after picking an example.
6. `What each one demonstrates` folds into the chooser rather than being its own section.

- [x] **Step 2: Anchors, voice, budget**

Every heading gets an id, and the chooser links to those ids, so it breaks loudly if one is renamed. `"examples"` into `ENFORCED`. Budget to 1,100.

- [x] **Step 3: Build and read**

```bash
npm run build
```

```bash
npx astro check
```

Click every link in the chooser and confirm each lands on its walkthrough.

- [x] **Step 4: Commit**

```bash
git add web/src/pages/docs/examples.astro web/scripts/verify-heading-anchors.ts web/scripts/verify-prose-budget.ts
git commit -m "refactor(web): examples opens with a chooser for your stack"
```

---

### Task 6: Chapter 5, The words

177 prose words around a data-driven lexicon table. There is nothing to cut. It needs the shape the other four now have, and its headings need ids.

This task is mechanical and is done in the main thread rather than dispatched.

**Files:**
- Modify: `web/src/pages/docs/terminology.astro`
- Modify: `web/scripts/verify-heading-anchors.ts`

- [x] **Step 1: Add the short version**

`<ShortVersion label="The five that matter">`: flock, fold, sheep, dog, bleats, one line each. A reader who learns those five can read every other chapter; the full table below is for when they meet a word that is not one of them.

- [x] **Step 2: Ids and enforcement**

Ids on `The lexicon`, `Sheepdogs and sheep were separate ideas from the start`, `Usage rules` and `Where to go next`. Add `"terminology"` to `ENFORCED`.

- [x] **Step 3: Build**

```bash
npm run build
```

```bash
npx astro check
```

- [x] **Step 4: Commit**

```bash
git add web/src/pages/docs/terminology.astro web/scripts/verify-heading-anchors.ts
git commit -m "feat(web): the words opens with the five that matter"
```

---

### Task 7: Check Part I as a whole

Five pages rewritten separately can each be right and still not read as a sequence.

**Files:**
- Modify: `web/scripts/verify-prose-budget.ts` if any budget needs correcting

- [x] **Step 1: Read all five in order**

Load chapters 1 to 5 and follow the chapter bar from one to the next, as a reader would. Check that nothing in chapter 2 assumes something only chapter 4 says, and that no page repeats an explanation another page already gave.

- [x] **Step 2: Confirm every page has the shape**

```bash
cd web && for p in getting-started from-pm2 startup examples terminology; do printf "%-18s short-version=%s headings-without-id=%s\n" "$p" "$(grep -c 'ShortVersion' src/pages/docs/$p.astro)" "$(grep -c '<h[23]>' src/pages/docs/$p.astro)"; done
```

Expected: every page uses `ShortVersion` at least once, and none has a heading without an id.

- [x] **Step 3: Confirm the budgets hold**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E 'getting-started|from-pm2|startup|examples|terminology|TOTAL'
```

Part I should total near 4,500 prose words, down from 5,555.

- [x] **Step 4: The DM test**

Take the last thing a beta tester was sent privately and find it in Part I in under thirty seconds. Anything that fails is a page bug with a name, and it gets recorded here rather than fixed silently.

- [x] **Step 5: Commit any budget corrections**

---

## Dispatch

Task 1 blocks everything. Task 6 and Task 7 run in the main thread.

Tasks 2 to 5 touch one page each plus the two check scripts, so they run in parallel with one conflict to manage: all four edit `verify-heading-anchors.ts` and `verify-prose-budget.ts`. The main thread makes those two edits after the four report, rather than four agents racing on the same two files.

**The divergence, stated:** the pm2 agent finds what a migrating user needs in what order; the quickstart agent finds what can be deferred from a first install; the reboot agent finds what a solo operator needs before trusting a machine restart; the examples agent finds which walkthrough matches which stack. Four different readers, four different pages, four different questions. Not four opinions on one.

`sonnet`, `effort: high` for all four. The design decisions are in this plan; the work is applying them to prose. Roughly 15 minutes of wall-clock against about 50 sequential.

**Agents cannot see a rendered page.** Every defect in the three completed plans was found by looking at one, and none by reading a diff. So each agent's report is a draft, and the main thread opens all five pages in the browser before any of it is called done.

## What the fan-out actually cost and caught

Four agents, sonnet at high effort, about 17 minutes each in parallel
against roughly 50 sequential. Every report was accurate. Two still needed
correcting, and both corrections came from opening the page rather than
from reading the report.

**The reboot agent put the ids on the `<details>` elements.** Its report
said "all green, every h2 still has a unique id", which was true because
three h2s had stopped being h2s. The page went from eight headings to
four, the anchor check stopped covering three sections while still
reporting green, and `/docs/startup#openrc-and-the-bsds` neither scrolled
nor opened. Fixed by putting each id back on an `<h2>` inside its
`<summary>`, which `<summary>` accepts by spec.

**The quickstart agent cut a paragraph to remove a stale link**, which
left the page with no route to the Flockfile reference at all, and its
Where-to-go-next cards offered Dogs. Swapped for Flockfile reference.

**One agent found a bug in the tooling and reported it instead of routing
around it.** The prose counter's terminal stripper ended at the first
nested `</div>`, so command lines and table output after the first were
counted as prose. The corpus was overstated by 913 words. It could have
restructured its markup to dodge the miscount and said nothing; it did not.

**And the fan-out surfaced a defect of mine that predated it.** Running
`astro check` through `tail -3` hides the error count whenever there are
errors, because the offending source is printed above the summary. Ten
ts(2339) and ts(2741) errors had been reported for four commits and I had
been reading "0 warnings / 0 hints" as success. Every field-group heading
on the Flockfile reference and every verb-group heading on the CLI page
was rendering empty. Never truncate the output of the command that exists
to tell you something is wrong.

## One spec criterion this plan does not meet

S3 in the spec says the verb table should be "one click from the sidebar,
not only from inside the migration narrative". Task 2 moves it to position
3 on its page and gives it a stable anchor, which covers bookmarking it and
pasting it into a message. It does not put it in the sidebar, because a
table is a section and the sidebar lists chapters.

Meeting it properly means making the verb table its own chapter, which
would be the twenty-eighth and would renumber everything after it. That is
a real option and it is deliberately not taken here: the table is most
useful beside the runbook a reader is working through, and a reader who
wants it alone can bookmark `/docs/from-pm2#verb-table`. Revisit if beta
readers keep asking where the verb list lives.

## Done when

- All five chapters open with a short version above the fold.
- The pm2 page's runbook is visible without scrolling at 768px, and the verb table is one scroll away and has a linkable anchor.
- The pm2 page shows a pm2 config and its Flockfile equivalent side by side.
- Quickstart reaches a running flock in three commands and about 400 words.
- Every heading in Part I has a unique id, and all five pages are in `ENFORCED`.
- No page names a chapter by a title it no longer has.
- Part I totals near 4,500 prose words, from 5,555.
- `npm run build` passes with four check scripts, and `npx astro check` is clean.
