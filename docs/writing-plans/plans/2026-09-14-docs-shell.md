# Docs shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the docs shell and build the mechanical gates the prose rewrite depends on, without changing a single sentence of docs prose or the order of the sidebar.

**Architecture:** Five independent changes under `web/`. One CSS fix to the sidebar, one extraction of duplicated page chrome into a shared stylesheet, and three new scripts or components that later phases are checked against. Nothing here touches page prose, `docsNav.ts` ordering, or any route.

**Tech Stack:** Astro 7 (static, no MDX), plain CSS, Node's built-in test runner with native TypeScript type stripping. No new dependencies.

**Spec:** `docs/brainstorming/specs/2026-09-14-docs-verbosity-structure-design.md`

## Global Constraints

- **Run every command from `web/`.** The worktree root has no `package.json`.
- **No new npm dependencies.** The site's only dependency is `astro`; devDependencies are `@astrojs/check`, `pagefind`, `typescript`.
- **Node >= 22.18.0**, enforced by `scripts/check-node.mjs`.
- **Both build commands, every time.** `npx astro build` does not typecheck. `npx astro check` is what catches a component being passed a prop it does not have. A wrong prop builds clean and renders wrong.
- **Conventional commit subjects**, `type(scope): summary`, with `!` on anything that breaks. `release-plz` reads individual commits and `filter_unconventional = true` silently drops whatever does not parse. Accepted types here: `fix`, `feat`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`, `perf`. Everything in this plan is `fix(web)` or `chore(web)`.
- **No absolute local paths** in any committed file. Repo-relative only.
- **Do not reorder `docsNav.ts` or edit any page's prose.** That is a later phase. A diff from this plan that changes a sentence of docs copy is out of scope.
- **The two check scripts added here ratchet.** Each has an explicit list of pages it enforces. Pages not on the list are reported but do not fail the build. Later phases add their pages to the list. Never widen a list to cover a page the current phase has not converted, and never shrink one.

---

### Task 1: The sidebar scrolls on its own

The sidebar is `position: sticky` with `min-height: calc(100vh - 64px)` and no overflow. Measured live at a 768px viewport: `clientHeight` 1492px, `overflow-y: visible`, so it cannot scroll independently and its lower items are only reachable by scrolling the article.

**Files:**
- Modify: `web/src/components/docs/DocsSidebar.astro:96-102` (the `.docs-sidebar` rule)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks read.

- [x] **Step 1: Confirm the bug before changing anything**

Start the preview and measure. From `web/`:

```bash
npx astro dev --port 4421
```

In another shell, or via the browser tooling, load `http://localhost:4421/docs/getting-started` and evaluate:

```js
const sb = document.querySelector('.docs-sidebar');
const c = getComputedStyle(sb);
({ clientHeight: sb.clientHeight, viewport: innerHeight, overflowY: c.overflowY,
   canScrollAlone: sb.scrollHeight > sb.clientHeight && c.overflowY !== 'visible' })
```

Expected before the fix: `clientHeight` well above `viewport`, `overflowY: "visible"`, `canScrollAlone: false`.

- [x] **Step 2: Apply the fix**

In `web/src/components/docs/DocsSidebar.astro`, replace the `min-height` line in `.docs-sidebar` and add two properties:

```css
  .docs-sidebar {
    position: sticky;
    top: 64px;
    padding: 34px 24px 60px;
    border-right: 3px solid var(--line);
    max-height: calc(100vh - 64px);
    overflow-y: auto;
    overscroll-behavior: contain;
  }
```

`min-height` becoming `max-height` is the fix itself: a sticky element taller than its viewport slot scrolls with the page instead of pinning. `overscroll-behavior: contain` stops the sidebar handing its scroll back to the article when it reaches either end.

- [x] **Step 3: Verify the fix in the browser**

Reload `http://localhost:4421/docs/getting-started` and re-run the Step 1 snippet.

Expected: `overflowY: "auto"` and `canScrollAlone: true`.

Then scroll the sidebar itself to its last item and confirm the footnote beginning "Docs track the repo" is reachable without moving the article.

- [x] **Step 4: Check the border did not break**

`border-right: 3px solid var(--line)` previously spanned the full column because `min-height` stretched the element. An `overflow` box ends at the viewport instead, so the rule may now stop short of the footer.

Take a screenshot at a 768px viewport with the page scrolled to the bottom and look at the line between the sidebar and the article.

If the border stops short, move it off the scroll box and onto the grid column. In `web/src/layouts/DocsLayout.astro`, add to the `.grid` rule:

```css
  .grid {
    max-width: 1340px;
    margin: 0 auto;
    display: grid;
    grid-template-columns: 264px minmax(0, 1fr);
    gap: 0;
    align-items: start;
    background: linear-gradient(to right, transparent 261px, var(--line) 261px, var(--line) 264px, transparent 264px);
  }
```

and delete `border-right: 3px solid var(--line);` from `.docs-sidebar`. Only do this if Step 4 shows a real gap; if the border still reaches, leave both files alone.

The mobile rule at `max-width: 860px` already sets `border-right: none` and `position: static`, so the border is unaffected either way.

**The mobile block does need one thing this plan did not anticipate.** It resets `min-height` but nothing resets `max-height` or `overflow-y`, and below 860px the sidebar is a static `<details>` in the page flow, so the new cap clips the expanded menu. Measured at 375px with the menu open, before the reset: 745px of visible height against 1441px of content, with a second scrollbar inside a disclosure that is already inside the page. Add to the `max-width: 860px` rule:

```css
    .docs-sidebar {
      position: static;
      /* The desktop rule caps the column so it can scroll beside the
         article. Here the whole disclosure is in the page flow, so the cap
         would clip the expanded menu and give it a second scrollbar. */
      max-height: none;
      overflow-y: visible;
      border-right: none;
      border-bottom: 3px solid var(--line);
      padding: 0;
    }
```

Then confirm at a 375px viewport that the Menu disclosure opens, closes, and is not clipped.

- [x] **Step 5: Build**

```bash
npx astro build
```

```bash
npx astro check
```

Expected: both clean.

- [x] **Step 6: Commit**

```bash
git add web/src/components/docs/DocsSidebar.astro
git commit -m "fix(web): let the docs sidebar scroll independently of the article"
```

If Step 4 required the gradient, add `web/src/layouts/DocsLayout.astro` to the same commit.

---

### Task 2: One stylesheet for the shared page chrome

`h1`, `h2`, `.lede`, `p`, `.fine` and the `.next-*` card grid are declared separately in 24 page `<style>` blocks. `h2` has drifted into nine variants. `output.astro` declares none at all, and `global.css` sets `h1, h2, h3 { margin: 0 }`, so every section heading on `/docs/output` renders at browser-default 24px with no gap above it.

**Files:**
- Create: `web/src/styles/docs-page.css`
- Modify: `web/src/layouts/DocsLayout.astro` (add the import)
- Modify: all 24 page files under `web/src/pages/docs/` that declare a `<style>` block (every file except `index.astro` and `output.astro`)

**Interfaces:**
- Consumes: nothing.
- Produces: the class names `.lede`, `.fine`, `.next-grid`, `.next-card`, `.next-title`, `.next-body`, styled globally for any page inside `.docs-shell`. Later phases write pages using these names without declaring them.

- [ ] **Step 1: Record the before state**

```bash
cd web && for f in src/pages/docs/*.astro; do s=$(awk '/^  h2 \{/,/\}/' "$f" | grep -E 'font-size|margin' | tr -d ' \n'); [ -n "$s" ] && printf "%-22s %s\n" "$(basename $f .astro)" "$s"; done | sort -u -k2
```

Expected: nine distinct `h2` shapes. Keep this output; Step 6 compares against it.

- [ ] **Step 2: Create the shared stylesheet**

Create `web/src/styles/docs-page.css`:

```css
/*
 * Shared chrome for every page under /docs, imported by DocsLayout.astro.
 *
 * These rules lived in 24 separate page <style> blocks until 2026-09-14,
 * by which point h2 alone had drifted into nine variants (26px/44/14 through
 * 29px/40/18) and output.astro had lost its copy entirely. global.css sets
 * h1, h2, h3 { margin: 0 }, so a page with no local rule rendered its
 * section headings at the browser default with no gap above them, which
 * astro build and astro check are both blind to.
 *
 * Every value below is the one the majority of pages already used, with one
 * deliberate change: h2's top margin goes from 40px to 48px, which is the
 * extra air between sections that the readability pass asked for.
 *
 * Scoped to `.docs-shell main` rather than left bare so it cannot reach the
 * landing page, and so it outranks any page-scoped leftover during the
 * migration.
 */

.docs-shell main h1 {
  font-size: clamp(36px, 4.6vw, 56px);
  line-height: 1;
  letter-spacing: -0.04em;
  margin: 0 0 18px;
}

.docs-shell main h2 {
  font-size: 29px;
  letter-spacing: -0.03em;
  margin: 48px 0 10px;
}

.docs-shell main h2:first-of-type {
  margin-top: 0;
}

.docs-shell main h3 {
  font-family: "Bricolage Grotesque", sans-serif;
  font-size: 19px;
  letter-spacing: -0.02em;
  margin: 28px 0 8px;
}

.docs-shell main p {
  font-size: 16.5px;
  line-height: 1.65;
  color: var(--ink-2);
  margin: 0 0 18px;
}

.docs-shell main .lede {
  font-size: 19px;
  line-height: 1.6;
  color: var(--ink-2);
  margin: 0 0 30px;
  text-wrap: pretty;
}

.docs-shell main .fine {
  font-size: 15px;
  line-height: 1.6;
  color: var(--ink-3);
  margin: -8px 0 30px;
}

.docs-shell main .next-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 16px;
  margin-top: 8px;
}

.docs-shell main .next-card {
  text-align: left;
  background: var(--paper-2);
  border: 3px solid var(--line);
  border-radius: 18px;
  box-shadow: 5px 5px 0 var(--shadow);
  padding: 20px;
  color: var(--ink);
  text-decoration: none;
  display: block;
}

.docs-shell main .next-card:hover {
  transform: translate(2px, 2px);
  box-shadow: 3px 3px 0 var(--shadow);
  color: var(--ink);
}

.docs-shell main .next-title {
  font-family: "Bricolage Grotesque", sans-serif;
  font-weight: 800;
  font-size: 17px;
  margin-bottom: 7px;
}

.docs-shell main .next-body {
  font-size: 14px;
  line-height: 1.5;
  color: var(--ink-2);
}

.docs-shell main .next-body code {
  font-size: 0.85em;
}
```

- [ ] **Step 3: Import it from the layout**

In `web/src/layouts/DocsLayout.astro`, add the import beneath the existing ones in the frontmatter:

```astro
import Base from "./Base.astro";
import DocsHeader from "../components/docs/DocsHeader.astro";
import DocsSidebar from "../components/docs/DocsSidebar.astro";
import "../styles/docs-page.css";
```

A plain CSS import is global, matching how `Base.astro` loads `global.css`. Because only `/docs/*` routes use this layout, the stylesheet reaches only those pages.

- [ ] **Step 4: Verify `/docs/output` is fixed before touching any other page**

Reload `http://localhost:4421/docs/output` and evaluate:

```js
[...document.querySelectorAll('main h2')].slice(0, 3)
  .map(h => { const c = getComputedStyle(h);
    return { t: h.textContent.trim().slice(0, 24), fontSize: c.fontSize, marginTop: c.marginTop }; })
```

Expected: `fontSize: "29px"` and `marginTop: "48px"` on all three. Before this task they were `24px` and `0px`.

- [ ] **Step 5: Remove the now-duplicated rules from each page**

For each of the 24 page files with a `<style>` block, delete only these rule blocks: `h1`, `h2`, `h2:first-of-type`, `h3`, `p`, `.lede`, `.fine`, `.next-grid`, `.next-card`, `.next-card:hover`, `.next-title`, `.next-body`, `.next-body code`.

Leave everything else. Pages carry genuinely page-specific styles that must stay: `.field-table` and its `.row` descendants, `.alias-grid`, `.terminal`, `.file-panel`, `.col-does`, `ul`, `li`, `code` and anything not in the list above.

Do this one file at a time and check each page renders before moving to the next. Two pages have a deliberately different `h1` (`clamp(34px, 4.4vw, 52px)` rather than `clamp(36px, 4.6vw, 56px)`); they lose it and adopt the shared value, which is intended.

If a page's `<style>` block becomes empty, delete the whole `<style>` element.

- [ ] **Step 6: Verify no page kept a shared rule**

```bash
cd web && grep -lE '^  (h1|h2|h3|p|\.lede|\.fine|\.next-grid|\.next-card|\.next-title|\.next-body) \{' src/pages/docs/*.astro
```

Expected: no output.

- [ ] **Step 7: Walk every page in the browser**

Load all 25 routes and confirm each still renders its own tables, terminals and grids. The pages with the most page-specific CSS are the ones most likely to have lost something by accident: `terminology`, `first-flockfile`, `from-pm2`, `examples`, `boot-order`.

- [ ] **Step 8: Build**

```bash
npx astro build
```

```bash
npx astro check
```

Expected: both clean.

- [ ] **Step 9: Commit**

```bash
git add web/src/styles/docs-page.css web/src/layouts/DocsLayout.astro web/src/pages/docs/
git commit -m "refactor(web): one stylesheet for docs page chrome, not 24"
```

---

### Task 3: Heading anchors, with a ratcheting build check

13 of 208 headings under `web/src/pages/docs/` carry an `id`, across five pages. No section can be linked, bookmarked or pasted into a message. This task adds the affordance and the guard; the ids themselves arrive per page in later phases.

Ids are hand-written rather than derived from heading text, because a derived slug changes the moment a heading is reworded and the spec's S3 wants a link that survives being pasted into a message.

**Files:**
- Create: `web/scripts/verify-heading-anchors.ts`
- Modify: `web/src/styles/docs-page.css` (the hover affordance)
- Modify: `web/src/layouts/DocsLayout.astro` (the click target)
- Modify: `web/package.json` (wire the check into `build`)

**Interfaces:**
- Consumes: `web/src/styles/docs-page.css` from Task 2.
- Produces: `ENFORCED`, an exported array of page slugs in `verify-heading-anchors.ts`. Later phases append their slugs to it. Also produces the convention every later page follows: `<h2 id="kebab-case-id">`.

- [ ] **Step 1: Write the failing test**

Create `web/scripts/verify-heading-anchors.ts`:

```ts
// Regression guard for docs heading anchors.
//
// Every H2 and H3 on an enforced page must carry a unique id, so any section
// can be linked to directly. Hand-written rather than derived from the
// heading text: a derived slug breaks the moment a heading is reworded, and
// the whole point is a URL that survives being pasted into a message.
//
// ENFORCED is a ratchet. A page joins it in the phase that converts it, and
// never leaves. Pages not listed are reported by the summary but do not fail
// the build, so this can be wired into `npm run build` on the day it is
// written rather than on the day the last page is converted.
//
// Runs under Node's built-in test runner with native TypeScript type
// stripping (`node --test scripts/verify-heading-anchors.ts`), matching
// verify-dogs-index.ts. No test framework is added for one module.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const pagesDir = fileURLToPath(new URL("../src/pages/docs", import.meta.url));

/**
 * Slugs whose H2s and H3s must all carry unique ids.
 *
 * Empty at the end of the docs-shell phase: that phase adds the affordance
 * and this guard, and converts no page. Each later phase appends the pages
 * it converted.
 */
export const ENFORCED: string[] = [];

/** Every `<h2 ...>` / `<h3 ...>` open tag in a page, with its id if it has one. */
export function headings(source: string): { tag: string; id: string | null }[] {
  const found: { tag: string; id: string | null }[] = [];
  for (const m of source.matchAll(/<(h[23])(\s[^>]*)?>/g)) {
    const attrs = m[2] ?? "";
    const id = /\sid=["']([^"']+)["']/.exec(attrs);
    found.push({ tag: m[1], id: id ? id[1] : null });
  }
  return found;
}

test("headings() finds an id when there is one, and reports null when there is not", () => {
  const found = headings(`<h2 id="naming-a-dependency">Naming a dependency</h2><h3>Paths</h3>`);
  assert.deepEqual(found, [
    { tag: "h2", id: "naming-a-dependency" },
    { tag: "h3", id: null },
  ]);
});

test("headings() is not fooled by an id on some other element", () => {
  const found = headings(`<div id="wrapper"><h2>Untagged</h2></div>`);
  assert.deepEqual(found, [{ tag: "h2", id: null }]);
});

test("every enforced page gives every H2 and H3 a unique id", async () => {
  for (const slug of ENFORCED) {
    const source = await readFile(`${pagesDir}/${slug}.astro`, "utf8");
    const found = headings(source);
    const missing = found.filter((h) => h.id === null);
    assert.equal(
      missing.length,
      0,
      `${slug}.astro has ${missing.length} heading(s) with no id; every H2 and H3 on an enforced page needs one`,
    );
    const ids = found.map((h) => h.id);
    const duplicates = ids.filter((id, i) => ids.indexOf(id) !== i);
    assert.deepEqual(
      duplicates,
      [],
      `${slug}.astro repeats heading id(s) ${duplicates.join(", ")}; an id has to be unique within its page`,
    );
  }
});

test("every enforced slug is a page that exists", async () => {
  const files = await readdir(pagesDir);
  for (const slug of ENFORCED) {
    assert.ok(
      files.includes(`${slug}.astro`),
      `ENFORCED lists "${slug}" but src/pages/docs/${slug}.astro does not exist`,
    );
  }
});
```

- [ ] **Step 2: Run it and watch the unit tests pass and the page tests pass vacuously**

```bash
cd web && node --test scripts/verify-heading-anchors.ts
```

Expected: 4 tests pass. The two page tests pass over an empty `ENFORCED`, which is correct for this phase.

- [ ] **Step 3: Prove the guard actually refuses something**

Temporarily set `export const ENFORCED: string[] = ["getting-started"];` and re-run:

```bash
cd web && node --test scripts/verify-heading-anchors.ts
```

Expected: FAIL with exactly `getting-started.astro has 8 heading(s) with no id; every H2 and H3 on an enforced page needs one`. A gate tested only against passing input is indistinguishable from one that never refuses anything.

Then set `ENFORCED` back to `[]` and re-run to confirm it passes again.

- [ ] **Step 4: Add the hover affordance**

Append to `web/src/styles/docs-page.css`:

```css
/*
 * Anchor affordance. The id is what makes a section linkable; this is only
 * the visible handle. The marker is a real <a> injected by DocsLayout rather
 * than a ::after pseudo-element, because a pseudo-element cannot be clicked
 * or focused. Without JavaScript the ids still resolve, the handle is just
 * not drawn.
 */
.docs-shell main h2[id],
.docs-shell main h3[id] {
  position: relative;
}

.docs-shell main .heading-anchor {
  position: absolute;
  left: -0.85em;
  opacity: 0;
  text-decoration: none;
  color: var(--ink-3);
  font-weight: 400;
  border: 0;
}

.docs-shell main h2[id]:hover .heading-anchor,
.docs-shell main h3[id]:hover .heading-anchor,
.docs-shell main .heading-anchor:focus-visible {
  opacity: 1;
}

@media (max-width: 860px) {
  /* No hover on a phone, and the negative offset would sit off-screen. */
  .docs-shell main .heading-anchor {
    display: none;
  }
}
```

- [ ] **Step 5: Inject the click target**

In `web/src/layouts/DocsLayout.astro`, add this after the closing `</div>` of `.grid` and before `</div>` of `.docs-shell`:

```astro
<script>
  // Turns every id-bearing section heading into something a reader can click
  // to get its URL. Ids are authored in the pages themselves; this only draws
  // the handle, so deep links keep working with this script blocked.
  for (const heading of document.querySelectorAll<HTMLElement>(
    ".docs-shell main h2[id], .docs-shell main h3[id]",
  )) {
    const link = document.createElement("a");
    link.className = "heading-anchor";
    link.href = `#${heading.id}`;
    link.textContent = "#";
    link.setAttribute("aria-label", `Link to ${heading.textContent?.trim() ?? "this section"}`);
    heading.prepend(link);
  }
</script>
```

- [ ] **Step 6: Verify on a page that already has ids**

`output.astro` already carries nine. Load `http://localhost:4421/docs/output`, hover a heading such as "Multi-instance apps group", and confirm a `#` appears to its left and navigates to `/docs/output#grouped-instances` when clicked.

Then confirm the deep link works cold: load `http://localhost:4421/docs/output#the-dogs-table` directly and check the page lands on that section.

- [ ] **Step 7: Wire the check into the build**

In `web/package.json`, add the new test to the `build` script, next to the existing one:

```json
    "build": "node scripts/check-node.mjs && node --test scripts/verify-dogs-index.ts && node --test scripts/verify-heading-anchors.ts && astro build && pagefind --site dist && node scripts/verify-pagefind-index.mjs",
```

- [ ] **Step 8: Build**

```bash
npm run build
```

```bash
npx astro check
```

Expected: both clean, and the build output shows the new test file running.

- [ ] **Step 9: Commit**

```bash
git add web/scripts/verify-heading-anchors.ts web/src/styles/docs-page.css web/src/layouts/DocsLayout.astro web/package.json
git commit -m "feat(web): linkable docs headings, with a ratcheting anchor check"
```

---

### Task 4: A prose budget, so a rewrite cannot quietly keep everything

The spec sets word targets per page. A rewrite brief without a numeric ceiling and a script that enforces it reliably comes back having kept most of the original text.

**Files:**
- Create: `web/scripts/verify-prose-budget.ts`
- Modify: `web/package.json` (wire into `build`)

**Interfaces:**
- Consumes: nothing.
- Produces: `BUDGETS`, an exported record of slug to maximum prose words, and `proseWords(source)`, the counting function. Later phases add entries to `BUDGETS` and are checked against them.

- [ ] **Step 1: Write the failing test**

Create `web/scripts/verify-prose-budget.ts`:

```ts
// Prose ceiling for the docs pages.
//
// Counts prose only: Astro frontmatter, <style> blocks, <CodeBlock> contents
// and terminal transcripts are excluded, because none of them is text a
// reader wades through and none should be cut to hit a number. A page over
// its budget fails the build.
//
// BUDGETS is a ratchet in the same shape as verify-heading-anchors.ts's
// ENFORCED. A page joins it in the phase that rewrites it. A page not listed
// is reported by the summary and does not fail.
//
// This exists because a rewrite brief carrying no numeric target reliably
// comes back having kept most of the original text. The number is the brief.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const pagesDir = fileURLToPath(new URL("../src/pages/docs", import.meta.url));

/**
 * Maximum prose words per page, by slug.
 *
 * Empty at the end of the docs-shell phase, which rewrites nothing. Each
 * later phase adds the pages it rewrote, at the target the spec sets.
 */
export const BUDGETS: Record<string, number> = {};

/** Words a reader actually reads: no frontmatter, styles, code or transcripts. */
export function proseWords(source: string): number {
  let s = source.replace(/^---[\s\S]*?^---/m, "");
  s = s.replace(/<style>[\s\S]*?<\/style>/g, "");
  s = s.replace(/<CodeBlock[^>]*>[\s\S]*?<\/CodeBlock>/g, "");
  s = s.replace(/<div class="terminal[\s\S]*?<\/div>\s*(?=<)/g, "");
  s = s.replace(/<div class="file-panel"[\s\S]*?<\/div>\s*<\/div>/g, "");
  s = s.replace(/<[^>]*>/g, " ");
  s = s.replace(/\{[^{}]*\}/g, " ");
  return s.split(/\s+/).filter(Boolean).length;
}

test("proseWords counts prose and ignores frontmatter, styles and code", () => {
  const page = [
    "---",
    'import DocsLayout from "../../layouts/DocsLayout.astro";',
    "---",
    "<p>One two three four five</p>",
    "<CodeBlock>{`this whole block is not prose at all`}</CodeBlock>",
    "<style>.x { color: red; }</style>",
  ].join("\n");
  assert.equal(proseWords(page), 5);
});

test("proseWords does not count a terminal transcript", () => {
  const page = `<p>Two words</p><div class="terminal"><div>shep start Flockfile.toml</div></div><p>here</p>`;
  assert.equal(proseWords(page), 3);
});

test("every budgeted page is at or under its ceiling", async () => {
  for (const [slug, budget] of Object.entries(BUDGETS)) {
    const source = await readFile(`${pagesDir}/${slug}.astro`, "utf8");
    const actual = proseWords(source);
    assert.ok(
      actual <= budget,
      `${slug}.astro has ${actual} prose words against a budget of ${budget}; cut ${actual - budget} or move them to the page they belong on`,
    );
  }
});

test("every budgeted slug is a page that exists", async () => {
  const files = await readdir(pagesDir);
  for (const slug of Object.keys(BUDGETS)) {
    assert.ok(
      files.includes(`${slug}.astro`),
      `BUDGETS names "${slug}" but src/pages/docs/${slug}.astro does not exist`,
    );
  }
});
```

- [ ] **Step 2: Run it**

```bash
cd web && node --test scripts/verify-prose-budget.ts
```

Expected: 4 tests pass.

- [ ] **Step 3: Prove it refuses an over-budget page, and accepts an under-budget one**

Temporarily set `export const BUDGETS: Record<string, number> = { "getting-started": 300, terminology: 500 };` and re-run.

Expected: FAIL with exactly `getting-started.astro has 1245 prose words against a budget of 300; cut 945 or move them to the page they belong on`, and no complaint about `terminology`, which is 176 and under its 500. One case of each direction, so the guard is known to discriminate rather than merely to refuse.

Set `BUDGETS` back to `{}` and re-run to confirm it passes.

- [ ] **Step 4: Add a reporting mode for the pages not yet budgeted**

Append to `web/scripts/verify-prose-budget.ts`:

```ts
test("report every page's prose count, so the unbudgeted ones stay visible", async () => {
  const files = (await readdir(pagesDir)).filter((f) => f.endsWith(".astro") && f !== "index.astro");
  const rows: [string, number][] = [];
  for (const file of files) {
    const slug = file.replace(/\.astro$/, "");
    rows.push([slug, proseWords(await readFile(`${pagesDir}/${file}`, "utf8"))]);
  }
  rows.sort((a, b) => b[1] - a[1]);
  const total = rows.reduce((sum, [, n]) => sum + n, 0);
  for (const [slug, n] of rows) {
    const budget = BUDGETS[slug];
    console.log(`  ${String(n).padStart(5)}  ${slug}${budget === undefined ? "" : `  (budget ${budget})`}`);
  }
  console.log(`  ${String(total).padStart(5)}  TOTAL across ${rows.length} pages`);
  assert.ok(rows.length > 0, "found no docs pages to count");
});
```

- [ ] **Step 5: Run and record the baseline**

```bash
cd web && node --test scripts/verify-prose-budget.ts 2>&1 | grep -E '^\s+[0-9]+\s'
```

Expected: 25 rows and a total of 44,584. Paste that output into the commit message, so the starting point is in the history rather than only in a spec.

- [ ] **Step 6: Wire into the build**

In `web/package.json`:

```json
    "build": "node scripts/check-node.mjs && node --test scripts/verify-dogs-index.ts && node --test scripts/verify-heading-anchors.ts && node --test scripts/verify-prose-budget.ts && astro build && pagefind --site dist && node scripts/verify-pagefind-index.mjs",
```

- [ ] **Step 7: Build**

```bash
npm run build
```

Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add web/scripts/verify-prose-budget.ts web/package.json
git commit -m "feat(web): a prose budget the docs pages are checked against"
```

---

### Task 5: An escape hatch on every page

The spec's acceptance list ends with the DM test: take the last thing a beta tester was sent privately and find it on the site. A page that does not answer a reader currently offers them nothing, which is how the question becomes a private message. Every page gets the same line, rendered by the layout so no page can omit it.

**Files:**
- Create: `web/src/components/docs/StillStuck.astro`
- Modify: `web/src/layouts/DocsLayout.astro` (render it in the footer)

**Interfaces:**
- Consumes: nothing.
- Produces: `StillStuck.astro`, rendered by `DocsLayout` for every `/docs/*` route. No page imports it.

- [ ] **Step 1: Create the component**

Create `web/src/components/docs/StillStuck.astro`:

```astro
---
/*
 * The last thing on every docs page. Rendered by DocsLayout rather than
 * imported per page, so a new page cannot ship without it.
 *
 * It exists because the alternative is a private message. A reader whose
 * question the page did not answer needs somewhere to go that is not the
 * maintainer's inbox, and an issue is also the only form of that question
 * anybody else ever gets to read.
 *
 * data-pagefind-ignore for the same reason the footer license line carries
 * it: repeated on all 25 pages, it would pad every search excerpt.
 */
---

<aside class="still-stuck" data-pagefind-ignore>
  <span class="ask">Didn't find it here?</span>
  <a href="https://github.com/shep-pm/shep/issues/new" rel="noopener">Open an issue</a>
  <span class="or">or browse the source on <a href="https://github.com/shep-pm/shep" rel="noopener">GitHub</a>.</span>
</aside>

<style>
  .still-stuck {
    margin-top: 44px;
    padding: 18px 20px;
    border: 3px solid var(--line);
    border-radius: 14px;
    background: var(--paper-2);
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
    font-size: 15px;
    color: var(--ink-2);
  }

  .ask {
    font-family: "Bricolage Grotesque", sans-serif;
    font-weight: 800;
    color: var(--ink);
  }

  .or {
    color: var(--ink-3);
  }
</style>
```

- [ ] **Step 2: Render it from the layout**

In `web/src/layouts/DocsLayout.astro`, import it alongside the others:

```astro
import StillStuck from "../components/docs/StillStuck.astro";
```

and place it inside `<main>`, between the page slot and the existing footer:

```astro
      <main data-pagefind-body>
        <div class="crumb">{crumb}</div>
        <slot />
        <StillStuck />
        <footer class="docs-footer" data-pagefind-ignore>
          <span class="license">
            shep docs · pre-release · MIT OR Apache-2.0
          </span>
        </footer>
      </main>
```

- [ ] **Step 3: Verify it renders on every route, and check the links**

Load three pages of different shapes, for example `/docs/getting-started`, `/docs/output` and `/docs/terminology`, and confirm the box appears above the license footer on each.

Confirm both hrefs resolve rather than 404: `https://github.com/shep-pm/shep/issues/new` and `https://github.com/shep-pm/shep`.

- [ ] **Step 4: Verify it did not pollute the search index**

```bash
cd web && npm run build
```

Then search the built index for the phrase and confirm it is absent from page excerpts:

```bash
cd web && grep -rl "Didn't find it here" dist/pagefind/ | head
```

Expected: no output. `data-pagefind-ignore` should have carved it out. If it appears, the attribute is on the wrong element.

- [ ] **Step 5: Build**

```bash
npm run build
```

```bash
npx astro check
```

Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add web/src/components/docs/StillStuck.astro web/src/layouts/DocsLayout.astro
git commit -m "feat(web): every docs page offers somewhere to ask"
```

---

## Done when

- The sidebar scrolls independently at a 768px viewport, and its last item is reachable without moving the article.
- `/docs/output` renders its section headings at 29px with 48px above, like every other page.
- `h2` is declared once, in `web/src/styles/docs-page.css`.
- `npm run build` runs both new checks and passes, and `npx astro check` is clean.
- Both new checks have been seen to fail on bad input and pass on good, not merely to pass.
- Every `/docs/*` route ends with the escape hatch, and it is absent from the search index.
- No docs page's prose has changed, and `docsNav.ts` is untouched.
