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
//
// What a page measures is not written in this file. It used to be a `// 1659`
// beside every ceiling, and 16 of 29 of those were stale on 2026-10-19, each
// one reading lower than the page measured: `from-pm2` showed 61 words of room
// and had 3. The comment was believed and the script was not run twice, which
// is #596. The report at the bottom prints the count and the words left under
// the ceiling, and a test below refuses a measured count written here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const pagesDir = fileURLToPath(new URL("../src/pages/docs", import.meta.url));

/**
 * Maximum prose words per page, by slug, at what each measures plus about
 * five percent.
 *
 * A number here only ever comes down. It came down across the board on
 * 2026-09-14, when the "Where to go next" cards were cut from all 25 pages
 * that carried them: 539 lines and the two-card pitch at the foot of each
 * chapter. The chapter bar already says what comes next, and the sidebar
 * lists every chapter on every page.
 *
 * Two pages had no entry at all until that pass. `lookout-config` and
 * `pm2-verbs` shipped after the phase that seeded this map and nothing
 * noticed, which is what the "every page has a budget" test below is for.
 * `lookout` is the other tell: it sat at 5750 against a page measuring
 * 2629, a ceiling so far above the text that it could not have failed.
 *
 * Raising one is allowed and has happened twice, both on 2026-09-14, both
 * for a correction rather than for prose: `getting-started` for the note
 * saying `./server` is the reader's own binary, and `from-pm2` for the
 * clause saying which of the three binaries 17.94 MiB is. Say why in a
 * comment when you do it, and say no number for the page's own count: the
 * report prints the one it just measured.
 */
export const BUDGETS: Record<string, number> = {
  // Raised from 4150 on 2026-09-27 for the per-sheep dog settings section:
  // the page had already drifted to 4131 against a ceiling recorded at
  // 4042, so almost none of the stated headroom was real, and the new
  // section (DogSheepSettings, parse_sheep_settings, config.sheep.<dog>)
  // is required content, not prose to trim back out.
  //
  // Raised again from 4200 the same day for "Publishing a per-sheep
  // schema": probe_with_sheep is the write side of that same feature, and
  // it needed its own section rather than a sentence borrowed from
  // somebody else's.
  //
  // Raised from 4340 on 2026-09-30 for "Asking for the shepherd channel":
  // `shep-channel: true` is a new line a dog author has to be told about,
  // and the page had 15 words of headroom.
  //
  // Raised from 4450 on 2026-10-13 for `DogConfigResolved`: a dog author
  // has to be told which request resolves a `{{secret:...}}` and which
  // one still gives the text as written.
  //
  // Raised from 4470 for the config pane's bounds sentence: the pane now
  // holds an integer to `exclusiveMinimum`/`exclusiveMaximum` too, and a
  // range that admits nothing is said once, which is what a dog author
  // publishing those keywords gets told about the editor.
  //
  // Raised from 4490 on 2026-10-23 for `SheepDogSettingsResolved`: a dog
  // author keeping a per-sheep credential in a sheep's table has to be told
  // which request resolves it.
  //
  // Raised from 4520 on 2026-10-23 for `$SHEP_DOG_FD`: the connection a dog
  // is spawned holding is a third variable shep sets, and a dog not on
  // `DogRuntime` has to be told to read it.
  //
  // Raised again on 2026-10-24 for `--shep-manifest`: `shep adopt` now
  // asks it first, caches the answer by the binary's sha256 and warns about
  // `depends`, which a dog author has to be told before the two older flags.
  //
  // Raised again the same day for an adopted dog inside shep's own listing:
  // `shep --help` prints a manifest's `verb.description` beside the dog's
  // name, `shep <dog> --help` prints its declared subcommands, and
  // `shep completions` offers both. An operator sees all three, and a dog
  // author writes the manifest that drives them.
  //
  // Raised again from 4705 the same day for the `defaults` write: `shep
  // adopt` filling `dogs.toml` from the manifest is what a dog author ships
  // defaults with, and it retires the `--print-config`-style flag (#356).
  //
  // Raised again the same day, to 4875, for what that surface refuses: a
  // manifest's copy prints as one line, and a name a shell would read as
  // code is not completed. With both sentences cut to what still reads, the
  // page measured 4860 against 4860, and a page at zero left fails the
  // moment another branch adds one word to it.
  //
  // Raised again on merge with #1113, to 4950: that branch's `defaults` copy
  // and this one's help-and-completion copy are both on the page, which
  // measures 4942 with neither side's prose cut. Neither is trimmable for
  // the other's sake.
  "writing-a-dog": 4950,
  // Raised from 3380 on 2026-10-24 for "Dropping one override": `shep unset`
  // is a new verb that undoes `shep set`, and the page's only other way to
  // put a setting back was a whole-file `--reset`, so the section is new
  // operator-facing surface, not prose to trim back out.
  overrides: 3440,
  // Raised from 3240 on 2026-09-27 for "A sheep's dogs row": the sub-screen
  // and the per-sheep table pane are new operator-facing surface, not
  // prose to trim back out.
  //
  // Raised from 3290 on 2026-10-01 for the dogs tab: a ninth group with its
  // own key is new operator-facing surface, not prose to trim back out.
  //
  // Raised from 3310 on 2026-10-12 for the tree of tables: `←`, `→` and
  // `Enter` open and close a dog's nested tables, which is new
  // operator-facing surface, not prose to trim back out.
  //
  // Raised again for the list sub-screen refusing an element: an item
  // schema's minLength, pattern, enum and integer bounds are checked before
  // the element is filed, and the page has to name those four rather than
  // say the editor refuses whatever the item schema rejects, which is what
  // it said and is not true of maxLength.
  //
  // Raised again for the dog config pane's arrays and maps of tables, which
  // read as a count and open onto entries: a new row shape the page has to
  // name, with how its entries are labelled and that they are read-only.
  //
  // Raised from 3465 for editing a field inside an entry: the page said
  // everything under one was read-only, and now has to say what edits, what
  // stays read-only, and that Esc writes the section once.
  // Raised from 3530 on 2026-10-13 for `d` on a dog pane's entry row: it
  // removes an operator's data from dogs.toml, so the page has to say when
  // it writes, what `u` does and where the cursor goes. The page had 9
  // words of headroom.
  // Raised from 3785 on 2026-10-13 for a dog setting that is a tagged union:
  // it reads as its tag, opens to that tag's fields and is picked with space,
  // which is new operator-facing surface. The page had no headroom.
  //
  // Raised from 3880 for editing a decimal field: the page has to name the
  // kind that now edits, say a whole number typed into one is filed as a
  // float, and say a list of numbers is still read-only, which the old text
  // left implied by silence.
  "lookout-config": 3920,
  // Raised from 2930 on 2026-09-14 for the host line, which `shep flock`
  // began printing on a one-shot listing and not only under `--follow`. It
  // shows above every table on this page, so four transcripts gained a line
  // and the page gained a section saying what the four figures are and why
  // a rate can read `-`.
  // Raised from 3130 on 2026-10-15 for "Following logs" saying which stream
  // colour a plain line gets, that a line carrying its own colour is left
  // alone, and that `--format json` stays raw. Required content for #846.
  // Raised from 3190 on 2026-10-21 for the one sentence saying `--host`
  // follows the host's flock. Required content for #949.
  output: 3200,
  // Raised from 2980 on 2026-10-13 for the `log_match` rule: a new `on`
  // value, its `stream` key, and the loss and secrets caveats are
  // operator-facing surface, not prose to trim back out.
  // Raised from 3100 on 2026-10-13 for the `secrets.json` rotation: a
  // `shep secret set` now reaches a running dog, which the live-updates
  // passage has to list.
  // Raised from 3130 on 2026-10-13 for dog names: a sheep cannot take a
  // dog's name, and one that already holds a built-in's keeps it.
  //
  // Raised from 3200 for `shep delete` on a dog: one verb now carries both
  // removals, and the page has to say what survives it and that a built-in
  // is refused. Required content, not prose to trim back out.
  //
  // Raised from 3254 on 2026-10-14 for what `shep daemon reload` does with a
  // dog: which ones cross, which one the successor registers afresh, and which
  // one it stops. An operator has to know a hand-edited `enabled_dogs` reaches
  // a running dog through the reload and nowhere else. Raised again the same
  // day for the review's correction to that rule: what a successor leaves
  // behind is a parked dog, and a marked one still holding a process crosses.
  //
  // Raised again for the metrics dog's OTLP export: four config keys an
  // operator has to be told the defaults of, the `--features otel` install
  // line, what a build without the feature does with `otlp_endpoint`, and
  // the measured binary size #292 asked for. All of it is operator-facing
  // surface for a shipped feature, not prose to trim back out.
  //
  // no headroom to pay for it.
  //
  // Raised from 3960 on 2026-10-14 for the `shep copy` sentence: a per-sheep
  // dog table is operator-facing config, and #830 is the verb that moves one
  // onto another sheep.
  //
  // Raised from 3990 on 2026-10-24 for the `shep adopt` defaults sentence:
  // the section an adopted dog arrives with is where an operator edits that
  // dog's settings (#356).
  dogs: 4030,
  // Raised from 2770 for the "Over --host" section: a lookout over a host
  // leaves panes blank that an operator would otherwise read as broken, so
  // the table naming them is required content.
  lookout: 2830,
  "first-flockfile": 2360,
  // Raised from 2080 on 2026-10-13 for the rotation paragraph: a provider's
  // push now reaches a running dog, which an operator rotating a secret has
  // to be told. The page had drifted to 2067 against a recorded 1977.
  // Raised again from 2150 on 2026-10-13 for the store poll: a `shep secret
  // set` reaches a running dog, and the page says how soon and which keys.
  secrets: 2200,
  // Raised from 2020 on 2026-10-13 for `disable` and `enable`: two verbs
  // with a refusal an operator has to be told about (an id or `name:slot`
  // exits 2, and `start`, `restart` and `stock` refuse a disabled app) are
  // operator-facing surface, not prose to trim back out.
  //
  // Raised from 2190 on 2026-10-13 for the handover half of the same pair of
  // verbs: the mark now survives `shep daemon reload`, and the recorded `2088`
  // was 95 words stale before that sentence went in.
  //
  // Raised from 2200 for `shep delete` reaching a dog: the verb an operator
  // already reads on this page now has a second path, and the page has to say
  // which names take it. The dog's own detail stays on the dogs page.
  //
  // Raised again for the same verb refusing a name it cannot judge, which is
  // an exit an operator hits. Paid for by tightening the sentence above it.
  //
  // Raised from 2245 for #951: `stock`, `enable` and `disable` take `--host`
  // and read a dog's name as a sheep's there, which the page must say.
  //
  // Raised from 2260 for #952: `start` takes `--host` for a sheep name only,
  // and the page has to say what a path target does instead.
  lifecycle: 2300,
  // Cut from 1900 for the stamps-off tail sentence: the page has to say a
  // sheep with `log_timestamps = false` carries no stamp for `bleats` to
  // strip, and it paid for that line by tightening the sentences around it
  // instead of raising the ceiling. Measured 1914 against 1900 before the cut.
  // Raised from 1895 on 2026-10-15 for the same #846 rule on this page:
  // which lines are coloured, the fallback for a line that already has its
  // own colour, and what a table strips out of a line.
  // Raised from 1995 on 2026-10-21 for the sentence saying a `--host` follow
  // has no history. Required content for #949.
  logs: 2010,
  // Raised from 1690 on 2026-09-28 for "Naming your lambs": `lamb-label`
  // is a new message an app author has to be told about, not prose to trim
  // back out.
  // Raised from 1890 for the "Metrics" section, which says where a `metric`
  // line turns up and what the dog refuses. New content rather than a
  // correction.
  // Raised from 2060 for "Notes for a dog": `note` is a new message an app
  // author has to be told about, and the section is already cut to the
  // topic, the grammar and the drop.
  // Raised from 2165 for "Handlers may run at the same time": an app author
  // who does not echo `id` has to be told the Rust library can answer
  // actions out of the order it was asked. Required content for #1071.
  "shepherd-channel": 2220,
  "from-pm2": 1720,
  "boot-order": 1670,
  "talking-to-a-sheep": 1620,
  // Raised from 1330 on the same day and for the same feature. `host` is a
  // fourth key beside `data`, and its three spellings are three different
  // answers a script has to tell apart: an object, `null`, and no key at
  // all.
  //
  // Raised from 1410 on 2026-10-13 for `disabled`: a key that is absent
  // unless true, beside a `status` that still reads `stopped`, is a shape
  // a script has to be told about.
  //
  // Raised from 1470 for the fleet listing: `origin` on a row and a `hosts`
  // map beside `host` are a shape a script has to be told about.
  "json-output": 1500,
  startup: 1250,
  examples: 1200,
  "not-built": 1080,
  folds: 900,
  whistle: 710,
  containers: 560,
  // Raised from 560 on 2026-10-13 for dog names: an upgrade must say a sheep
  // named after a built-in keeps its name.
  //
  // Raised from 610 on 2026-10-13 for "Going back to an older version": an
  // operator has to be told a downgrade loses the disable marks and that
  // `shep disable` is what to re-run. Nothing on the page was cut for it.
  //
  // Raised again the same day, to 675, when review named the wrong downgrade
  // behaviour in that section. A parked sheep stays down and loses its refusals
  // rather than coming straight back up, and saying that correctly costs 28
  // more words than saying it wrongly.
  // Raised from 675 on 2026-10-14 for the `enabled_dogs` half of the reload
  // the page already describes: a successor stops a dog the new file dropped
  // and registers a parked one at rest. Required content, not prose.
  upgrading: 710,
  kv: 550,
  // Raised from 440 on 2026-09-21 for the prebuilt-binary download list,
  // which is new content rather than a correction. The page was sitting at
  // exactly 440, one word from failing the build, and the `// 413` beside
  // the old number was 27 words stale: the figure in this comment is only
  // as good as the run that wrote it.
  //
  // Raised from 500 on 2026-10-24 for the completions sentence: the script
  // now completes adopted dogs and the subcommands their manifests declare,
  // and generating it before an adopt leaves it stale.
  "getting-started": 520,
  serve: 480,
  hosts: 500,
  // Raised from 380 on 2026-10-21 for the global `--host` flag, which is new
  // content: what a destination is and that only `flock` takes it so far.
  // Raised from 400 on 2026-10-21 for the verbs `--host` now reaches and
  // `describe` showing no secrets over it. Required content for #949.
  // Raised from 420 for the six verbs #951 adds to the list `--host` reaches.
  // Raised from 430 for `lookout` in the list `--host` reaches (#954).
  cli: 435,
  "community-dogs": 300,
  terminology: 210,
  "pm2-verbs": 40,
  // Raised from 330 for the adopt and enable prompt: what the operator is
  // asked, what a no, `--yes` and no terminal do, and the two unknown-
  // permission cases are behaviour a reader cannot guess, not prose to trim.
  "dog-grants": 430,
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

test("every page that exists has a budget", async () => {
  // The other direction, and the one that was missing. `lookout-config` and
  // `pm2-verbs` shipped after the phase that seeded BUDGETS, so they had no
  // ceiling at all: the report below printed them without one and nothing
  // failed. A page with no ceiling is a page that can grow back.
  const files = await readdir(pagesDir);
  const missing = files
    .filter((f) => f.endsWith(".astro"))
    .map((f) => f.replace(/\.astro$/, ""))
    // `index` is the redirect to chapter 1, not a chapter. Same exclusion
    // verify-docs-nav.ts makes, for the same reason.
    .filter((slug) => slug !== "index" && !(slug in BUDGETS));
  assert.deepEqual(
    missing,
    [],
    `no prose budget for ${missing.join(", ")}; add an entry at the page's current count plus a little headroom`,
  );
});

/** Column widths of the report, so its header and its rows line up. */
const COUNT_WIDTH = 5;
const CEILING_WIDTH = 6;
const LEFT_WIDTH = 5;

/** Words left under a ceiling below which the report names the page itself. */
const THIN_HEADROOM = 10;

/** The report's first line, above the rows `reportRow` writes. */
export const REPORT_HEADER = [
  "count".padStart(COUNT_WIDTH),
  "budget".padStart(CEILING_WIDTH),
  "left".padStart(LEFT_WIDTH),
  "page",
].join("  ");

/**
 * One report row: words measured, ceiling, words left under it, slug.
 *
 * The headroom is the number a page edit needs, and it is always the one this
 * run measured. A copy of it written beside a ceiling in `BUDGETS` is the stale
 * comment #596 is about.
 */
export function reportRow(slug: string, count: number, budget: number | undefined): string {
  return [
    String(count).padStart(COUNT_WIDTH),
    (budget === undefined ? "-" : String(budget)).padStart(CEILING_WIDTH),
    (budget === undefined ? "-" : String(budget - count)).padStart(LEFT_WIDTH),
    slug,
  ].join("  ");
}

/** One page in the report. `budget` is undefined for a page with no ceiling. */
type PageRow = { slug: string; count: number; budget: number | undefined };

/** Slugs with under `under` words left, thinnest first, each as `slug left`. */
export function thinHeadroom(
  rows: { slug: string; count: number; budget: number }[],
  under: number,
): string[] {
  return rows
    .filter((row) => row.budget - row.count < under)
    .sort((a, b) => a.budget - a.count - (b.budget - b.count))
    .map((row) => `${row.slug} ${row.budget - row.count}`);
}

test("reportRow prints the count, the ceiling and the words left under it", () => {
  assert.equal(reportRow("from-pm2", 1717, 1720), " 1717    1720      3  from-pm2");
});

test("reportRow says so when a page has no ceiling", () => {
  assert.equal(reportRow("a-new-page", 120, undefined), "  120       -      -  a-new-page");
});

test("thinHeadroom names the pages with the least room, thinnest first", () => {
  const rows = [
    { slug: "writing-a-dog", count: 4489, budget: 4490 },
    { slug: "from-pm2", count: 1720, budget: 1720 },
    { slug: "cli", count: 356, budget: 380 },
  ];
  assert.deepEqual(thinHeadroom(rows, 10), ["from-pm2 0", "writing-a-dog 1"]);
});

test("thinHeadroom says nothing when every page has room", () => {
  assert.deepEqual(thinHeadroom([{ slug: "cli", count: 356, budget: 380 }], 10), []);
});

test("no BUDGETS entry carries a hand-written measured count", async () => {
  // A `// 1659` beside a ceiling used to be the only number anyone saw without
  // running this script. 16 of 29 were stale on 2026-10-19 and every one read
  // lower than the page measured: `from-pm2` showed 61 words of room and had 3.
  // The comment was believed twice and the script was not run, which is #596.
  // The count lives in the report this script prints, where it is always the
  // one just measured.
  const self = fileURLToPath(import.meta.url);
  const source = await readFile(self, "utf8");
  const start = source.indexOf("export const BUDGETS");
  assert.notEqual(start, -1, "this guard reads the BUDGETS block by name, and the name is gone");
  const block = source.slice(start, source.indexOf("};", start));
  const lines = block.split("\n");
  const entries = lines.filter((line) => /^\s*"?[a-z][a-z0-9-]*"?:\s*\d+,?\s*$/.test(line));
  assert.equal(
    entries.length,
    Object.keys(BUDGETS).length,
    "this guard only saw part of the block, so it checked nothing",
  );
  const written = lines.filter((line) => /^\s*"?[a-z][a-z0-9-]*"?:\s*\d+,?\s*\/\/\s*\d+\s*$/.test(line));
  assert.deepEqual(
    written,
    [],
    "a measured count written beside a ceiling goes stale; run this script and read the headroom column it prints",
  );
});

test("report every page's prose count, so the unbudgeted ones stay visible", async () => {
  const files = (await readdir(pagesDir)).filter((f) => f.endsWith(".astro") && f !== "index.astro");
  const rows: PageRow[] = [];
  for (const file of files) {
    const slug = file.replace(/\.astro$/, "");
    rows.push({
      slug,
      count: proseWords(await readFile(`${pagesDir}/${file}`, "utf8")),
      budget: BUDGETS[slug],
    });
  }
  rows.sort((a, b) => b.count - a.count);
  const total = rows.reduce((sum, row) => sum + row.count, 0);
  console.log(REPORT_HEADER);
  for (const row of rows) {
    console.log(reportRow(row.slug, row.count, row.budget));
  }
  console.log(`  ${String(total).padStart(COUNT_WIDTH)}  TOTAL across ${rows.length} pages`);
  const budgeted = rows.filter((row): row is PageRow & { budget: number } => row.budget !== undefined);
  const thin = thinHeadroom(budgeted, THIN_HEADROOM);
  if (thin.length > 0) {
    console.log(`  thin headroom, under ${THIN_HEADROOM} words left: ${thin.join(", ")}`);
  }
  assert.ok(rows.length > 0, "found no docs pages to count");
});
