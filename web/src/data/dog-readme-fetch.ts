/*
 * Fetching a first-party dog's README, and the pages it links to, from the
 * tag of its latest GitHub release.
 *
 * This is the data half of the decision in `docs/decisions.md` under "A
 * first-party dog's docs page": a row in `web/public/dogs.json` whose `repo`
 * is under `github.com/shep-pm/` gets a page at `/docs/dogs/<adopt_as>`,
 * built from that repo's README at its latest release tag, never vendored and
 * never pinned. Drawing those pages is the next slice's job; this file only
 * gets the text and the tag it came from, before any page can be drawn.
 *
 * Plain TypeScript with no `astro:` imports and no `?raw` import, the way
 * `dogs.ts` is, so `node --test scripts/verify-dog-readme.ts` can load it
 * directly and every case in that file runs against a fake `fetchFn`. Nothing
 * here touches the network on its own: the caller passes the fetch function,
 * which is also what lets a build inject a token and a cache.
 *
 * Three shapes are worth naming up front.
 *
 * "Latest release" means GitHub's `releases/latest`, which skips drafts and
 * pre-releases. An unreleased README therefore never reaches the site, which
 * is what that decision bought.
 *
 * Every file is read at the tag, not at the default branch: the tag the
 * release page names and the bytes the page shows are then the same bytes,
 * and a dog that pushes a half-finished README to `main` cannot change what
 * the site says about a release it has already shipped.
 *
 * A failure is a failure. A 404, a refused request, a release with no
 * `tag_name` and a README that links a `docs/setup.md` that does not exist all
 * throw one `Error` naming the dog, the URL and the status. `strict` then
 * decides who hears about it: strict rethrows, which is what stops a deploy;
 * not strict warns once, naming the dog, and returns `undefined` so a PR CI
 * run still builds the rest of the site.
 */

import type { Dog } from "./dogs.ts";
import { findRelativeMdLinks, rawFileUrl } from "./dog-readme-links.ts";

/**
 * The org prefix that makes a dog first-party. Matched as a literal prefix of
 * the `repo` URL, which is what `dogs.ts`'s own `requireRepoUrl` guarantees a
 * row has: `https://github.com/shep-pm-fake/x` is someone else's repo and
 * reads as one.
 */
export const FIRST_PARTY_REPO_PREFIX = "https://github.com/shep-pm/";

/** The page every dog's docs start at, in its own repository. */
export const README_PATH = "README.md";

/**
 * The one environment variable this module reads. A GitHub token is not needed
 * to read a public repo, only to stop being rate-limited: an unauthenticated
 * build gets 60 requests an hour from a shared CI IP, and one deploy fetching
 * three READMEs and their sub-pages can spend a third of that.
 */
const TOKEN_ENV_VAR = "GITHUB_TOKEN";

/** A response, narrowed to the three things this module asks a response for. */
export interface FetchResponse {
  ok: boolean;
  status: number;
  text(): Promise<string>;
}

export interface FetchInit {
  headers?: Record<string, string>;
}

/**
 * The fetch this module calls. Global `fetch` satisfies it, and so does a
 * test's fixture, which is the point of taking it as an argument: every case
 * in `scripts/verify-dog-readme.ts` runs with no network at all.
 */
export type FetchFn = (url: string, init?: FetchInit) => Promise<FetchResponse>;

/**
 * Markdown to HTML. Injected because which Markdown renderer draws a dog's
 * page is the page slice's choice, not this one's; the only thing this file
 * needs is that relative `.md` links come back as `href` attributes.
 */
export type RenderMarkdown = (markdown: string) => string | Promise<string>;

/** One fetched page: the Markdown as the dog wrote it, and the HTML it renders to. */
export interface DogPage {
  markdown: string;
  html: string;
}

/** Everything one dog contributed to the site, and the tag it was read at. */
export interface DogPages {
  tag: string;
  /** Keyed by repository-relative path: `README.md`, `docs/setup.md`. */
  pages: Map<string, DogPage>;
}

function reason(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The rows that get a docs page: every dog this org owns. Order is the index's
 * own, so a page list follows the order a contributor filed the row in.
 */
export function firstPartyDogs(dogs: readonly Dog[]): Dog[] {
  return dogs.filter((dog) => dog.repo.startsWith(FIRST_PARTY_REPO_PREFIX));
}

/**
 * The `owner` and `repo` a `repo` URL names.
 *
 * @throws {Error} when the URL is not a GitHub repository URL. `dogs.ts`'s
 * `validate()` already refuses a row that is not a repository, so this is the
 * guard against a row that is a repository on somewhere else.
 */
export function githubRepoOf(dog: Dog): { owner: string; repo: string } {
  let url: URL;
  try {
    url = new URL(dog.repo);
  } catch {
    throw new Error(`dog "${dog.package}": "repo" is not a URL: "${dog.repo}"`);
  }
  const segments = url.pathname.split("/").filter((segment) => segment !== "");
  if (url.hostname !== "github.com" || segments.length < 2) {
    throw new Error(`dog "${dog.package}": "repo" is not a GitHub repository: "${dog.repo}"`);
  }
  return { owner: segments[0], repo: segments[1].replace(/\.git$/, "") };
}

/**
 * Fetch, and hand back the body of, one URL that has to answer 2xx.
 *
 * @throws {Error} naming `label` and `url` for a request that never got a
 * response, and naming the status for one that answered 404, 403, 502 or
 * anything else outside 2xx.
 */
async function fetchBody(
  url: string,
  fetchFn: FetchFn,
  label: string,
  init?: FetchInit,
): Promise<string> {
  let response: FetchResponse;
  try {
    response = await fetchFn(url, init);
  } catch (cause) {
    throw new Error(`${label}: the request for ${url} failed: ${reason(cause)}`, { cause });
  }
  if (!response.ok) {
    throw new Error(`${label}: ${url} returned HTTP ${response.status}`);
  }
  try {
    return await response.text();
  } catch (cause) {
    throw new Error(
      `${label}: ${url} returned HTTP ${response.status}, but its body could not be read: ${reason(cause)}`,
      { cause },
    );
  }
}

/**
 * The tag of a repository's latest release: `releases/latest`, which skips
 * drafts and pre-releases.
 *
 * Sends `Authorization: Bearer` only when `GITHUB_TOKEN` is set, and reads no
 * other environment variable.
 *
 * @param label names this repository in an error. Defaults to `owner/repo`,
 * which is enough to identify it; `fetchDogPages` passes the dog's package so
 * an error names the dog an operator picked out of the index.
 * @throws {Error} for a 404, which is what GitHub answers when the repository
 * has no release at all, and for a body that is not a release with a
 * `tag_name`.
 */
export async function latestTag(
  owner: string,
  repo: string,
  fetchFn: FetchFn,
  label = `${owner}/${repo}`,
): Promise<string> {
  const url = `https://api.github.com/repos/${owner}/${repo}/releases/latest`;
  const headers: Record<string, string> = { Accept: "application/vnd.github+json" };
  const token = process.env[TOKEN_ENV_VAR];
  if (token !== undefined && token.trim() !== "") {
    headers.Authorization = `Bearer ${token.trim()}`;
  }

  const body = await fetchBody(url, fetchFn, label, { headers });
  let release: unknown;
  try {
    release = JSON.parse(body);
  } catch {
    throw new Error(`${label}: ${url} returned a body that is not JSON.`);
  }
  const tag = isRecord(release) ? release["tag_name"] : undefined;
  if (typeof tag !== "string" || tag.trim() === "") {
    throw new Error(`${label}: ${url} returned a release with no "tag_name".`);
  }
  return tag.trim();
}

/**
 * One file from a repository, read at a tag.
 *
 * @param path repository-relative, e.g. `README.md` or `docs/setup.md`. The
 * name says README because that is what it was written for; it reads any file
 * in the repo, which is what following a dog's `.md` links needs.
 * @throws {Error} naming the URL and the status for a file that is not there
 * at that tag.
 */
export async function readmeAt(
  owner: string,
  repo: string,
  tag: string,
  path: string,
  fetchFn: FetchFn,
  label = `${owner}/${repo}@${tag}`,
): Promise<string> {
  return fetchBody(rawFileUrl(owner, repo, tag, path), fetchFn, label);
}

/**
 * Fetch a dog's README and every page it links to, at its latest release tag.
 *
 * Starts at `README.md`, renders it, follows each relative `.md` link resolved
 * against the page it sits on, and repeats to any depth. A page already
 * fetched is never fetched again, which is what makes a README that links its
 * sub-pages which link back to the README terminate rather than recurse.
 *
 * @param strict true stops the build on a failure; false warns once, naming
 * the dog, and returns `undefined`. The caller decides which, and this module
 * deliberately does not read it from the environment.
 * @returns the tag everything was read at, and every page keyed by
 * repository-relative path — `README.md` first. Rewriting the links is the
 * caller's call: `dog-readme-links.ts`'s `rewriteLinks` takes the tag, the
 * owner and the repo back out of `githubRepoOf(dog)` plus this result's `tag`.
 * @throws {Error} when `strict` and any fetch fails, including a repository
 * with no release, a release with no README, and a linked page that is not in
 * the repository at that tag.
 */
export async function fetchDogPages(
  dog: Dog,
  render: RenderMarkdown,
  fetchFn: FetchFn,
  strict: boolean,
): Promise<DogPages | undefined> {
  const label = `dog "${dog.package}"`;
  try {
    const { owner, repo } = githubRepoOf(dog);
    const tag = await latestTag(owner, repo, fetchFn, label);
    const pages = new Map<string, DogPage>();
    const queue: string[] = [README_PATH];

    while (queue.length > 0) {
      const path = queue.shift();
      if (path === undefined || pages.has(path)) continue;
      const markdown = await readmeAt(owner, repo, tag, path, fetchFn, label);
      const html = await render(markdown);
      pages.set(path, { markdown, html });
      for (const linked of findRelativeMdLinks(html, path)) {
        if (!pages.has(linked)) queue.push(linked);
      }
    }

    return { tag, pages };
  } catch (error) {
    if (strict) throw error;
    console.warn(`${label} was skipped: ${reason(error)}`);
    return undefined;
  }
}
