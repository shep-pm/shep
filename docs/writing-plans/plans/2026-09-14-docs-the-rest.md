# Docs Parts II to VII Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the remaining nineteen chapters the shape Part I now has, so a reader meets the same page everywhere and every section on the site can be linked to.

**Architecture:** Five buckets, one agent each, grouped by part so each agent holds one reader in mind across its pages. The pattern is proven on Part I; this applies it. The two check scripts stay with the main thread, because every bucket would otherwise edit them.

**Tech Stack:** Astro 7 (static, no MDX), plain CSS, Node's built-in test runner. No new dependencies.

**Spec:** `docs/brainstorming/specs/2026-09-14-docs-verbosity-structure-design.md`

**Previous plans:** `docs-shell.md`, `docs-book.md`, `docs-moves.md`, `docs-part-one.md`, all complete.

## Global Constraints

- **Run every command from `web/`.** The worktree root has no `package.json`.
- **No new npm dependencies.**
- **`node --test scripts/verify-prose-budget.ts` is the only command an agent runs.** Five agents share this worktree; concurrent `astro build` or `astro check` corrupts the shared cache. The main thread builds and verifies.
- **Never pipe `astro check` through `tail`.** When it has errors it prints the offending source above the summary, so `tail -3` cuts off the error count and prints "0 warnings / 0 hints". That hid ten real errors for four commits in the previous plan. Read the whole output or grep for `error`.
- **Prose is not code.** Run `humanizer` then `rin-voice` over every sentence written or reworked. No em dashes, bold almost never, no three-item parallel lists, no closing paragraph restating what was just said, no "we".
- **Cut by deleting whole sentences, never by compressing clear prose into dense prose.** A page that cannot reach its budget without dropping a caveat, a failure mode or a number keeps them and reports the overage. The budget is a ceiling on the reader's time, not a licence to delete what they need.
- **A collapsed section keeps its heading.** Put the `<h2 id="...">` inside the `<summary>`, never the id on the `<details>`. With the id on the `<details>` the section leaves the document outline, leaves the anchor check's reach, loses its hover anchor, and its deep link stops working. Measured on `/docs/startup#openrc-and-the-bsds` in the previous plan. The shared stylesheet already styles `summary > h2`.
- **Pages pass `description` and `activeSlug` only.** `DocsLayout` derives the title and crumb from `docs-nav.ts`.
- **Do not touch `docs-nav.ts`, any slug, `verify-heading-anchors.ts`, or `verify-prose-budget.ts`.**
- **Conventional commit subjects.** `revert` and `build` are refused by the commit hook; they match no parser in `release-plz-changelog.toml`.

## The page contract

Every page ends up in this shape:

```
h1
lede                 one sentence: what you can do after this page
<ShortVersion>       the commands, or the answer. no caveats. above the fold
h2 depth section     every heading with a unique id
  <details>          edge cases, behind a one-line visible summary
h2 depth section
```

`ShortVersion` takes an optional `label` for the few pages where "The short version" is the wrong words. Import it from `../../components/docs/ShortVersion.astro`.

## What each bucket holds

| bucket | chapters | prose now | target |
| --- | --- | --- | --- |
| A | lookout | 6,040 | 3,200 |
| B | logs, overrides, lifecycle, talking-to-a-sheep, output | 11,799 | 9,800 |
| C | folds, boot-order, secrets, kv | 5,232 | 4,700 |
| D | dogs, community-dogs, whistle, json-output, shepherd-channel | 6,844 | 6,200 |
| E | serve, cli, first-flockfile, not-built | 4,198 | 3,900 |

Only bucket A is a large cut. The rest are mostly shape and anchors: most of these pages are already close to the length their reader needs, and 169 headings across the nineteen have no id at all.

---

### Task 1: Bucket A, the lookout

6,040 prose words and seventeen headings, none with an id. It is the longest page on the site and it documents a terminal dashboard that ships its own keymap overlay.

**Files:** `web/src/pages/docs/lookout.astro`

- [ ] **Step 1: Cut what the program already shows**

v0.8.0 added an in-app keymap overlay, bound to `h` and `?`. The page's own keymap section duplicates it and will drift from it. Cut that section and replace it with one sentence naming the key. This is the only outright deletion in this plan; everything else moves or folds.

- [ ] **Step 2: Restructure**

1. Lede.
2. `<ShortVersion>`: `shep lookout`, and the three or four keys that get someone moving. Nothing else.
3. The flock table, the three panes, and the sheep pane stay as visible sections: they are what the dashboard is.
4. Every pane-by-pane walkthrough beyond those folds into a `<details>` behind a one-line summary of what that pane is for. The settings, secrets, config and dog-config panes are the obvious candidates.
5. When the terminal is small, and if the shepherd goes away, stay visible. Both are failure modes a reader hits without looking for them.

- [ ] **Step 3: Anchors, voice, report**

Seventeen headings need unique ids. Run `humanizer` then `rin-voice`. Report the final count, what folded, and anything you refused to cut.

---

### Task 2: Bucket B, Day to day

Five chapters a reader returns to: `logs` (1,926), `overrides` (3,427), `lifecycle` (2,019), `talking-to-a-sheep` (1,547), `output` (2,879). 11,799 words, target 9,800.

**Files:** those five pages only.

- [ ] **Step 1: Give each one a short version**

The answer or the commands, above the fold. For `logs` that is where lines land and how to follow them. For `overrides` it is the rule that a Flockfile is a template and the three doors into the override store. For `lifecycle` it is the difference between restart, reload and stop. For `talking-to-a-sheep` it is signal, whisper and trigger in one block. For `output` it is the three style levels.

- [ ] **Step 2: Merge the two overlapping openings on overrides**

That page now opens with `A Flockfile is a template, not live config`, moved there by the moves phase, directly above `Why a template and not just config`. They cover the same ground from two directions and were deliberately left adjacent for this task. Merge them into one section, keeping every distinct fact from both.

- [ ] **Step 3: Fold the edge cases**

`overrides` has twelve sections after its opening; several are refusal and precedence cases a reader meets rarely. `output` has nineteen headings, nine already with ids. Fold what is genuinely rare, keep what a reader hits by accident.

- [ ] **Step 4: Anchors, voice, report**

All headings need unique ids, including the ten on `output` that lack them. Keep every existing id exactly as it is: they are live URLs.

---

### Task 3: Bucket C, Configuration

`folds` (970), `boot-order` (1,632), `secrets` (2,066), `kv` (564). 5,232 words, target 4,700.

**Files:** those four pages only.

- [ ] **Step 1: Short version on each**

`folds`: setting one and selecting one. `boot-order`: naming a dependency. `secrets`: the three verbs. `kv`: the three verbs, which that page already leads with.

- [ ] **Step 2: Fold the edge cases, keep the honesty**

`secrets` includes a section on why there is no encryption. That stays visible. A reader deciding whether to put a password in this store needs it before they decide, not after.

- [ ] **Step 3: Anchors, voice, report**

33 headings across the four, none with an id.

---

### Task 4: Bucket D, Dogs and machine surfaces

`dogs` (2,884), `community-dogs` (288), `whistle` (731), `json-output` (1,290), `shepherd-channel` (1,651). 6,844 words, target 6,200.

**Files:** those five pages only.

- [ ] **Step 1: Move the channel's summary to the top**

`shepherd-channel` has a section called `Summary for the impatient` at position 7 of 8. It is the short version, written before there was a component for it, sitting after the thing it summarises. Move it into `<ShortVersion>` at the top and delete the old heading.

- [ ] **Step 2: Short version on the rest**

`dogs`: enabling one. `whistle`: running it. `json-output`: the envelope. `community-dogs` is 288 words and needs shape rather than cutting.

- [ ] **Step 3: State the stability guarantee**

The spec's S6 asks each machine surface to say what a consumer can rely on. `whistle`, `json-output` and `shepherd-channel` each state theirs, in one sentence, near the top. The facts are in `docs/decisions.md` and in `PROTOCOL_VERSION` / `SCHEMA_VERSION`: the envelope's schema moves only on a rename, removal or retype, and an additive field moves nothing. Do not invent a guarantee. If a page's guarantee is not documented anywhere, say so in the report rather than writing one.

- [ ] **Step 4: Anchors, voice, report**

39 headings across the five, one with an id.

---

### Task 5: Bucket E, Other places and Reference

`serve` (488), `cli` (355), `first-flockfile` (2,343), `not-built` (1,012). 4,198 words, target 3,900.

**Files:** those four pages only.

- [ ] **Step 1: Drop the duplicate Flockfile minimum**

`first-flockfile` opens with `Two fields is still a complete one`. Quickstart already carries that example, and this page is now the Flockfile reference. Cut the section and keep one line pointing at chapter 1 for anyone who arrived here first.

- [ ] **Step 2: Short version on each**

`serve`: the command and its defaults. `cli`: this page is generated from the binary's own help, so its short version is the alias table it already has. `first-flockfile`: the smallest complete file, then the field reference. `not-built`: what is deferred and what is scheduled.

- [ ] **Step 3: Say that the field reference and the lookout config pane are the same list**

`lookout/field.rs` builds its config form from the Flockfile JSON Schema, so every field on the reference page is editable in the TUI. The page documents the fields and never says this. One sentence, near the field reference.

- [ ] **Step 4: Anchors, voice, report**

26 headings across the four, none with an id.

---

### Task 6: Land it, in the main thread

- [ ] **Step 1: Build and typecheck each bucket as it reports**

```bash
npm run build
```

```bash
npx astro check
```

Read `astro check`'s whole output. Do not pipe it through `tail`.

- [ ] **Step 2: Open every page that changed**

Every defect across four completed plans was found by looking at a rendered page and none by reading a diff. For each page: the short version is above the fold, every disclosure opens, every heading has a hover anchor, and no heading renders empty.

- [ ] **Step 3: Check the deep links that already existed**

`output` and `overrides` and `lifecycle` and `json-output` carry thirteen ids between them that are live URLs. Confirm each still resolves after its page is rewritten.

```bash
cd web && npm run build && for id in grouped-instances the-dogs-table after-a-lifecycle-command; do grep -l "id=\"$id\"" dist/docs/*/index.html || echo "LOST: $id"; done
```

- [ ] **Step 4: Update the two check scripts**

Add all nineteen slugs to `ENFORCED`, which then covers all 27. Set each page's budget to what it measures plus a little headroom.

- [ ] **Step 5: Commit per bucket**

One commit per bucket, plus one for the check scripts.

---

## Done when

- All 27 chapters open with a short version above the fold.
- `ENFORCED` holds all 27 slugs and `npm run build` passes.
- Every heading on the site has a unique id, and no heading renders empty.
- Every id that was live before this plan still resolves.
- `shepherd-channel`'s summary is at the top of its page rather than seventh of eight.
- The lookout page no longer documents a keymap the program draws itself.
- `npx astro check` reports zero errors, read in full rather than tailed.

## Deliberately not in this plan

- **`/llms.txt`.** It is independent of every page rewrite and gets its own small plan.
- **The benchmark re-measure.** It needs a controlled machine with both shep and pm2, and it must not block the docs.
- **The verb table as its own chapter.** Recorded as a knowingly unmet criterion in the Part I plan. Revisit if beta readers keep asking where the verb list lives.
