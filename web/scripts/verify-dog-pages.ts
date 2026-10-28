/*
 * Regression guard for the pages this site imports: their heading anchors, and
 * the listing that links them.
 *
 * verify-heading-anchors.ts holds the same rule and has held it for every
 * phase of the docs rewrite. It cannot reach an imported page twice over: its
 * `ENFORCED` list names slugs a human added, and it reads
 * `src/pages/docs/<slug>.astro`, where a dog's page is not — the route is
 * `src/pages/docs/dogs/[dog]/[...page].astro` and the text comes from another
 * repository at build time.
 *
 * So this reads the built HTML instead, which is the only place an imported
 * page's ids actually exist. The renderer gives them and nothing here rewrites
 * them: `[...page].astro`'s own comment says the ids the renderer gave are the
 * ids the page ships, and DocsLayout only decorates an id that is already
 * there. A README that writes its own `<h2>` as raw HTML therefore ships an
 * unanchored heading, and a README with the same heading text twice ships one
 * id for two sections. Both break a link someone can paste into a message,
 * which is the whole reason the ids exist.
 *
 * Zero imported pages is a pass, not a failure. `SHEP_DOGS_STRICT` is unset on
 * a pull request, so a build that cannot reach GitHub builds the site with no
 * dog pages at all, and a check that demanded one would turn a fetch failure
 * into a docs failure. The four built-page checks are each proven by a fixture
 * test instead of by a page that happens to be there.
 *
 * `dogs/index.html` is left out: `/docs/dogs` is a page this site writes, and
 * verify-heading-anchors.ts already reads its source.
 *
 * The last two checks are the other end of the same fact. An imported page is
 * worth writing only if a reader can reach it, and `/docs/community-dogs` is
 * where they look for one. That page is this site's own, so this reads its
 * built entries against the built routes rather than against any rule the
 * listing could answer on its own: a build that wrote no dog page must draw no
 * Docs link, and a build that wrote one must draw the link that reaches it.
 * The check before them reads every built page for the same thing, because a
 * hand-written page can hard-code a dog route the build never fetched.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { builtDocsPages, type BuiltPage } from "./built-pages.ts";
import { anchorOf, dogs } from "../src/data/dogs.ts";

/** A heading on a built page: its tag, its text, and the id it carries. */
export interface BuiltHeading {
  tag: string;
  /** Tags stripped and whitespace collapsed, for naming the heading in a message. */
  text: string;
  /** The literal `id="..."`, or null when the heading has none. */
  id: string | null;
}

/**
 * The imported dog pages in a page list.
 *
 * A route under `dogs/` is a dog's page: `dogs/deploy` is a README and
 * `dogs/deploy/docs/setup` is a page that README links to. `dogs` on its own is
 * the index this site writes.
 */
export function dogPages(pages: BuiltPage[]): BuiltPage[] {
  return pages.filter(([route]) => route.startsWith("dogs/"));
}

/**
 * Every `<h2>` and `<h3>` inside the article wrapper the dog route writes.
 *
 * Scoped to the wrapper rather than the whole page because the rule is about
 * the imported text: a heading the site's own chrome draws is this repository's
 * business, and verify-heading-anchors.ts reads that chrome from source.
 */
export function builtHeadings(html: string): BuiltHeading[] {
  const article = /<article class="dog-readme"[\s\S]*?<\/article>/.exec(html);
  if (article === null) return [];
  const found: BuiltHeading[] = [];
  const heading = /<(h[23])(\s[^>]*)?>([\s\S]*?)<\/\1>/g;
  for (const m of article[0].matchAll(heading)) {
    const literal = /\sid="([^"]+)"/.exec(m[2] ?? "");
    found.push({
      tag: m[1],
      text: m[3].replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim(),
      id: literal === null ? null : literal[1],
    });
  }
  return found;
}

/** Headings with no id, as `h2: text`. */
export function unanchored(html: string): string[] {
  return builtHeadings(html)
    .filter((h) => h.id === null)
    .map((h) => `${h.tag}: ${h.text.slice(0, 40)}`);
}

/** Ids two headings on the page share, as `id: used by "text"`. */
export function repeatedIds(html: string): string[] {
  const headings = builtHeadings(html).filter((h) => h.id !== null);
  const seen = new Set<string>();
  const repeated: string[] = [];
  for (const h of headings) {
    if (!seen.has(h.id!)) {
      seen.add(h.id!);
      continue;
    }
    repeated.push(`${h.id}: used again by "${h.text.slice(0, 40)}"`);
  }
  return repeated;
}

/** A dog README whose renderer drew two sections and one heading by hand. */
const BROKEN_README = `<article class="dog-readme"><div class="dog-readme-body"><h2 id="install">Install</h2><h2>Setup</h2><h3 id="install">Install from a tag</h3></div></article>`;

/** The same README with every heading drawn by the renderer and each its own. */
const CLEAN_README = `<article class="dog-readme"><div class="dog-readme-body"><h2 id="install">Install</h2><h2 id="setup">Setup</h2><h3 id="install-from-a-tag">Install from a tag</h3></div></article>`;

test("unanchored() catches an imported heading the README wrote as raw HTML", () => {
  assert.deepEqual(unanchored(BROKEN_README), ["h2: Setup"]);
});

test("unanchored() passes an imported page whose headings all carry an id", () => {
  assert.deepEqual(unanchored(CLEAN_README), []);
});

test("repeatedIds() catches an imported page that uses one id twice", () => {
  assert.deepEqual(repeatedIds(BROKEN_README), [
    `install: used again by "Install from a tag"`,
  ]);
});

test("repeatedIds() passes an imported page whose ids are each used once", () => {
  assert.deepEqual(repeatedIds(CLEAN_README), []);
});

test("dogPages() takes a dog's pages and not the index this site writes", () => {
  const routes = ["", "dogs", "dogs/deploy", "dogs/deploy/docs/setup", "terminology"];
  assert.deepEqual(
    dogPages(routes.map((route) => [route, ""] as BuiltPage)).map(([route]) => route),
    ["dogs/deploy", "dogs/deploy/docs/setup"],
  );
});

test("dogPages() reads a build with no imported page as nothing to check", () => {
  assert.deepEqual(dogPages([["terminology", "<h2>Only a written page</h2>"]]), []);
});

test("every imported dog page is drawn inside the wrapper this route writes", async () => {
  for (const [route, html] of dogPages(await builtDocsPages())) {
    assert.ok(
      html.includes(`class="dog-readme"`),
      `/docs/${route} has no <article class="dog-readme">, so builtHeadings() read nothing on it and the two checks below pass it without looking. [...page].astro writes that wrapper.`,
    );
  }
});

test("every imported dog page anchors every heading and repeats none", async () => {
  for (const [route, html] of dogPages(await builtDocsPages())) {
    const missing = unanchored(html);
    assert.deepEqual(
      missing,
      [],
      `/docs/${route} has ${missing.length} heading(s) with no id: ${missing.join(" | ")}. A reader cannot link to that section, which is what a heading's id is for. The renderer anchors every heading it draws from markdown, so an unanchored one is a heading the README wrote as raw HTML: write it as \`## Setup\` in the README instead.`,
    );
    const repeated = repeatedIds(html);
    assert.deepEqual(
      repeated,
      [],
      `/docs/${route} repeats heading id(s) ${repeated.join(" | ")}. Two sections under one id means a link to it lands on whichever the browser finds first. Reword one of the headings in the README so the renderer gives each its own id.`,
    );
  }
});

/**
 * Each `<article class="dog-entry">` the built listing drew, as its own block.
 *
 * Per entry rather than over the page: the page's own chrome links `/docs/dogs`
 * from the sidebar, so a walk over the whole document would read that as an
 * entry carrying a Docs link it does not have.
 */
export function dogEntryBlocks(html: string): string[] {
  return [...html.matchAll(/<article class="dog-entry"[\s\S]*?<\/article>/g)].map(
    (match) => match[0],
  );
}

/**
 * The dog routes a built page links to, as routes: `dogs/deploy`,
 * `dogs/deploy/docs/setup`, with any trailing slash and `#fragment` off.
 *
 * `/docs/dogs` on its own is not one of them: that is the index this site
 * writes, and the sidebar carries it on every docs page.
 */
export function dogHrefs(html: string): string[] {
  return [...html.matchAll(/href="\/docs\/(dogs\/[^"]*)"/g)]
    .map((match) => match[1].split("#")[0].split("?")[0].replace(/\/$/, ""))
    .filter((route) => route !== "");
}

/**
 * The href a listing would use for each dog page a page list wrote.
 *
 * A dog's own page is `dogs/<adopt_as>`; anything deeper is a page some
 * README links to, which a row has no business linking.
 */
export function dogReadmeHrefs(pages: BuiltPage[]): string[] {
  return dogPages(pages)
    .filter(([route]) => !route.slice("dogs/".length).includes("/"))
    .map(([route]) => `/docs/${route}`);
}

/** A listing with one entry that links a docs page and one that does not. */
const LISTING =
  `<article class="dog-entry"><h3 id="ours"><a href="#ours">shep-ours (Ours)</a></h3>` +
  `<p class="meta">MIT · <a href="/docs/dogs/ours">Docs</a> · ` +
  `<a href="https://github.com/shep-pm/shep-ours">Repository</a></p></article>` +
  `<p>the page's own prose</p>` +
  `<article class="dog-entry"><h3 id="theirs"><a href="#theirs">not-ours (Theirs)</a></h3>` +
  `<p class="meta">MIT · <a href="https://github.com/someone/not-ours">Repository</a></p></article>`;

test("dogEntryBlocks() reads each entry as its own block", () => {
  const blocks = dogEntryBlocks(LISTING);
  assert.equal(blocks.length, 2, LISTING);
  assert.ok(blocks[0].includes(`href="/docs/dogs/ours"`), blocks[0]);
  assert.ok(!blocks[1].includes("/docs/dogs/"), blocks[1]);
});

test("dogEntryBlocks() ignores a docs link the page's own chrome carries", () => {
  const html = `<nav><a href="/docs/dogs">Dogs</a></nav>${LISTING}`;
  assert.equal(dogEntryBlocks(html).length, 2);
});

test("dogHrefs() reads a dog's pages and not the index this site writes", () => {
  const html =
    `<a href="/docs/dogs">Dogs</a> <a href="/docs/dogs/deploy">Docs</a> ` +
    `<a href="/docs/dogs/deploy/docs/setup#install">Setup</a> ` +
    `<a href="https://github.com/shep-pm/shep-deploy">Repository</a>`;
  assert.deepEqual(dogHrefs(html), ["dogs/deploy", "dogs/deploy/docs/setup"]);
});

test("dogHrefs() reads a link to a page no build wrote as a link", () => {
  // The point of the check below: a route is a route whether or not anything
  // sits at it, and only the build output says which is which.
  assert.deepEqual(dogHrefs(`<a href="/docs/dogs/gone">Docs</a>`), ["dogs/gone"]);
});

test("dogReadmeHrefs() names each dog's own page and not the pages below it", () => {
  const pages: BuiltPage[] = [
    ["", ""],
    ["dogs", ""],
    ["dogs/deploy", ""],
    ["dogs/deploy/docs/setup", ""],
    ["community-dogs", ""],
  ];
  assert.deepEqual(dogReadmeHrefs(pages), ["/docs/dogs/deploy"]);
});

test("no built page links a dog route this build never wrote", async () => {
  const pages = await builtDocsPages();
  const built = new Set(pages.map(([route]) => `/docs/${route}`));
  for (const [route, html] of pages) {
    for (const href of dogHrefs(html)) {
      assert.ok(
        built.has(`/docs/${href}`),
        `/docs/${route} links /docs/${href}, and this build wrote no page there. A dog's page is text the build fetched from another repository at its latest release tag, so a build that could not fetch it writes nothing at that route and every link into it is a 404. Link the route only where the build has it, as \`docsPageOf\` does, or write it as text rather than as a link.`,
      );
    }
  }
});

test("the built listing links a Docs page only where this build wrote one", async () => {
  const pages = await builtDocsPages();
  const listing = pages.find(([route]) => route === "community-dogs");
  assert.ok(listing, "no built /docs/community-dogs page to read");

  const blocks = dogEntryBlocks(listing[1]);
  assert.equal(
    blocks.length,
    dogs.length,
    `the built listing draws ${blocks.length} entries for an index of ${dogs.length} rows`,
  );

  const built = new Set(pages.map(([route]) => `/docs/${route}`));
  const written = new Set(dogReadmeHrefs(pages));
  const linked: string[] = [];

  for (const dog of dogs) {
    const block = blocks.find((entry) => entry.includes(`id="${anchorOf(dog)}"`));
    assert.ok(block, `the built listing has no entry for "${dog.package}"`);
    const hrefs = dogHrefs(block);
    assert.ok(
      hrefs.length <= 1,
      `${dog.package}'s entry carries ${hrefs.length} Docs links: ${hrefs.join(", ")}. A row has one page.`,
    );
    for (const href of hrefs) {
      assert.ok(
        built.has(`/docs/${href}`),
        `${dog.package}'s entry links /docs/${href}, which this build wrote no page at.`,
      );
      linked.push(`/docs/${href}`);
    }
  }

  assert.deepEqual(
    linked.sort(),
    [...written].sort(),
    `the listing links ${linked.length} dog page(s) and this build wrote ${written.size}. A page a build wrote that no row links is a page no reader can reach, and a link to a page no build wrote is a 404.`,
  );
});
