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
 * Maximum prose words per page, by slug, at what each measures plus about
 * five percent.
 *
 * A number here only ever comes down. It came down across the board on
 * 2026-09-14, when the "Where to go next" cards were cut from all 25 pages
 * that carried them: 539 lines and the two-card pitch at the foot of each
 * chapter. The chapter bar already says what comes next, and the sidebar
 * lists every chapter on every page.
 *
 * Two pages had no entry at all until that pass. `lookout-config` and
 * `pm2-verbs` shipped after the phase that seeded this map and nothing
 * noticed, which is what the "every page has a budget" test below is for.
 * `lookout` is the other tell: it sat at 5750 against a page measuring
 * 2629, a ceiling so far above the text that it could not have failed.
 *
 * Raising one is allowed and has happened twice, both on 2026-09-14, both
 * for a correction rather than for prose: `getting-started` for the note
 * saying `./server` is the reader's own binary, and `from-pm2` for the
 * clause saying which of the three binaries 17.94 MiB is. Say why in a
 * comment when you do it.
 */
export const BUDGETS: Record<string, number> = {
  "writing-a-dog": 4150, // 4042
  overrides: 3380, // 3250
  "lookout-config": 3240, // 3085
  output: 2930, // 2872
  dogs: 2980, // 2834
  lookout: 2770, // 2629
  "first-flockfile": 2360, // 2245
  secrets: 2080, // 1977
  lifecycle: 2020, // 1919
  logs: 1900, // 1805
  "from-pm2": 1720, // 1659
  "shepherd-channel": 1690, // 1604
  "boot-order": 1670, // 1586
  "talking-to-a-sheep": 1610, // 1530
  "json-output": 1330, // 1275
  startup: 1250, // 1192
  examples: 1200, // 1156
  "not-built": 1080, // 1020
  folds: 900, // 852
  whistle: 710, // 675
  containers: 560, // 526
  upgrading: 560, // 526
  kv: 550, // 519
  serve: 480, // 448
  "getting-started": 440, // 413
  cli: 380, // 356
  "community-dogs": 300, // 277
  terminology: 210, // 199
  "pm2-verbs": 40, // 36
};


/**
 * Remove every `<div>` opening with `open`, balancing nested div tags.
 *
 * An unterminated tag ends the walk rather than continuing it. `indexOf`
 * answers -1 there, so the old `+ 1` restarted `j` at 0 and the scan found
 * the same tag forever. This script runs first in `npm run build`, so the
 * hang arrived instead of Astro's report of the malformed page.
 */
function stripBlock(source: string, open: string): string {
  let out = "";
  let i = 0;
  for (;;) {
    const start = source.indexOf(`<${open}`, i);
    if (start === -1) return out + source.slice(i);
    out += source.slice(i, start);
    let depth = 0;
    let j = start;
    for (;;) {
      const next = source.slice(j).search(/<\/?div\b/);
      if (next === -1) return out;
      j += next;
      const closer = source.startsWith("</div", j);
      const end = source.indexOf(">", j);
      if (end === -1) return out + source.slice(start);
      depth += closer ? -1 : 1;
      j = end + 1;
      if (closer && depth === 0) break;
    }
    i = j;
  }
}

/** Words a reader actually reads: no frontmatter, styles, code or transcripts. */
export function proseWords(source: string): number {
  let s = source.replace(/^---[\s\S]*?^---/m, "");
  s = s.replace(/<style>[\s\S]*?<\/style>/g, "");
  s = s.replace(/<CodeBlock[^>]*>[\s\S]*?<\/CodeBlock>/g, "");
  // Terminal transcripts and file panels nest <div>s, so a lazy match ends
  // at the FIRST inner </div> and leaves the rest of the block counted as
  // prose. Found by an agent rewriting the examples page, which has two of
  // them: `$ shep --help` and a column header row were being counted as
  // words a reader wades through. Balance the tags instead.
  s = stripBlock(s, 'div class="terminal');
  s = stripBlock(s, 'div class="file-panel"');
  // A tag may carry a quoted attribute that itself contains "<" or ">":
  // folds.astro's description says `fold:<name> reaches it from any verb`.
  // A plain /<[^>]*>/ stops at the first ">" it meets, which is the one
  // inside those quotes, so the tail of the tag was counted as prose. This
  // walks quoted runs as single units instead.
  s = s.replace(/<[a-zA-Z/!][^>"']*(?:(?:"[^"]*"|'[^']*')[^>"']*)*>/g, " ");
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

test("an unterminated div ends the walk instead of spinning forever", () => {
  // This used to hang. `indexOf(">")` answers -1 on a tag with no `>`, the
  // old `+ 1` sent the cursor back to 0, and the scan found the same tag
  // again. `npm run build` runs this script before astro, so the malformed
  // page was never reported: the build simply stopped.
  assert.equal(proseWords(`<p>ok here</p><div class="terminal"><div>a</div`), 3);
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

test("every page that exists has a budget", async () => {
  // The other direction, and the one that was missing. `lookout-config` and
  // `pm2-verbs` shipped after the phase that seeded BUDGETS, so they had no
  // ceiling at all: the report below printed them without one and nothing
  // failed. A page with no ceiling is a page that can grow back.
  const files = await readdir(pagesDir);
  const missing = files
    .filter((f) => f.endsWith(".astro"))
    .map((f) => f.replace(/\.astro$/, ""))
    // `index` is the redirect to chapter 1, not a chapter. Same exclusion
    // verify-docs-nav.ts makes, for the same reason.
    .filter((slug) => slug !== "index" && !(slug in BUDGETS));
  assert.deepEqual(
    missing,
    [],
    `no prose budget for ${missing.join(", ")}; add an entry at the page's current count plus a little headroom`,
  );
});

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
