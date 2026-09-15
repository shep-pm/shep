// Regression guard for the docs callouts.
//
// A callout is a two-column grid: the tag on the left, the prose on the
// right. It used to wrap its slot in a `<p>`, and a `<p>` cannot hold a `<p>`,
// so a caller passing paragraphs had them closed out of the wrapper and
// promoted to grid items of their own. `/docs/writing-a-dog` did exactly
// that. The tag's column is sized to its content, so a paragraph landing in
// it took 267px of the 327 a phone had and left the prose column at zero:
// the text ran 54px past the right edge and the whole page scrolled
// sideways. At 1280px the same callout stood 2,639px tall behind a 688px
// tag pill.
//
// Nothing about that is a width problem, so this does not measure one. The
// rule that would have caught it is on the wrapper: a callout's prose sits
// somewhere a paragraph can go.
//
// It is deliberately not on the child count, and that is worth saying,
// because the child count looks like the more direct check and cannot see
// this defect at all. The parser is what moves those paragraphs; the file
// still holds them nested inside the wrapper, so a walk over the built HTML
// counts two children for the callout the browser lays out as five. Measured
// against the broken build. The child count is here anyway, for a third
// element arriving in the component's own markup, which is a thing the file
// does say.
//
// Reads the built pages rather than the `.astro` sources, for the reason
// verify-fragment-targets.ts gives: `.astro` is not HTML.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const distDir = fileURLToPath(new URL("../dist", import.meta.url));

/** Elements with no closing tag, which must not open a level. */
const VOID = new Set([
  "area",
  "base",
  "br",
  "col",
  "embed",
  "hr",
  "img",
  "input",
  "link",
  "meta",
  "source",
  "track",
  "wbr",
]);

/** A callout: the variant on its container, and its direct children. */
export interface Callout {
  /** The container's classes, `callout` itself left out. */
  variant: string;
  /** Each direct child as `tag.class`, in document order. */
  children: string[];
}

/**
 * Every callout on the page, with the children the grid lays out.
 *
 * Walks tags from the container, counting depth, and records what sits at
 * depth one. Attribute values holding a `>` would throw the walk off; none
 * of these pages has one, and the last test in this file is what notices if
 * that stops being true.
 */
export function callouts(html: string): Callout[] {
  const found: Callout[] = [];
  const open = /<div class="callout([^"]*)"[^>]*>/g;
  for (const start of html.matchAll(open)) {
    const children: string[] = [];
    const tag = /<(\/?)([a-zA-Z0-9-]+)((?:"[^"]*"|[^>"])*)>/g;
    tag.lastIndex = start.index + start[0].length;
    let depth = 1;
    let m: RegExpExecArray | null;
    while (depth > 0 && (m = tag.exec(html))) {
      const name = m[2].toLowerCase();
      if (m[1]) {
        depth--;
        continue;
      }
      if (VOID.has(name) || m[3].endsWith("/")) continue;
      if (depth === 1) {
        const cls = /\sclass="([^"]*)"/.exec(m[3]);
        children.push(cls ? `${name}.${cls[1].split(/\s+/)[0]}` : name);
      }
      depth++;
    }
    found.push({
      variant: start[1].trim().split(/\s+/).filter(Boolean).join("."),
      children,
    });
  }
  return found;
}

/** Callouts laying out more or fewer than two grid items, as `variant: kids`. */
export function extraItems(html: string): string[] {
  return callouts(html)
    .filter((c) => c.children.length !== 2)
    .map((c) => `${c.variant || "callout"}: ${c.children.join(" ")}`);
}

/** Callouts holding their prose in a `<p>`, which a caller can break out of. */
export function pWrapped(html: string): string[] {
  return callouts(html)
    .filter((c) => c.children[1] === "p" || c.children[1] === undefined)
    .map((c) => `${c.variant || "callout"}: ${c.children.join(" ")}`);
}

async function docsPages(): Promise<[string, string][]> {
  const dirs = await readdir(`${distDir}/docs`, { withFileTypes: true });
  const names = dirs.filter((d) => d.isDirectory()).map((d) => d.name);
  assert.ok(
    names.length > 0,
    `no built pages under ${distDir}/docs; run astro build first`,
  );
  return Promise.all(
    names.map(
      async (name) =>
        [name, await readFile(`${distDir}/docs/${name}/index.html`, "utf8")] as [
          string,
          string,
        ],
    ),
  );
}

test("callouts() reads the two children a callout should have", () => {
  const html = `<div class="callout note" data-astro-cid-x><span class="tag" data-astro-cid-x>note</span><div class="body" data-astro-cid-x><p>text</p></div></div>`;
  assert.deepEqual(callouts(html), [
    { variant: "note", children: ["span.tag", "div.body"] },
  ]);
});

test("callouts() counts only direct children, not the prose below them", () => {
  const html = `<div class="callout careful"><span class="tag">careful</span><div class="body"><p>a <code>b</code></p><ul><li>c</li></ul></div></div>`;
  assert.deepEqual(callouts(html)[0].children, ["span.tag", "div.body"]);
});

test("callouts() sees the children a broken-out slot leaves behind", () => {
  // What a `<p>` wrapper produced once a caller passed paragraphs.
  const html = `<div class="callout note"><span class="tag">note</span><p></p><p>one</p><p>two</p></div>`;
  assert.deepEqual(callouts(html)[0].children, [
    "span.tag",
    "p",
    "p",
    "p",
  ]);
});

test("callouts() is not thrown off by a void element in the prose", () => {
  const html = `<div class="callout note"><span class="tag">note</span><div class="body">a<br>b</div></div>`;
  assert.deepEqual(callouts(html)[0].children, ["span.tag", "div.body"]);
});

test("extraItems() passes a callout whose slot stayed in its wrapper", () => {
  const html = `<div class="callout note"><span class="tag">note</span><div class="body"><p>a</p><p>b</p></div></div>`;
  assert.deepEqual(extraItems(html), []);
});

test("extraItems() reports a callout laying out loose paragraphs", () => {
  const html = `<div class="callout note"><span class="tag">note</span><p>one</p><p>two</p></div>`;
  assert.deepEqual(extraItems(html), ["note: span.tag p p"]);
});

test("pWrapped() reports a p wrapper even while it still holds", () => {
  // extraItems() reads two children here and is happy, and it stays happy
  // once a caller passes paragraphs, since they nest in the file and only
  // come apart in the browser. This is the check that sees it.
  const html = `<div class="callout note"><span class="tag">note</span><p>plain text</p></div>`;
  assert.deepEqual(pWrapped(html), ["note: span.tag p"]);
});

test("pWrapped() passes a wrapper that can hold a paragraph", () => {
  const html = `<div class="callout note"><span class="tag">note</span><div class="body"><p>a</p></div></div>`;
  assert.deepEqual(pWrapped(html), []);
});

test("every callout's markup holds exactly the tag and the body", async () => {
  for (const [page, html] of await docsPages()) {
    const bad = extraItems(html);
    assert.deepEqual(
      bad,
      [],
      `/docs/${page} has ${bad.length} callout(s) whose markup holds something other than the tag and the body: ${bad.join(" | ")}. Every extra child is a grid item of its own, and the tag's column grows to hold the widest of them, which leaves the prose column at zero and runs its text off the side of the page.`,
    );
  }
});

test("every callout keeps its prose where a paragraph can go", async () => {
  for (const [page, html] of await docsPages()) {
    const bad = pWrapped(html);
    assert.deepEqual(
      bad,
      [],
      `/docs/${page} has ${bad.length} callout(s) holding their prose in a <p>: ${bad.join(" | ")}. A <p> is closed by the parser the moment a caller passes one, and every paragraph after it becomes a grid item. That is how /docs/writing-a-dog came to scroll sideways on a phone, and the check above only catches it once some page is passing paragraphs.`,
    );
  }
});

test("every callout container is one the walk found", async () => {
  for (const [page, html] of await docsPages()) {
    const containers = [...html.matchAll(/<div class="callout[ "]/g)].length;
    assert.equal(
      callouts(html).length,
      containers,
      `/docs/${page} has ${containers} callout container(s) and ${callouts(html).length} the tag walk reached. A container it cannot reach is one the checks above never read.`,
    );
  }
});
