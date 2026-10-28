// Regression guard for the definition tables on a phone.
//
// The tables this site writes are a grid with a header row on top. Under 640px
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
// its cells. Each one is run twice, once over the site's own `div.row.header`
// grids and once over a real `<table>`, because the rule is about the header
// row going away and not about which markup carries it.
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
//
// A second kind of table is in the build now, and it is the reason the walk
// reaches every depth under `dist/docs`. A fetched dog README renders its
// markdown tables as real `<table>` elements, which the two checks above cannot
// see at all: there is no `div.row.header` in one. `dog-readme-tables.ts`
// labels their cells and `dog-readme.css` stacks them, and this is what proves
// both happened — over the built page, where a label that never got spliced in
// or a rule that never got emitted is visible instead of assumed. `/docs/dog-grants`
// writes its own `<table>` by hand and is read by the same two checks.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { builtDocsPages, distDir } from "./built-pages.ts";

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

/** One open tag found directly inside a span of the document. */
interface Element {
  /** Index of its `<`. */
  start: number;
  /** Index just past its matching closing tag. */
  end: number;
  /** Its opening tag's attributes, `data-label` among them. */
  attrs: string;
  /** Index just past its opening tag's `>`. */
  bodyStart: number;
  /** Index of its matching closing tag's `<`. */
  bodyEnd: number;
}

/** A cell of a `<table>`: its tag, its attributes, and its text. */
interface Cell {
  tag: string;
  attrs: string;
  /** Tags stripped and whitespace collapsed, for naming the cell in a message. */
  text: string;
}

/** A `<table>` element on the page. */
export interface ElementTable {
  /** The table's own markup, opening tag to closing tag. */
  html: string;
  /** Its rows, and each row's cells, this table's own only. */
  rows: { cells: Cell[] }[];
  /** True when its first row holds a `th`, which is the row a phone hides. */
  headed: boolean;
}

/**
 * The tags in `family` sitting directly inside `[from, to)`, in document order.
 *
 * Counts opens and closes, so a tag of the same family inside another stays
 * with the outer one: a nested `<table>`, the `<tr>` belonging to it, and a
 * `<td>` inside a `<td>` all read as part of the cell holding them, which is
 * how `labelTableCells()` treats them too.
 *
 * An attribute value holding a `>` throws the walk off, the same limit
 * verify-callout-children.ts records. None of these pages has one, and the last
 * test in this file is what notices if that stops being true.
 */
function direct(html: string, from: number, to: number, family: string): Element[] {
  const out: Element[] = [];
  const tag = new RegExp(`<(\\/)?(${family})\\b([^>]*)>`, "g");
  tag.lastIndex = from;
  let depth = 0;
  let open: RegExpExecArray | null = null;
  let m: RegExpExecArray | null;
  while ((m = tag.exec(html)) !== null && m.index < to) {
    if (m[1] === "/") {
      if (depth === 0) break;
      depth -= 1;
      if (depth === 0 && open !== null) {
        out.push({
          start: open.index,
          end: m.index + m[0].length,
          attrs: open[3],
          bodyStart: open.index + open[0].length,
          bodyEnd: m.index,
        });
        open = null;
      }
    } else {
      if (depth === 0) open = m;
      depth += 1;
    }
  }
  return out;
}

/**
 * Every `<table>` on the page, a table nested in a cell included.
 *
 * A nested table is its own entry for the reason `dog-readme-tables.ts` gives
 * for labelling one: it has its own header row, and `dog-readme.css` stacks it
 * on a phone like any other.
 */
export function elementTables(html: string): ElementTable[] {
  const found: ElementTable[] = [];
  const walk = (from: number, to: number): void => {
    for (const table of direct(html, from, to, "table")) {
      const rows = direct(html, table.bodyStart, table.bodyEnd, "tr").map((row) => ({
        cells: direct(html, row.bodyStart, row.bodyEnd, "td|th").map((cell) => ({
          tag: html.slice(cell.start + 1, cell.start + 3),
          attrs: cell.attrs,
          text: html
            .slice(cell.bodyStart, cell.bodyEnd)
            .replace(/<[^>]*>/g, "")
            .replace(/\s+/g, " ")
            .trim(),
        })),
      }));
      found.push({
        html: html.slice(table.start, table.end),
        rows,
        headed: rows.length > 0 && rows[0].cells.some((c) => c.tag === "th"),
      });
      walk(table.bodyStart, table.bodyEnd);
    }
  };
  walk(0, html.length);
  return found;
}

/** A table's first header cell, for naming it in a message. */
function headerLabel(table: ElementTable): string {
  const cell = table.rows[0]?.cells[0];
  return cell === undefined || cell.text === "" ? "table" : cell.text;
}

/**
 * `<table>` cells past the first in a body row that carry no `data-label`, as
 * `table: text`.
 *
 * The exemptions are the ones `labelTableCells()` works with, for the same
 * reasons: the header row is the thing a phone hides, so its cells name the
 * columns instead of carrying a copy of themselves, and a `th` in a body row is
 * a row header that names itself.
 */
export function unlabelledElementCells(html: string): string[] {
  const missing: string[] = [];
  for (const table of elementTables(html)) {
    if (!table.headed) continue;
    for (const row of table.rows.slice(1)) {
      for (const [index, cell] of row.cells.entries()) {
        if (index === 0 || cell.tag === "th") continue;
        if (/\sdata-label="/.test(cell.attrs)) continue;
        missing.push(`table: ${cell.text.slice(0, 40)}`);
      }
    }
  }
  return missing;
}

/**
 * `<table>` elements on a page that has no phone-width rule stacking one, as
 * their header labels.
 *
 * Stacking means the table, its rows and its cells all become blocks and its
 * header row goes away, which is what `dog-readme.css` does under 640px, and
 * `phoneStacksTables()` requires both halves. Read per page rather than per
 * table because the rule belongs to a stylesheet: one phone block covers every
 * table the page has.
 */
export function unstackedElementTables(html: string, css: string): string[] {
  const headed = elementTables(html).filter((t) => t.headed);
  if (headed.length === 0) return [];
  return phoneStacksTables(css) ? [] : headed.map(headerLabel);
}

/**
 * True when the selector's subject, the element it actually paints, is `name`.
 *
 * The subject is the last compound selector, the one after every combinator:
 * `.dog-readme table code` paints `code` and `.dog-readme table` paints
 * `table`. A compound carrying its own modifiers still names its element
 * first, which is how Astro's scoped output reads (`table:where(.astro-cid-x)`).
 * A subject wrapped in `:is()` or `:where()` is not read as that element, and
 * no sheet on this site writes one.
 */
function targetsElement(selector: string, name: string): boolean {
  const compounds = selector.split(/[\s>+~]+/).filter(Boolean);
  const subject = compounds[compounds.length - 1] ?? "";
  return new RegExp(`^${name}(?![\\w-])`).test(subject);
}

/**
 * True when some phone-width block both stacks `<table>` and hides `<thead>`.
 *
 * Both halves are the rule, and the label check above only makes sense once
 * the second one holds: the header row is what goes away, and each cell's
 * `data-label` is what replaces it. A rule whose subject is not the element is
 * not a rule about it either, so `.dog-readme table code{display:block}`
 * restyles the code inside a table and leaves the table laid out as a table.
 */
function phoneStacksTables(css: string): boolean {
  let stacked = false;
  let headHidden = false;
  for (const rule of phoneRules(css).matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = rule[1].split(",").map((s) => s.trim());
    if (/display:\s*block/.test(rule[2])) {
      stacked ||= selectors.some((s) => targetsElement(s, "table"));
    }
    if (/display:\s*none/.test(rule[2])) {
      headHidden ||= selectors.some((s) => targetsElement(s, "thead"));
    }
  }
  return stacked && headHidden;
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

// The page list used to be a `readdir` of `dist/docs` here, one level deep. It
// is builtDocsPages() in built-pages.ts now, which walks every depth.

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

/** A README's table, labelled the way `labelTableCells()` leaves it. */
const DOG_TABLE = `<table><thead><tr><th>Flag</th><th>Does</th></tr></thead><tbody><tr><td><code>--yes</code></td><td data-label="Does">skips the prompt</td></tr></tbody></table>`;

/** The phone block `dog-readme.css` writes, minified the way Astro emits it. */
const STACKING_CSS = `@media (width<=640px){.dog-readme table,.dog-readme tbody,.dog-readme tr,.dog-readme td{display:block}.dog-readme thead{display:none}}`;

test("unstackedElementTables() catches a dog page whose stylesheet never stacks table", () => {
  assert.deepEqual(
    unstackedElementTables(DOG_TABLE, `@media (width<=640px){.dog-readme p{color:red}}`),
    ["Flag"],
  );
});

test("unstackedElementTables() catches a dog page that stacks table and keeps its thead", () => {
  // One column, header row still on screen: the reader sees `Flag` and `Does`
  // above a run of stacked values that also carry them as `data-label`.
  assert.deepEqual(
    unstackedElementTables(
      DOG_TABLE,
      `@media (width<=640px){.dog-readme table{display:block}}`,
    ),
    ["Flag"],
  );
});

test("unstackedElementTables() catches a phone rule about something inside the table", () => {
  assert.deepEqual(
    unstackedElementTables(
      DOG_TABLE,
      `@media (width<=640px){.dog-readme table code{display:block}.dog-readme thead{display:none}}`,
    ),
    ["Flag"],
  );
});

test("unstackedElementTables() passes a dog page whose stylesheet stacks table and hides its thead", () => {
  assert.deepEqual(unstackedElementTables(DOG_TABLE, STACKING_CSS), []);
});

test("unlabelledElementCells() catches a dog README table whose cells lost their labels", () => {
  const readme = `<table><thead><tr><th>Flag</th><th>Does</th></tr></thead><tbody><tr><td><code>--yes</code></td><td>skips the prompt</td></tr></tbody></table>`;
  assert.deepEqual(unlabelledElementCells(readme), ["table: skips the prompt"]);
});

test("unlabelledElementCells() passes a dog README table labelled from its header row", () => {
  const readme = `<table><thead><tr><th>Flag</th><th>Does</th></tr></thead><tbody><tr><td><code>--yes</code></td><td data-label="Does">skips the prompt</td></tr></tbody></table>`;
  assert.deepEqual(unlabelledElementCells(readme), []);
});

test("elementTables() reaches a table nested in a cell and keeps its cells its own", () => {
  const html = `<table><thead><tr><th>Outer</th><th>Col</th></tr></thead><tbody><tr><td><table><thead><tr><th>In</th><th>Out</th></tr></thead><tbody><tr><td>a</td><td>b</td></tr></tbody></table></td><td>tail</td></tr></tbody></table>`;
  assert.equal(elementTables(html).length, 2);
  assert.deepEqual(unlabelledElementCells(html), ["table: tail", "table: b"]);
});

test("every table with a header row stacks under 640px", async () => {
  for (const [route, html] of await builtDocsPages()) {
    const left = unstacked(html, await styles(html));
    assert.deepEqual(
      left,
      [],
      `/docs/${route} has ${left.length} table(s) whose header row is still on screen at 640px: ${left.join(" | ")}. A table that does not stack keeps its column floors, runs wider than the container, and loses the right-hand column to the overflow that rounds its corners.`,
    );
  }
});

test("every stacking table labels the cells a hidden header would have named", async () => {
  for (const [route, html] of await builtDocsPages()) {
    const missing = unlabelled(html);
    assert.deepEqual(
      missing,
      [],
      `/docs/${route} has ${missing.length} cell(s) with no data-label: ${missing.slice(0, 4).join(" | ")}. Under 640px the header row hides, so each of these stacks as a value with nothing naming it.`,
    );
  }
});

test("every <table> with a header row has a phone-width rule stacking it", async () => {
  for (const [route, html] of await builtDocsPages()) {
    const left = unstackedElementTables(html, await styles(html));
    assert.deepEqual(
      left,
      [],
      `/docs/${route} has ${left.length} <table> element(s) nothing stacks at 640px: ${left.join(" | ")}. A <table> that does not stack keeps its column floors and loses a column to horizontal scroll. src/styles/dog-readme.css has the rule for an imported page and src/styles/docs-page.css for a page written here; a new table needs one of them.`,
    );
  }
});

test("every stacking <table> labels the cells a hidden header would have named", async () => {
  for (const [route, html] of await builtDocsPages()) {
    const missing = unlabelledElementCells(html);
    assert.deepEqual(
      missing,
      [],
      `/docs/${route} has ${missing.length} <table> cell(s) past the first with no data-label: ${missing.slice(0, 4).join(" | ")}. Under 640px its header row hides, so each of these stacks as a value with nothing naming it. On an imported dog page src/data/dog-readme-tables.ts is what labels these and did not reach this one; a column its header row left blank needs a heading in the README.`,
    );
  }
});

test("every header row opens its table, and every <table> is one the walk reached", async () => {
  for (const [route, html] of await builtDocsPages()) {
    const headers = [...html.matchAll(/<div class="row header"/g)].length;
    assert.equal(
      tables(html).length,
      headers,
      `/docs/${route} has ${headers} header row(s) and ${tables(html).length} reachable table(s). tables() finds a table by the container tag immediately before its header row, so a header row that is not its table's first child is invisible to both checks above.`,
    );
    const opened = [...html.matchAll(/<table(?=[\s/>])/g)].length;
    assert.equal(
      elementTables(html).length,
      opened,
      `/docs/${route} opens ${opened} <table> tag(s) and ${elementTables(html).length} elementTables() reached. A table it cannot reach is one the two checks above never read, which is also what an unclosed table or an attribute holding a bare > looks like.`,
    );
  }
});
