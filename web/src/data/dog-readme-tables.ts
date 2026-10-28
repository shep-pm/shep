/*
 * Giving a rendered markdown table's cells the headings its header row names.
 *
 * A fetched dog README renders its markdown tables as real `<table>` elements
 * (`dog-readme-fetch.ts` hands the README to Astro's own renderer). Every other
 * table on this site is a `<div class="row">` grid, and `docs-page.css`
 * collapses those to one column under 640px with the header row hidden and each
 * cell naming its own heading through `data-label`. A `<table>` gets none of
 * that for free: its header row is a `<thead>` those rules cannot reach, so on
 * a phone it either keeps its column floors and loses a column to horizontal
 * scroll, or it stacks and the reader cannot tell which value is which.
 *
 * `dog-readme.css` does the stacking. This file supplies what it stacks with, by
 * copying a header cell's text onto every cell past the first in its row. The
 * first cell is exempt for the reason `verify-stacked-table-labels.ts` gives: it
 * is the thing being described — a field name, an option, a metric — and it
 * reads on its own.
 *
 * A table with no header cells is left alone. There is nothing to copy, and a
 * label invented from a column number would name a heading nobody wrote.
 *
 * Why a tag walk rather than a renderer plugin: Astro 7's markdown pipeline does
 * take hast plugins, but wiring one means editing `astro.config.mjs` for the
 * whole site to style one kind of page, and a plugin is handed a tree this file
 * has no use for. A rendered markdown table is `<table><thead><tr><th>` all the
 * way down, so walking the tags says the same thing with no config and no new
 * dependency, in the shape `hiddenIds()` in `verify-fragment-targets.ts` already
 * uses — and it is what lets `verify-dog-readme.ts` test this with no Astro
 * build in the way.
 */

/** One cell's opening tag, located by index so `data-label` goes in in place. */
interface Cell {
  tag: "td" | "th";
  /** Index just past the tag name, where the attribute is inserted. */
  insertAt: number;
  /** Index just past the opening tag's `>`. */
  contentStart: number;
  /** Index where the cell's content ends: its closing tag, or its row's end. */
  contentEnd: number;
  /** True when the cell already carries a `data-label`, which is left alone. */
  labelled: boolean;
}

/** A half-open span of the document. */
interface Span {
  start: number;
  end: number;
}

/** A text to splice in at an index, leaving what was already there alone. */
interface Edit {
  at: number;
  text: string;
}

/** The character references a rendered cell's text can hold. */
const NAMED_ENTITIES = new Map([
  ["amp", "&"],
  ["apos", "'"],
  ["gt", ">"],
  ["lt", "<"],
  ["quot", '"'],
]);

/** A tag's name is over only at whitespace, `/` or `>`. */
function isTagBoundary(html: string, index: number): boolean {
  const char = html[index];
  return char === " " || char === "\t" || char === "\n" || char === "\r" || char === ">" || char === "/";
}

/** True at `<table`, `<table>` or `<table class=`, and not at `<tables`. */
function opensTable(html: string, at: number): boolean {
  return html.startsWith("table", at + 1) && isTagBoundary(html, at + 6);
}

/**
 * Index of the `>` that closes the tag starting at `start`, or -1 when it never
 * closes.
 *
 * A quoted attribute value may hold `>` — a README writing its own
 * `<td title="a > b">` is legal markdown — so the tag is walked a quoted run at
 * a time instead of searched for the first `>`.
 */
function tagEnd(html: string, start: number): number {
  let i = start + 1;
  while (i < html.length) {
    const char = html[i];
    if (char === ">") return i;
    if (char === '"' || char === "'") {
      const close = html.indexOf(char, i + 1);
      if (close === -1) return -1;
      i = close + 1;
      continue;
    }
    i += 1;
  }
  return -1;
}

/**
 * Index just past the `</table>` that closes the `<table>` at `start`.
 *
 * Counts opens and closes so a table nested inside a cell ends at its own
 * `</table>` and the outer one is still found.
 */
function tableEnd(html: string, start: number): number {
  let depth = 0;
  let i = start;
  while (i < html.length) {
    const close = html.indexOf("</table", i);
    if (close === -1) return html.length;
    const open = html.indexOf("<table", i);
    if (open !== -1 && open < close && opensTable(html, open)) {
      depth += 1;
      i = open + 6;
      continue;
    }
    depth -= 1;
    if (depth === 0) {
      const gt = tagEnd(html, close);
      return gt === -1 ? html.length : gt + 1;
    }
    i = close + 7;
  }
  return html.length;
}

/**
 * The next tag named in `names` inside `[from, limit)`, or null.
 *
 * A `<table>` met on the way is jumped over whole, so a nested table's rows and
 * cells belong to its own pass below and never read as the outer table's. A
 * closing tag never matches, because `</td` does not start with `td`.
 */
function nextTag(html: string, names: string[], from: number, limit: number): { name: string; at: number } | null {
  let i = from;
  while (i < limit) {
    const lt = html.indexOf("<", i);
    if (lt === -1 || lt >= limit) return null;
    if (opensTable(html, lt)) {
      i = Math.min(tableEnd(html, lt), limit);
      continue;
    }
    for (const name of names) {
      if (html.startsWith(name, lt + 1) && isTagBoundary(html, lt + 1 + name.length)) {
        return { name, at: lt };
      }
    }
    i = lt + 1;
  }
  return null;
}

/** The next `</name>` inside `[from, limit)` that is not inside a nested table, or -1. */
function closingTag(html: string, name: string, from: number, limit: number): number {
  let i = from;
  while (i < limit) {
    const close = html.indexOf(`</${name}`, i);
    if (close === -1 || close >= limit) return -1;
    const open = html.indexOf("<table", i);
    if (open !== -1 && open < close && opensTable(html, open)) {
      i = tableEnd(html, open);
      continue;
    }
    return close;
  }
  return -1;
}

/**
 * Every `<table>` in the document, a nested one included, as a half-open span.
 *
 * A table inside a cell is its own entry, because it has its own header row and
 * `dog-readme.css` stacks it on a phone like any other: left unlabelled, its
 * cells read as values with nothing naming their column. The scan moves past the
 * opening tag rather than to the closing one so a nested table is met, and
 * `rows()` jumps it for the outer table, so no cell is read as a column of both.
 */
function tables(html: string): Span[] {
  const found: Span[] = [];
  let i = 0;
  while (i < html.length) {
    const start = html.indexOf("<table", i);
    if (start === -1) break;
    if (!opensTable(html, start)) {
      i = start + 6;
      continue;
    }
    const end = tableEnd(html, start);
    found.push({ start, end });
    const gt = tagEnd(html, start);
    i = gt === -1 || gt >= end ? end : gt + 1;
  }
  return found;
}

/** The rows of one table, in document order. */
function rows(html: string, table: Span): Span[] {
  const found: Span[] = [];
  // Past the table's own opening tag: nextTag() would otherwise read it as a
  // nested table and jump the whole span.
  const gt = tagEnd(html, table.start);
  let i = gt === -1 ? table.end : gt + 1;
  for (;;) {
    const open = nextTag(html, ["tr"], i, table.end);
    if (open === null) break;
    const close = closingTag(html, "tr", open.at, table.end);
    const gt = close === -1 ? -1 : tagEnd(html, close);
    const end = gt === -1 ? table.end : Math.min(gt + 1, table.end);
    found.push({ start: open.at, end });
    i = end;
  }
  return found;
}

/** The cells of one row, in document order. */
function cells(html: string, row: Span): Cell[] {
  const found: Cell[] = [];
  let i = row.start;
  for (;;) {
    const open = nextTag(html, ["td", "th"], i, row.end);
    if (open === null) break;
    const tag = open.name as "td" | "th";
    const gt = tagEnd(html, open.at);
    if (gt === -1) break;
    const close = closingTag(html, tag, gt + 1, row.end);
    found.push({
      tag,
      insertAt: open.at + 1 + tag.length,
      contentStart: gt + 1,
      contentEnd: close === -1 ? row.end : close,
      labelled: /\bdata-label\s*=/.test(html.slice(open.at + 1 + tag.length, gt)),
    });
    i = gt + 1;
  }
  return found;
}

/** Decode the character references in a cell's text so `Q&amp;a` reads as `Q&a`. */
function decodeCharacterReferences(text: string): string {
  if (!text.includes("&")) return text;
  return text.replace(/&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g, (whole, body: string) => {
    if (body.startsWith("#")) {
      const hex = body[1] === "x" || body[1] === "X";
      const code = Number.parseInt(hex ? body.slice(2) : body.slice(1), hex ? 16 : 10);
      if (Number.isNaN(code) || code < 0 || code > 0x10ffff) return whole;
      return String.fromCodePoint(code);
    }
    return NAMED_ENTITIES.get(body.toLowerCase()) ?? whole;
  });
}

/**
 * A header cell's text, as an attribute value: tags stripped, whitespace
 * collapsed, and re-escaped for inside a double-quoted `data-label`.
 *
 * `&` is escaped before `"` because the quote escape writes one, the same order
 * `dog-readme-links.ts` uses for the same reason.
 */
function cellLabel(html: string, cell: Cell): string {
  const text = decodeCharacterReferences(
    html.slice(cell.contentStart, cell.contentEnd).replace(/<[^>]*>/g, ""),
  )
    .replace(/\s+/g, " ")
    .trim();
  return text.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
}

/**
 * The label each column carries, indexed by column.
 *
 * Read off the table's first row, which is where a markdown table's header row
 * always is. A `td` in that row and a column it never names both come back
 * `undefined`: there is no heading written for them to copy.
 */
function columnLabels(html: string, firstRow: Span): (string | undefined)[] {
  return cells(html, firstRow).map((cell) => (cell.tag === "th" ? cellLabel(html, cell) : undefined));
}

/**
 * Queue a `data-label` for every cell past the first in one table, taken from
 * that column's header cell.
 *
 * @param edits the insertions found so far, appended to in place.
 */
function labelTable(html: string, table: Span, edits: Edit[]): void {
  const found = rows(html, table);
  if (found.length === 0) return;
  const labels = columnLabels(html, found[0]);
  // No `th` in the header row is a table with no header cells, and there is
  // nothing on it to name a column.
  if (!labels.some((label) => label !== undefined)) return;

  for (const row of found.slice(1)) {
    const rowCells = cells(html, row);
    for (let column = 1; column < rowCells.length; column += 1) {
      const cell = rowCells[column];
      const label = labels[column];
      // A `th` in a body row is a row header and names itself. A column the
      // header row left blank has nothing to copy. A cell the README already
      // labelled keeps its own.
      if (cell.tag !== "td" || label === undefined || label === "" || cell.labelled) continue;
      edits.push({ at: cell.insertAt, text: ` data-label="${label}"` });
    }
  }
}

/**
 * Label the cells of every table in rendered HTML.
 *
 * Everything outside a cell's opening tag is copied out byte for byte, so a
 * README that already labels or styles its own cells is unchanged.
 */
export function labelTableCells(html: string): string {
  const edits: Edit[] = [];
  for (const table of tables(html)) labelTable(html, table, edits);
  if (edits.length === 0) return html;

  // Back to front, so each insertion leaves every index before it pointing at
  // the character it did.
  let out = html;
  for (const edit of edits.sort((a, b) => b.at - a.at)) {
    out = out.slice(0, edit.at) + edit.text + out.slice(edit.at);
  }
  return out;
}
