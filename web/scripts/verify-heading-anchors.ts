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
 * Starts with the one page the docs-shell phase converted. That phase is
 * about the guard rather than the pages, but a list that is empty passes
 * whatever it is handed, which makes it indistinguishable from a guard that
 * checks nothing. One real page keeps it honest. Each later phase appends
 * what it converted, and nothing ever leaves.
 */
export const ENFORCED: string[] = ["containers"];

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
