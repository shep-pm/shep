/*
 * Every page the build wrote under `dist/docs`, at whatever depth.
 *
 * The four built-page checks — verify-fragment-targets.ts,
 * verify-stacked-table-labels.ts, verify-callout-children.ts and
 * verify-dog-pages.ts — each used to do this walk themselves as a `readdir`
 * that kept the directories and read `<name>/index.html` one level down. That
 * is the shape a page this site writes builds: `src/pages/docs/terminology.astro`
 * becomes `dist/docs/terminology/index.html`. It is not the shape a page this
 * site imports builds. A first-party dog's README lands at
 * `dist/docs/dogs/deploy/index.html`, and a page that README links to lands
 * deeper still — shep-deploy's own design plan came out at
 * `dist/docs/dogs/deploy/docs/brainstorming/specs/2026-08-26-deploy-dog-design/`.
 * A check one level deep never read any of those, so it reported green over
 * half the site.
 *
 * The route is the path from the walked directory to the page with its
 * `index.html` dropped, which is the site's own URL with `/docs` and the
 * leading slash taken off: `terminology`, `dogs/deploy`,
 * `dogs/deploy/docs/setup`. A failure message prints `/docs/${route}` so it
 * names the page a reader would open rather than a directory someone has to
 * translate.
 *
 * `dist/docs/index.html`, the redirect at `/docs/`, comes back with an empty
 * route. It is a built docs page like any other and the checks read it.
 *
 * The walk throws when it finds no page at all. Every caller reads the build
 * output, and a run against a missing or empty `dist/` is a run that checked
 * nothing while saying it checked everything.
 *
 * The tests below run inside every script that imports this file, which is how
 * `node --test` treats a test registered by an import. That is why `build` in
 * package.json names the four checks and not this module: naming it too would
 * run these tests a fifth time for no added coverage.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

/** The build output. A check that reads a built stylesheet needs this one. */
export const distDir = fileURLToPath(new URL("../dist", import.meta.url));

/** The docs part of it, which is where every route under `/docs` is written. */
export const distDocsDir = fileURLToPath(new URL("../dist/docs", import.meta.url));

/** A built page: its route, and the HTML the build wrote for it. */
export type BuiltPage = [route: string, html: string];

/**
 * Every `index.html` under `dir`, each with the route it sits at.
 *
 * Routes come back in path order so a failure names the same page on every
 * run. `readdir` returns entries in whatever order the directory happens to
 * hold them, and a check that stops at the first bad page would then report
 * whichever one the filesystem felt like returning.
 *
 * @param dir The directory to walk. Defaults to `dist/docs`; a test passes a
 *   temp tree instead.
 * @throws {AssertionError} when the walk finds no `index.html` at any depth,
 *   which means the build has not run.
 */
export async function builtDocsPages(dir: string = distDocsDir): Promise<BuiltPage[]> {
  const pages: BuiltPage[] = [];

  async function walk(at: string, route: string): Promise<void> {
    for (const entry of await readdir(at, { withFileTypes: true })) {
      const path = `${at}/${entry.name}`;
      if (entry.isDirectory()) {
        await walk(path, route === "" ? entry.name : `${route}/${entry.name}`);
      } else if (entry.name === "index.html") {
        pages.push([route, await readFile(path, "utf8")]);
      }
    }
  }

  await walk(dir, "");
  assert.ok(
    pages.length > 0,
    `no built page under ${dir}; run astro build before the built-page checks`,
  );
  return pages.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
}

/**
 * A temp tree shaped like a build output, handed to `run` and then removed.
 *
 * @param files Each key is a route; its value is that route's `index.html`, or
 *   null for a directory the build wrote no page into. The empty key writes
 *   `index.html` in the walked directory itself.
 */
async function withTree<T>(
  files: Record<string, string | null>,
  run: (dir: string) => Promise<T>,
): Promise<T> {
  const dir = await mkdtemp(`${tmpdir()}/built-pages-`);
  try {
    for (const [route, html] of Object.entries(files)) {
      const at = route === "" ? dir : `${dir}/${route}`;
      await mkdir(at, { recursive: true });
      if (html !== null) await writeFile(`${at}/index.html`, html, "utf8");
    }
    return await run(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test("builtDocsPages() returns a page nested below the first level", async () => {
  await withTree(
    {
      "": "<p>docs root</p>",
      "terminology": "<h2 id=\"x\">T</h2>",
      "dogs/x": "<h2 id=\"a\">A</h2>",
      "dogs/x/y": "<h2 id=\"b\">B</h2>",
    },
    async (dir) => {
      const pages = await builtDocsPages(dir);
      assert.deepEqual(
        pages.map(([route]) => route),
        ["", "dogs/x", "dogs/x/y", "terminology"],
      );
      const deep = pages.find(([route]) => route === "dogs/x/y");
      assert.ok(deep, "the nested page is missing from the walk");
      assert.equal(deep[1], `<h2 id="b">B</h2>`);
    },
  );
});

test("builtDocsPages() throws when the tree holds no page", async () => {
  await withTree({ "terminology/sub": null }, async (dir) => {
    await assert.rejects(
      () => builtDocsPages(dir),
      /no built page/,
      "an empty build output has to stop the checks that read it",
    );
  });
});
