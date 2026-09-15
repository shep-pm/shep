// Regression guard for the tables that stack on a phone.
//
// `.field-table`, and from-pm2's `.verb-table`/`.bench-table`, all collapse to
// one column at 640px and hide their header row. A cell that loses its heading
// and carries no `data-label` becomes a value with nothing saying what it is:
// `/docs/first-flockfile` stacked a field name, the bare word `required`, and
// a sentence, and `/docs/from-pm2` stacked two spellings with no way to tell
// pm2's from shep's.
//
// The first cell of a row is exempt. It is the thing being described (a field
// name, a program name, a metric), so it reads on its own, and that is the
// convention boot-order set before any of this was enforced.
//
// Four pages hit this one at a time: examples, from-pm2 twice, then
// first-flockfile. Fourth instance is where a check is cheaper than a fifth
// review comment.
//
// Runs over the BUILT pages rather than the `.astro` sources, for the reason
// verify-fragment-targets.ts gives: `.astro` is not HTML.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const distDir = fileURLToPath(new URL("../dist/docs", import.meta.url));

/**
 * Tables whose header row is hidden under 640px, so their cells need labels.
 *
 * `.field-table` only, because the shared sheet is the one place that hides a
 * header unconditionally. from-pm2 hides its own `.verb-table`/`.bench-table`
 * headers in a scoped block and labels those cells beside it; listing those
 * two classes here instead flagged /docs/lifecycle and /docs/pm2-verbs, whose
 * verb tables keep three columns and a visible header at 375px and need no
 * labels at all.
 */
const STACKING = ["field-table"];

/** Cells past the first that stack without a heading, as `table: text`. */
export function unlabelled(html: string): string[] {
  const missing: string[] = [];
  for (const table of STACKING) {
    const open = new RegExp(`<div class="[^"]*\\b${table}\\b[^"]*"[^>]*>`, "g");
    for (const start of html.matchAll(open)) {
      // Rows are flat inside a table, so the table ends at the first `</div>`
      // that closes it: count opens and closes from the table's own tag.
      let i = start.index + start[0].length;
      let depth = 1;
      while (depth > 0) {
        const next = /<\/?div\b[^>]*>/g;
        next.lastIndex = i;
        const m = next.exec(html);
        if (!m) break;
        depth += m[0].startsWith("</div") ? -1 : 1;
        i = m.index + m[0].length;
      }
      const body = html.slice(start.index, i);
      for (const row of body.matchAll(/<div class="row(?! header)[^"]*"[^>]*>([\s\S]*?)<\/div>\s*(?=<div class="row|<\/div>)/g)) {
        const cells = [...row[1].matchAll(/<span\b([^>]*)>([\s\S]*?)<\/span>/g)];
        for (const [index, cell] of cells.entries()) {
          if (index === 0) continue;
          if (/\sdata-label="/.test(cell[1])) continue;
          const text = cell[2].replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim();
          missing.push(`${table}: ${text.slice(0, 40)}`);
        }
      }
    }
  }
  return missing;
}

test("unlabelled() exempts the first cell and reports the rest", () => {
  const html = `<div class="field-table"><div class="row header"><span>A</span><span>B</span></div><div class="row"><span>name</span><span>no label here</span></div></div>`;
  assert.deepEqual(unlabelled(html), ["field-table: no label here"]);
});

test("unlabelled() accepts a row whose later cells are labelled", () => {
  const html = `<div class="field-table"><div class="row"><span>name</span><span data-label="Default">required</span></div></div>`;
  assert.deepEqual(unlabelled(html), []);
});

test("every stacking table labels the cells a hidden header would have named", async () => {
  const dirs = await readdir(distDir, { withFileTypes: true });
  const pages = dirs.filter((d) => d.isDirectory()).map((d) => d.name);
  assert.ok(pages.length > 0, `no built pages under ${distDir}; run astro build first`);
  for (const page of pages) {
    const html = await readFile(`${distDir}/${page}/index.html`, "utf8");
    const missing = unlabelled(html);
    assert.deepEqual(
      missing,
      [],
      `/docs/${page} has ${missing.length} cell(s) with no data-label: ${missing.slice(0, 4).join(" | ")}. Under 640px the header row hides, so each of these stacks as a value with nothing naming it.`,
    );
  }
});
