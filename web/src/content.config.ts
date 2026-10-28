/*
 * The docs pages this site imports rather than writes.
 *
 * Every dog in `public/dogs.json` whose `repo` is under `github.com/shep-pm/`
 * gets a page at `/docs/dogs/<adopt_as>`, built from that repository's README at
 * the tag of its latest GitHub release, plus a page under it for each relative
 * `.md` link the README carries. `docs/decisions.md` records why under "A
 * first-party dog's docs page": never vendored, never pinned, and the tag the
 * page names is the tag the bytes came from.
 *
 * The fetching and the link rewriting live in `src/data/dog-readme-fetch.ts` and
 * `src/data/dog-readme-links.ts`, which import nothing from `astro:` so
 * `scripts/verify-dog-readme.ts` can run them against a fake `fetchFn`. This
 * file is the half that can only run inside an Astro build: it hands the loader
 * context's own `renderMarkdown` to the fetcher, so a dog's README is drawn by
 * the same renderer the rest of the site uses, with its own heading ids, its own
 * syntax highlighting, and its own table markup.
 *
 * `SHEP_DOGS_STRICT` is the one dial. Set to `1` — which only `pages.yml` does —
 * a dog whose README cannot be read stops the build, and GitHub Pages keeps the
 * last good site up. Unset, which is how `web.yml` runs a pull request, the same
 * failure is one warning on the build log and the dog simply gets no page, so a
 * dog repository's bad release never lands in shep's own pull request queue.
 * Unset is also what an offline build gets, and the loader clears the collection
 * before it fills it, so that is zero dog pages rather than the last build's,
 * which is a page naming a tag its bytes no longer came from.
 */
import { defineCollection } from "astro:content";
import { z } from "astro/zod";
import { dogs } from "./data/dogs.ts";
import { README_PATH, fetchDogPages, firstPartyDogs, githubRepoOf } from "./data/dog-readme-fetch.ts";
import { rewriteLinks } from "./data/dog-readme-links.ts";
import { labelTableCells } from "./data/dog-readme-tables.ts";
import type { Dog } from "./data/dogs.ts";

/**
 * Whether a failed fetch stops the build.
 *
 * The name is this slice's, and `pages.yml` is what sets it. Anything other than
 * exactly `1` is not strict, so a stray `SHEP_DOGS_STRICT=true` or `yes` warns
 * rather than stops, which is the safe direction to fail in.
 */
const strict = process.env.SHEP_DOGS_STRICT === "1";

/**
 * The part of `/docs/dogs/<adopt_as>` a repository path becomes.
 *
 * `README.md` is the dog's page itself and so adds nothing to the route; a
 * sub-page keeps its directories and drops the extension, which is the route
 * `dogDocRoute()` writes into the README's own links.
 */
function routeSuffix(path: string): string {
  if (path === README_PATH) return "";
  return path.replace(/\.md$/i, "");
}

/**
 * The HTML a dog page ships: the renderer's output with its relative links
 * pointed at this site, and its table cells labelled for a phone.
 *
 * @throws {Error} when a relative link climbs above the repository root, which
 * no file in the repository can be.
 */
function pageHtml(dog: Dog, tag: string, path: string, html: string): string {
  const { owner, repo } = githubRepoOf(dog);
  return labelTableCells(
    rewriteLinks(html, { owner, repo, tag, adoptAs: dog.adopt_as, fromPath: path }),
  );
}

export const collections = {
  "dog-readme": defineCollection({
    loader: {
      name: "dog-readme",
      async load({ parseData, renderMarkdown, store }) {
        // The data store outlives this build: Astro keeps it between runs, and a
        // loader that only adds entries would serve the last build's pages when
        // this one cannot reach GitHub, or when a new release drops a linked
        // sub-page. This loader re-reads every dog from the network every run,
        // so it owns the collection and starts it empty.
        store.clear();

        const render = async (markdown: string): Promise<string> =>
          (await renderMarkdown(markdown)).html;

        for (const dog of firstPartyDogs(dogs)) {
          const fetched = await fetchDogPages(dog, render, globalThis.fetch, strict);
          // `fetchDogPages` already warned and returned nothing. Strict rethrows
          // instead, so this is only reachable when not strict.
          if (fetched === undefined) continue;

          try {
            const pages: { id: string; data: Record<string, unknown> }[] = [];
            for (const [path, page] of fetched.pages) {
              const id = `${dog.adopt_as}/${path}`;
              pages.push({
                id,
                data: await parseData({
                  id,
                  data: {
                    adoptAs: dog.adopt_as,
                    package: dog.package,
                    name: dog.name,
                    repo: dog.repo,
                    tag: fetched.tag,
                    path,
                    route: routeSuffix(path),
                    html: pageHtml(dog, fetched.tag, path, page.html),
                  },
                }),
              });
            }
            // Nothing is stored until every page of the dog is built, because
            // the README links its sub-pages: storing the README on its own
            // would ship a link to a route that was never written.
            for (const page of pages) store.set(page);
          } catch (error) {
            // The fetch succeeded and the post-processing did not: same policy,
            // same one line, so a dog that half-works never half-ships.
            if (strict) throw error;
            const reason = error instanceof Error ? error.message : String(error);
            console.warn(`dog "${dog.package}" was skipped: ${reason}`);
          }
        }
      },
    },
    schema: z.object({
      /** The `adopt_as` the dog is filed under, which is the slug its page sits at. */
      adoptAs: z.string(),
      /** The crate or package name, used for the page title's fallback. */
      package: z.string(),
      /** The dog's display name from the index, e.g. "Moon". */
      name: z.string(),
      /** The repository, which the page links at the tag below. */
      repo: z.string(),
      /** The release tag every page of this dog was read at, e.g. "v0.6.2". */
      tag: z.string(),
      /** This page's own path in the repository, e.g. `README.md` or `docs/setup.md`. */
      path: z.string(),
      /** The part of the URL after `/docs/dogs/<adoptAs>`: empty for the README. */
      route: z.string(),
      /** The page body, already link-rewritten and table-labelled. */
      html: z.string(),
    }),
  }),
};
