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
 * The docs-shell phase rewrites nothing, so none of these is a rewrite
 * target. They are the six pages already short enough not to need one,
 * seeded at roughly their current length plus headroom, so the guard holds
 * a real line from its first day. An empty map passes whatever it is handed
 * and is indistinguishable from a guard that checks nothing.
 *
 * Each later phase adds the pages it rewrote, at the target the spec sets.
 * A number here only ever comes down.
 */
export const BUDGETS: Record<string, number> = {
  terminology: 250, // 243 today
  "community-dogs": 400, // 288
  cli: 500, // 355
  examples: 1200, // 1182
  serve: 650, // 488
  kv: 750, // 564
  containers: 750, // 572

  // The eight pages the moves phase touched, at their post-move counts plus
  // a little headroom. These are not rewrite targets either: they are a
  // floor that stops a page growing back before the rewrite phase sets real
  // numbers against each reader's need.
  // Raised from 400 on 2026-09-14 for the note saying `./server` is the
  // reader's own binary and pointing at `shep serve` for anyone who has no
  // app to hand. The quickstart's three blocks were not runnable from a
  // fresh install without it.
  "getting-started": 450, // 440
  upgrading: 580, // 539
  // Raised from 1650 on 2026-09-14 for the paragraph comparing this run
  // against the previous one. A reader deciding whether to switch wants to
  // know four figures moved against shep, and the alternative was
  // compressing clear prose to hit a number. Raised again the same day, to
  // 1720, for the clause saying which of the three binaries 17.94 MiB is:
  // without it the paragraph compared one binary against three.
  "from-pm2": 1720, // 1708
  startup: 1250, // 1242
  dogs: 3000, // 2884
  "writing-a-dog": 4150, // 4042
  "first-flockfile": 2450, // 2343
  overrides: 3380, // 3314

  // Parts II to VII, at what each measures plus headroom.
  lookout: 5750,
  // Raised from 2930 on 2026-09-14 for the host line, which `shep flock`
  // began printing on a one-shot listing and not only under `--follow`. It
  // shows above every table on this page, so four transcripts gained a line
  // and the page gained a section saying what the four figures are and why
  // a rate can read `-`. Cut to a callout and a paragraph first; what is
  // left is the `-` rule, which a reader who does not have it reads an
  // unmeasured machine as an idle one.
  output: 3130, // 3111
  secrets: 2110,
  lifecycle: 2060,
  logs: 1900,
  "shepherd-channel": 1700,
  "boot-order": 1670,
  "talking-to-a-sheep": 1620,
  // Raised from 1330 on the same day and for the same feature. `host` is a
  // fourth key beside `data`, and its three spellings are three different
  // answers a script has to tell apart: an object, `null`, and no key at
  // all. Documenting the key without them would leave a reader unable to
  // read its absence.
  "json-output": 1410, // 1397
  "not-built": 1100,
  folds: 930,
  whistle: 770,
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
