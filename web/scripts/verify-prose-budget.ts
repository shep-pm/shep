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
  terminology: 250, // 176 today
  "community-dogs": 400, // 288
  cli: 500, // 355
  serve: 650, // 488
  kv: 750, // 564
  containers: 750, // 572
};

/** Words a reader actually reads: no frontmatter, styles, code or transcripts. */
export function proseWords(source: string): number {
  let s = source.replace(/^---[\s\S]*?^---/m, "");
  s = s.replace(/<style>[\s\S]*?<\/style>/g, "");
  s = s.replace(/<CodeBlock[^>]*>[\s\S]*?<\/CodeBlock>/g, "");
  s = s.replace(/<div class="terminal[\s\S]*?<\/div>\s*(?=<)/g, "");
  s = s.replace(/<div class="file-panel"[\s\S]*?<\/div>\s*<\/div>/g, "");
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
