// Regression guard for deep links into a collapsed <details>.
//
// Half the docs pages fold their longer sections behind a disclosure, and
// every one of those puts the section's `<h2 id="...">` inside the
// `<summary>`, which renders whether the disclosure is open or shut. That is
// what makes the fragments in those pages work everywhere: a link to
// `#never-escalates` scrolls to a heading that is already on screen.
//
// Nothing stops a page putting an id in the BODY of a `<details>` instead.
// Chromium opens an ancestor disclosure when a fragment targets something
// inside it; Firefox and Safari have historically left it shut, so the link
// would land on a heading the reader cannot see. DocsLayout only decorates
// existing ids, and handles neither the initial fragment nor `hashchange`.
//
// Rather than write that fallback for a case the site does not have, this
// asserts the site keeps not having it. Runs over the BUILT pages, not the
// sources: `.astro` is not HTML, and five separate regex defects on this
// branch came from pretending otherwise.
//
// Raised by CodeRabbit on PR #248.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const distDir = fileURLToPath(new URL("../dist/docs", import.meta.url));

/**
 * Ids that a closed `<details>` would hide: inside one, outside its summary.
 *
 * Walks the tags rather than matching a whole block, so a nested disclosure
 * is counted at its own depth.
 */
export function hiddenIds(html: string): string[] {
  const hidden: string[] = [];
  let depth = 0;
  let inSummary = false;
  // The tag alternatives come first and swallow their own attributes, so an
  // id on the `<details>` or the `<summary>` itself is never read as hidden:
  // both elements render whether the disclosure is open or shut.
  const token = /<(\/?)(details|summary)\b[^>]*>|\sid="([^"]+)"/g;
  for (const m of html.matchAll(token)) {
    const [, closing, tag, id] = m;
    if (id !== undefined) {
      if (depth > 0 && !inSummary) hidden.push(id);
    } else if (tag === "details") {
      depth += closing ? -1 : 1;
    } else {
      inSummary = !closing;
    }
  }
  return hidden;
}

test("hiddenIds() reports an id in the body of a details", () => {
  const html = `<details><summary><h2 id="shown">S</h2></summary><h3 id="buried">B</h3></details>`;
  assert.deepEqual(hiddenIds(html), ["buried"]);
});

test("hiddenIds() allows an id on the details or the summary itself", () => {
  const html = `<details id="on-the-details"><summary id="on-the-summary">S</summary><p>text</p></details>`;
  assert.deepEqual(hiddenIds(html), []);
});

test("hiddenIds() leaves ids outside a details alone", () => {
  const html = `<h2 id="free">F</h2><details><summary><h2 id="also-free">A</h2></summary><p>text</p></details><h2 id="after">A</h2>`;
  assert.deepEqual(hiddenIds(html), []);
});

test("hiddenIds() counts a nested details at its own depth", () => {
  const html = `<details><summary><h2 id="outer">O</h2></summary><details><summary><h3 id="inner">I</h3></summary><p id="deep">d</p></details></details>`;
  assert.deepEqual(hiddenIds(html), ["deep"]);
});

test("no built page hides a fragment target inside a closed details", async () => {
  const dirs = await readdir(distDir, { withFileTypes: true });
  const pages = dirs.filter((d) => d.isDirectory()).map((d) => d.name);
  assert.ok(pages.length > 0, `no built pages under ${distDir}; run astro build first`);
  for (const page of pages) {
    const html = await readFile(`${distDir}/${page}/index.html`, "utf8");
    assert.deepEqual(
      hiddenIds(html),
      [],
      `/docs/${page} puts id(s) ${hiddenIds(html).join(", ")} in the body of a <details>. Firefox and Safari leave the disclosure shut when a fragment targets one, so the link lands on a heading nobody can see. Move the id onto the heading inside the <summary>.`,
    );
  }
});
