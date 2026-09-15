// Regression guard for the definition tables on a phone.
//
// Every one of these tables is a grid with a header row on top. Under 640px
// the grid collapses to one column, the header row hides, and each cell
// carries its own heading as a `data-label`. Two things go wrong, and each
// one had already gone wrong on four or five pages before it was caught:
//
//   1. The cells lose their headings. `/docs/first-flockfile` stacked a field
//      name, the bare word `required`, and a sentence, with nothing saying
//      that the middle one was the default.
//   2. The table never stacks at all. Its columns keep their floors, the row
//      runs wider than the container, and the `overflow: hidden` that rounds
//      the corners takes the right-hand column away with no scrollbar to get
//      it back. `/docs/whistle` wanted 558px and had 321.
//
// So there are two checks here, and they are one rule read from both ends: a
// table with a header row stacks on a phone, and a table that stacks labels
// its cells.
//
// Neither check names a table class. The first version of this file did, and
// the list was already wrong when it was written: it covered `.field-table`
// and not the `.metrics-table` that was cutting a column off four tables on
// `/docs/writing-a-dog`. The header row is what these key on instead, since
// it is the thing that goes away.
//
// Runs over the BUILT pages rather than the `.astro` sources, for the reason
// verify-fragment-targets.ts gives: `.astro` is not HTML. It reads the built
// CSS too, both the inline `<style>` blocks Astro puts in each page and the
// sheets that page links.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const distDir = fileURLToPath(new URL("../dist", import.meta.url));

/** A definition table: the classes on its container, and its markup. */
interface Table {
  /** The container's classes, minus the `row`/`header` a cell would carry. */
  classes: string[];
  /** The container's own tag through to its closing `</div>`. */
  html: string;
}

/**
 * Every table on the page, found by its header row.
 *
 * A header row is always the first child of its table, so the container is
 * the tag immediately before it. That is an assumption rather than a rule
 * Astro enforces, which is what the last test in this file is for: it counts
 * header rows against tables found, so a header row that moves takes the
 * whole page out of both checks above and says so.
 */
export function tables(html: string): Table[] {
  const found: Table[] = [];
  const open = /<div class="([^"]*)"[^>]*>\s*<div class="row header"/g;
  for (const start of html.matchAll(open)) {
    // Rows are flat inside a table, so the table ends at the first `</div>`
    // that closes it: count opens and closes from the container's own tag.
    let i = start.index + start[0].length;
    let depth = 2;
    while (depth > 0) {
      const next = /<\/?div\b[^>]*>/g;
      next.lastIndex = i;
      const m = next.exec(html);
      if (!m) break;
      depth += m[0].startsWith("</div") ? -1 : 1;
      i = m.index + m[0].length;
    }
    const classes = start[1]
      .split(/\s+/)
      .filter((c) => c && c !== "row" && c !== "header");
    found.push({ classes, html: html.slice(start.index, i) });
  }
  return found;
}

/**
 * Cells past the first that stack without a heading, as `table: text`.
 *
 * The first cell of a row is exempt. It is the thing being described, a field
 * name or a program name or a metric, so it reads on its own, and that is the
 * convention boot-order set before any of this was enforced.
 */
export function unlabelled(html: string): string[] {
  const missing: string[] = [];
  for (const table of tables(html)) {
    const rows = table.html.matchAll(
      /<div class="row(?! header)[^"]*"[^>]*>([\s\S]*?)<\/div>\s*(?=<div class="row|<\/div>)/g,
    );
    for (const row of rows) {
      const cells = [...row[1].matchAll(/<span\b([^>]*)>([\s\S]*?)<\/span>/g)];
      for (const [index, cell] of cells.entries()) {
        if (index === 0) continue;
        if (/\sdata-label="/.test(cell[1])) continue;
        const text = cell[2].replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim();
        missing.push(`${table.classes.join(".")}: ${text.slice(0, 40)}`);
      }
    }
  }
  return missing;
}

/**
 * The declarations inside every phone-width media block in some CSS.
 *
 * Astro's build minifies `@media (max-width: 640px)` to `@media
 * (width<=640px)`, so both spellings count.
 */
function phoneRules(css: string): string {
  let out = "";
  const at = /@media([^{]*)\{/g;
  for (const m of css.matchAll(at)) {
    const prelude = m[1].replace(/\s+/g, "");
    if (!/(max-width:640px|width<=640px)/.test(prelude)) continue;
    let i = m.index + m[0].length;
    let depth = 1;
    while (depth > 0 && i < css.length) {
      if (css[i] === "{") depth++;
      else if (css[i] === "}") depth--;
      i++;
    }
    out += css.slice(m.index + m[0].length, i);
  }
  return out;
}

/** Tables whose header row survives a phone, so their columns are cut off. */
export function unstacked(html: string, css: string): string[] {
  const phone = phoneRules(css);
  const hidden = [...phone.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
    .filter((r) => /display:\s*none/.test(r[2]))
    .map((r) => r[1]);
  return tables(html)
    .filter(
      (t) =>
        !hidden.some(
          (sel) =>
            sel.includes(".header") &&
            t.classes.some((c) => sel.includes(`.${c}`)),
        ),
    )
    .map((t) => t.classes.join("."));
}

/** A page's CSS: the styles Astro inlines, plus the sheets the page links. */
async function styles(html: string): Promise<string> {
  const inline = [...html.matchAll(/<style>([\s\S]*?)<\/style>/g)].map(
    (m) => m[1],
  );
  const linked = [...html.matchAll(/<link rel="stylesheet" href="([^"]+)"/g)]
    .map((m) => m[1])
    .filter((href) => href.startsWith("/"));
  const sheets = await Promise.all(
    linked.map((href) => readFile(`${distDir}${href}`, "utf8")),
  );
  return [...inline, ...sheets].join("\n");
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

test("tables() finds a table by its header row, whatever it is called", () => {
  const html = `<div class="wire-table" data-astro-cid-x><div class="row header"><span>A</span></div><div class="row"><span>b</span></div></div>`;
  assert.deepEqual(
    tables(html).map((t) => t.classes),
    [["wire-table"]],
  );
});

test("unlabelled() exempts the first cell and reports the rest", () => {
  const html = `<div class="field-table"><div class="row header"><span>A</span><span>B</span></div><div class="row"><span>name</span><span>no label here</span></div></div>`;
  assert.deepEqual(unlabelled(html), ["field-table: no label here"]);
});

test("unlabelled() accepts a row whose later cells are labelled", () => {
  const html = `<div class="field-table"><div class="row header"><span>A</span><span>B</span></div><div class="row"><span>name</span><span data-label="Default">required</span></div></div>`;
  assert.deepEqual(unlabelled(html), []);
});

test("unstacked() reads a hide rule through the table's own class", () => {
  const html = `<div class="verb-table"><div class="row header"><span>A</span></div></div>`;
  const css = `@media (width<=640px){.verb-table[data-astro-cid-x] .row[data-astro-cid-x].header{display:none}}`;
  assert.deepEqual(unstacked(html, css), []);
});

test("unstacked() reports a table nothing hides", () => {
  const html = `<div class="verb-table"><div class="row header"><span>A</span></div></div>`;
  assert.deepEqual(unstacked(html, `@media (width<=640px){.p{color:red}}`), [
    "verb-table",
  ]);
});

test("unstacked() ignores a hide rule outside a phone-width block", () => {
  const html = `<div class="verb-table"><div class="row header"><span>A</span></div></div>`;
  const css = `.verb-table .row.header{display:none}`;
  assert.deepEqual(unstacked(html, css), ["verb-table"]);
});

test("every table with a header row stacks under 640px", async () => {
  for (const [page, html] of await docsPages()) {
    const left = unstacked(html, await styles(html));
    assert.deepEqual(
      left,
      [],
      `/docs/${page} has ${left.length} table(s) whose header row is still on screen at 640px: ${left.join(" | ")}. A table that does not stack keeps its column floors, runs wider than the container, and loses the right-hand column to the overflow that rounds its corners.`,
    );
  }
});

test("every stacking table labels the cells a hidden header would have named", async () => {
  for (const [page, html] of await docsPages()) {
    const missing = unlabelled(html);
    assert.deepEqual(
      missing,
      [],
      `/docs/${page} has ${missing.length} cell(s) with no data-label: ${missing.slice(0, 4).join(" | ")}. Under 640px the header row hides, so each of these stacks as a value with nothing naming it.`,
    );
  }
});

test("every header row opens its table", async () => {
  for (const [page, html] of await docsPages()) {
    const headers = [...html.matchAll(/<div class="row header"/g)].length;
    assert.equal(
      tables(html).length,
      headers,
      `/docs/${page} has ${headers} header row(s) and ${tables(html).length} reachable table(s). tables() finds a table by the container tag immediately before its header row, so a header row that is not its table's first child is invisible to both checks above.`,
    );
  }
});
