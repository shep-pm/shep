// Regression guard for the docs book.
//
// docs-nav.ts is the only place a chapter is named, numbered or ordered, and
// DocsLayout derives the crumb and the title from it. That makes three
// failures silent rather than loud: a built entry whose page does not
// exist, a page nobody filed in the nav, and an unbuilt entry whose page
// has since been written and is now unreachable from the sidebar.
//
// Runs under Node's built-in test runner with native TypeScript type
// stripping, matching verify-dogs-index.ts.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chapters, chapterFor } from "../src/data/docs-nav.ts";

const pagesDir = fileURLToPath(new URL("../src/pages/docs", import.meta.url));

/** Page slugs that really exist on disk. `index` is the redirect, not a chapter. */
async function pageSlugs(): Promise<string[]> {
  const files = await readdir(pagesDir);
  return files
    .filter((f) => f.endsWith(".astro"))
    .map((f) => f.replace(/\.astro$/, ""))
    .filter((slug) => slug !== "index");
}

test("every built chapter has a page", async () => {
  const slugs = await pageSlugs();
  const missing = chapters
    .filter((c) => c.item.built && !slugs.includes(c.item.slug))
    .map((c) => c.item.slug);
  assert.deepEqual(missing, [], `built chapters with no page: ${missing.join(", ")}`);
});

test("every page is filed in the nav", async () => {
  const slugs = await pageSlugs();
  const filed = new Set(chapters.map((c) => c.item.slug));
  const unfiled = slugs.filter((slug) => !filed.has(slug));
  assert.deepEqual(
    unfiled,
    [],
    `pages missing from docs-nav.ts, so unreachable from the sidebar: ${unfiled.join(", ")}`,
  );
});

test("no unbuilt chapter has quietly grown a page", async () => {
  const slugs = await pageSlugs();
  const written = chapters
    .filter((c) => !c.item.built && slugs.includes(c.item.slug))
    .map((c) => c.item.slug);
  assert.deepEqual(
    written,
    [],
    `these have a page but are still marked built: false, so the sidebar will not link them: ${written.join(", ")}`,
  );
});

test("chapter numbers run 1..n with no gaps and no repeats", () => {
  assert.deepEqual(
    chapters.map((c) => c.number),
    chapters.map((_, i) => i + 1),
  );
});

test("no two chapters share a slug", () => {
  const slugs = chapters.map((c) => c.item.slug);
  const repeated = slugs.filter((s, i) => slugs.indexOf(s) !== i);
  assert.deepEqual(repeated, [], `repeated slugs: ${repeated.join(", ")}`);
});

test("neighbours skip unbuilt chapters in both directions", () => {
  for (const { item } of chapters) {
    const found = chapterFor(item.slug);
    assert.ok(found, `chapterFor lost ${item.slug}`);
    assert.notEqual(
      found.next?.item.built,
      false,
      `next from ${item.slug} is unbuilt, so the bar would link a 404`,
    );
    assert.notEqual(
      found.previous?.item.built,
      false,
      `previous from ${item.slug} is unbuilt, so the bar would link a 404`,
    );
  }
});

test("chapterFor returns undefined for a slug that is not a chapter", () => {
  assert.equal(chapterFor("not-a-real-page"), undefined);
});

test("the first chapter has no previous and the last has no next", () => {
  const first = chapterFor(chapters[0].item.slug);
  const last = chapterFor(chapters.at(-1)!.item.slug);
  assert.equal(first?.previous, undefined);
  assert.equal(last?.next, undefined);
});
