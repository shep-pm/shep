// Regression guard for a first-party dog's fetched docs pages
// (web/src/data/dog-readme-fetch.ts, web/src/data/dog-readme-links.ts and
// web/src/data/dog-readme-tables.ts).
//
// The next slice draws `/docs/dogs/<adopt_as>` from a dog repo's README at its
// latest release tag. That needs two things proven before any page exists: the
// fetch reaches the right URLs and stops on its own, and a README's links come
// out pointing at this site rather than at GitHub.
//
// The third module is the page slice's own: a README's markdown tables render
// as real `<table>` elements, which the site's phone-width table rules do not
// reach, so each cell past the first has to carry its column's heading as
// `data-label` before `dog-readme.css` can stack the table at all.
//
// Every case here runs against a fake `fetchFn`, so no case touches the
// network and no case can be skipped by a rate limit or a missing token. That
// is why both modules take `fetchFn` as an argument instead of calling
// `fetch` themselves.
//
// Runs under Node's built-in test runner and native TypeScript type stripping
// (`node --test scripts/verify-dog-readme.ts`), like verify-dogs-index.ts, so
// this project still adds no test framework for one module.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  FIRST_PARTY_REPO_PREFIX,
  README_PATH,
  fetchDogPages,
  firstPartyDogs,
  githubRepoOf,
  latestTag,
  readmeAt,
} from "../src/data/dog-readme-fetch.ts";
import type { DogPages, FetchFn, FetchInit, RenderMarkdown } from "../src/data/dog-readme-fetch.ts";
import {
  dogDocRoute,
  findRelativeMdLinks,
  resolveRepoPath,
  rewriteLinks,
} from "../src/data/dog-readme-links.ts";
import type { DogPageAddress } from "../src/data/dog-readme-links.ts";
import { labelTableCells } from "../src/data/dog-readme-tables.ts";
import { dogs } from "../src/data/dogs.ts";
import type { Dog } from "../src/data/dogs.ts";

/* --- fixtures ---------------------------------------------------------- */

const OWNER = "shep-pm";
const REPO = "shep-rex";
const TAG = "v1.2.0";
const ADOPT_AS = "rex";

const API_URL = `https://api.github.com/repos/${OWNER}/${REPO}/releases/latest`;
const rawUrl = (path: string): string =>
  `https://raw.githubusercontent.com/${OWNER}/${REPO}/${TAG}/${path}`;

function dogFixture(repo: string, overrides: Partial<Dog> = {}): Dog {
  return {
    name: "Rex",
    package: "shep-rex",
    adopt_as: ADOPT_AS,
    description: "Does a rex thing.",
    repo,
    license: "MIT",
    category: "other",
    source: { kind: "cargo" },
    ...overrides,
  };
}

const firstParty = dogFixture(`https://github.com/${OWNER}/${REPO}`);

const page: DogPageAddress = { owner: OWNER, repo: REPO, tag: TAG, adoptAs: ADOPT_AS, fromPath: "README.md" };

/** A body that is a rendered page: `findRelativeMdLinks` reads its `href` attributes. */
const body = (html: string): string => `<p>${html}</p>`;

/** Stand-in for the slice that picks a Markdown renderer, proven to be applied. */
const render: RenderMarkdown = (markdown: string) => `<div>${markdown}</div>`;

interface Stubbed {
  fetchFn: FetchFn;
  calls: { url: string; init?: FetchInit }[];
}

/**
 * A `fetchFn` over a URL table. A URL that is not in the table answers 404,
 * which is what GitHub answers for a missing file, a missing release and a
 * missing repository alike, so a fixture only lists the URLs it means to have.
 */
function stubFetch(routes: Record<string, string | { status: number; body?: string }>): Stubbed {
  const calls: { url: string; init?: FetchInit }[] = [];
  const fetchFn: FetchFn = async (url, init) => {
    calls.push({ url, init });
    const route = routes[url];
    if (route === undefined) return { ok: false, status: 404, text: async () => "" };
    if (typeof route === "string") return { ok: true, status: 200, text: async () => route };
    return {
      ok: route.status >= 200 && route.status < 300,
      status: route.status,
      text: async () => route.body ?? "",
    };
  };
  return { fetchFn, calls };
}

const releaseJson = (tag: string): string => JSON.stringify({ tag_name: tag });

/** How many times a stubbed fetch was asked for one URL. */
function hits(calls: { url: string }[], url: string): number {
  return calls.filter((call) => call.url === url).length;
}

/** Run `run` with `console.warn` collected instead of printed. */
async function captureWarnings(run: () => Promise<unknown>): Promise<string[]> {
  const warnings: string[] = [];
  const original = console.warn;
  console.warn = (...args: unknown[]) => {
    warnings.push(args.map(String).join(" "));
  };
  try {
    await run();
  } finally {
    console.warn = original;
  }
  return warnings;
}

/** Asserts one failure message carries the three things the policy requires. */
function assertNames(message: string, dog: string, url: string, status?: number): void {
  assert.ok(message.includes(dog), `the error does not name the dog: ${message}`);
  assert.ok(message.includes(url), `the error does not name the URL: ${message}`);
  if (status !== undefined) {
    assert.ok(message.includes(String(status)), `the error does not name the status: ${message}`);
  }
}

/* --- firstPartyDogs ---------------------------------------------------- */

test("firstPartyDogs keeps a row whose repo is under the org", () => {
  const kept = firstPartyDogs([dogFixture("https://github.com/shep-pm/shep-log-rotate")]);
  assert.equal(kept.length, 1);
  assert.equal(kept[0].package, "shep-rex");
});

test("firstPartyDogs drops a repo that is someone else's", () => {
  const kept = firstPartyDogs([dogFixture("https://github.com/someone/x")]);
  assert.deepEqual(kept, []);
});

test("firstPartyDogs drops a repo that only looks like the org", () => {
  const kept = firstPartyDogs([dogFixture("https://github.com/shep-pm-fake/x")]);
  assert.deepEqual(kept, []);
});

test("firstPartyDogs keeps every first-party row of the published index", () => {
  const kept = firstPartyDogs(dogs);
  assert.ok(kept.length >= 1, "the published index should have at least one first-party dog");
  for (const dog of kept) {
    assert.ok(dog.repo.startsWith(FIRST_PARTY_REPO_PREFIX), dog.repo);
  }
});

test("githubRepoOf reads the owner and repo a repo URL names", () => {
  assert.deepEqual(githubRepoOf(firstParty), { owner: OWNER, repo: REPO });
  assert.throws(
    () => githubRepoOf(dogFixture("https://gitlab.com/someone/x")),
    (error: Error) => {
      assertNames(error.message, "shep-rex", "https://gitlab.com/someone/x");
      return true;
    },
  );
});

/* --- latestTag, readmeAt ------------------------------------------------ */

test("latestTag reads tag_name from the releases/latest URL", async () => {
  const { fetchFn, calls } = stubFetch({ [API_URL]: releaseJson(TAG) });
  assert.equal(await latestTag(OWNER, REPO, fetchFn), TAG);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, API_URL);
});

test("latestTag sends Authorization: Bearer only when GITHUB_TOKEN is set", async () => {
  const { fetchFn, calls } = stubFetch({ [API_URL]: releaseJson(TAG) });
  const saved = process.env.GITHUB_TOKEN;
  try {
    process.env.GITHUB_TOKEN = "a-token";
    await latestTag(OWNER, REPO, fetchFn);
    assert.equal(calls[0].init?.headers?.Authorization, "Bearer a-token");

    delete process.env.GITHUB_TOKEN;
    await latestTag(OWNER, REPO, fetchFn);
    assert.equal(calls[1].init?.headers?.Authorization, undefined);
  } finally {
    if (saved === undefined) delete process.env.GITHUB_TOKEN;
    else process.env.GITHUB_TOKEN = saved;
  }
});

test("a release with no tag_name is a failure", async () => {
  const { fetchFn } = stubFetch({ [API_URL]: JSON.stringify({ name: "draft of something" }) });
  await assert.rejects(
    () => latestTag(OWNER, REPO, fetchFn, "dog \"shep-rex\""),
    (error: Error) => {
      assertNames(error.message, "shep-rex", API_URL);
      assert.ok(error.message.includes("tag_name"), error.message);
      return true;
    },
  );
});

test("readmeAt reads a file at the tag, not at the default branch", async () => {
  const { fetchFn, calls } = stubFetch({ [rawUrl("docs/setup.md")]: "setup text" });
  assert.equal(await readmeAt(OWNER, REPO, TAG, "docs/setup.md", fetchFn), "setup text");
  assert.equal(calls[0].url, rawUrl("docs/setup.md"));
});

test("a file that is not at the tag is a failure naming the URL and the status", async () => {
  const { fetchFn } = stubFetch({});
  await assert.rejects(
    () => readmeAt(OWNER, REPO, TAG, "docs/setup.md", fetchFn, 'dog "shep-rex"'),
    (error: Error) => {
      assertNames(error.message, "shep-rex", rawUrl("docs/setup.md"), 404);
      return true;
    },
  );
});

/* --- fetchDogPages: the failure policy ---------------------------------- */

test("a 404 from releases/latest throws naming the dog and the URL in strict mode", async () => {
  const { fetchFn } = stubFetch({ [API_URL]: { status: 404, body: '{"message":"Not Found"}' } });
  await assert.rejects(
    () => fetchDogPages(firstParty, render, fetchFn, true),
    (error: Error) => {
      assertNames(error.message, "shep-rex", API_URL, 404);
      return true;
    },
  );
});

test("a 404 from releases/latest warns once and returns undefined when not strict", async () => {
  const { fetchFn } = stubFetch({ [API_URL]: { status: 404 } });
  let result: DogPages | undefined;
  const warnings = await captureWarnings(async () => {
    result = await fetchDogPages(firstParty, render, fetchFn, false);
  });
  assert.equal(result, undefined);
  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].includes("shep-rex"), warnings[0]);
  assert.ok(warnings[0].includes(API_URL), warnings[0]);
});

test("a request that never gets a response is a failure either way", async () => {
  const boom: FetchFn = async () => {
    throw new TypeError("fetch failed");
  };
  await assert.rejects(
    () => fetchDogPages(firstParty, render, boom, true),
    (error: Error) => {
      assertNames(error.message, "shep-rex", API_URL);
      return true;
    },
  );
  let result: DogPages | undefined;
  const warnings = await captureWarnings(async () => {
    result = await fetchDogPages(firstParty, render, boom, false);
  });
  assert.equal(result, undefined);
  assert.equal(warnings.length, 1);
});

test("a release with no README is a failure", async () => {
  const { fetchFn } = stubFetch({ [API_URL]: releaseJson(TAG) });
  await assert.rejects(
    () => fetchDogPages(firstParty, render, fetchFn, true),
    (error: Error) => {
      assertNames(error.message, "shep-rex", rawUrl(README_PATH), 404);
      return true;
    },
  );
});

test("a linked page that is not in the repository at that tag is a failure", async () => {
  const { fetchFn } = stubFetch({
    [API_URL]: releaseJson(TAG),
    [rawUrl(README_PATH)]: body('<a href="docs/setup.md">setup</a>'),
  });
  await assert.rejects(
    () => fetchDogPages(firstParty, render, fetchFn, true),
    (error: Error) => {
      assertNames(error.message, "shep-rex", rawUrl("docs/setup.md"), 404);
      return true;
    },
  );
});

/* --- fetchDogPages: following .md links --------------------------------- */

test("a README's relative .md links are fetched to any depth, and it terminates", async () => {
  const { fetchFn, calls } = stubFetch({
    [API_URL]: releaseJson(TAG),
    [rawUrl(README_PATH)]: body('<a href="docs/a.md">a</a>'),
    [rawUrl("docs/a.md")]: body('<a href="../README.md">back</a> <a href="b/c.md">c</a>'),
    [rawUrl("docs/b/c.md")]: body("no links here"),
  });

  const result = await fetchDogPages(firstParty, render, fetchFn, true);
  assert.ok(result !== undefined);
  assert.equal(result.tag, TAG);
  assert.deepEqual([...result.pages.keys()], [README_PATH, "docs/a.md", "docs/b/c.md"]);
  assert.equal(result.pages.get("docs/b/c.md")?.markdown, body("no links here"));
  assert.equal(result.pages.get(README_PATH)?.html, `<div>${body('<a href="docs/a.md">a</a>')}</div>`);
  // Three pages, three fetches: nothing was asked for twice.
  assert.equal(calls.length, 4);
  assert.equal(hits(calls, rawUrl(README_PATH)), 1);
  assert.equal(hits(calls, rawUrl("docs/a.md")), 1);
  assert.equal(hits(calls, rawUrl("docs/b/c.md")), 1);
});

test("a link cycle between two .md pages terminates", async () => {
  const { fetchFn, calls } = stubFetch({
    [API_URL]: releaseJson(TAG),
    [rawUrl(README_PATH)]: body('<a href="docs/a.md">a</a> <a href="docs/b.md">b</a>'),
    [rawUrl("docs/a.md")]: body('<a href="b.md">b</a> <a href="../README.md">home</a>'),
    [rawUrl("docs/b.md")]: body('<a href="a.md">a</a> <a href="../README.md">home</a>'),
  });

  const result = await fetchDogPages(firstParty, render, fetchFn, true);
  assert.ok(result !== undefined);
  assert.deepEqual([...result.pages.keys()], [README_PATH, "docs/a.md", "docs/b.md"]);
  assert.equal(hits(calls, rawUrl(README_PATH)), 1);
  assert.equal(hits(calls, rawUrl("docs/a.md")), 1);
  assert.equal(hits(calls, rawUrl("docs/b.md")), 1);
});

test("a page that links itself is fetched once", async () => {
  const { fetchFn, calls } = stubFetch({
    [API_URL]: releaseJson(TAG),
    [rawUrl(README_PATH)]: body('<a href="README.md">this page</a>'),
  });
  const result = await fetchDogPages(firstParty, render, fetchFn, true);
  assert.ok(result !== undefined);
  assert.deepEqual([...result.pages.keys()], [README_PATH]);
  assert.equal(hits(calls, rawUrl(README_PATH)), 1);
});

/* --- rewriteLinks: what stays ------------------------------------------- */

function rewritten(html: string, overrides: Partial<DogPageAddress> = {}): string {
  return rewriteLinks(html, { ...page, ...overrides });
}

test("an absolute URL, a mailto and a bare fragment stay as written", () => {
  const html =
    '<a href="https://example.com/x">x</a> <a href="mailto:hi@example.com">mail</a> ' +
    '<a href="#setup">top</a> <a href="//example.com/y">y</a> <a href="/docs/terminology">t</a>';
  assert.equal(rewritten(html), html);
});

test("attributes this file does not rewrite are copied out byte for byte", () => {
  assert.equal(
    rewritten('<a href="docs/a.md" title="docs/b.md" class="x" data-tag="docs/c.md">a</a>'),
    '<a href="/docs/dogs/rex/docs/a" title="docs/b.md" class="x" data-tag="docs/c.md">a</a>',
  );
});

test("a quoted value containing > does not end the tag", () => {
  assert.equal(
    rewritten('<a href="docs/a.md" title="a > b">a</a>'),
    '<a href="/docs/dogs/rex/docs/a" title="a > b">a</a>',
  );
});

test("text is never rewritten, only attributes", () => {
  const html = '<p>Write &lt;a href="docs/a.md"&gt; in your README.</p>';
  assert.equal(rewritten(html), html);
});

test("a code block is left alone, escaped or not", () => {
  const escaped = '<pre><code class="language-html">&lt;a href="docs/a.md"&gt;a&lt;/a&gt;</code></pre>';
  assert.equal(rewritten(escaped), escaped);
  // A renderer that did not escape the example still cannot get its link rewritten.
  assert.equal(
    rewritten('<p>Write <code><a href="docs/a.md">x</a></code> in your README.</p>'),
    '<p>Write <code><a href="docs/a.md">x</a></code> in your README.</p>',
  );
});

test("an HTML comment is left alone", () => {
  const html = '<!-- <a href="docs/a.md">a</a> --><p><a href="docs/a.md">a</a></p>';
  assert.equal(rewritten(html), '<!-- <a href="docs/a.md">a</a> --><p><a href="/docs/dogs/rex/docs/a">a</a></p>');
});

/* --- rewriteLinks: a relative .md link ---------------------------------- */

test("a relative .md link becomes a page under the dog's page", () => {
  assert.equal(rewritten('<a href="docs/a.md">a</a>'), '<a href="/docs/dogs/rex/docs/a">a</a>');
  assert.equal(rewritten('<a href="docs/a.md#setup">a</a>'), '<a href="/docs/dogs/rex/docs/a#setup">a</a>');
  assert.equal(rewritten('<a href="a.md">a</a>'), '<a href="/docs/dogs/rex/a">a</a>');
});

test("README.md at the root is the dog's page itself", () => {
  assert.equal(rewritten('<a href="README.md">home</a>'), '<a href="/docs/dogs/rex">home</a>');
  assert.equal(rewritten('<a href="./README.md">home</a>'), '<a href="/docs/dogs/rex">home</a>');
  assert.equal(rewritten('<a href="docs/../README.md#top">home</a>'), '<a href="/docs/dogs/rex#top">home</a>');
  assert.equal(
    rewritten('<a href="docs/../README.md">home</a>', { fromPath: "docs/deep/inner.md" }),
    '<a href="/docs/dogs/rex/docs/deep/README">home</a>',
  );
});

test("a relative link resolves against the directory of the page it sits on", () => {
  const from = { fromPath: "docs/a.md" };
  assert.equal(rewritten('<a href="b/c.md">c</a>', from), '<a href="/docs/dogs/rex/docs/b/c">c</a>');
  assert.equal(rewritten('<a href="../README.md">home</a>', from), '<a href="/docs/dogs/rex">home</a>');
  assert.equal(rewritten('<a href="./same.md">same</a>', from), '<a href="/docs/dogs/rex/docs/same">same</a>');
  assert.equal(rewritten('<a href="../other/deep.md">deep</a>', from), '<a href="/docs/dogs/rex/other/deep">deep</a>');
});

test("a link that climbs above the repository root is an error", () => {
  assert.throws(
    () => rewritten('<a href="../../escape.md">escape</a>', { fromPath: "docs/a.md" }),
    (error: Error) => {
      assert.ok(error.message.includes("../../escape.md"), error.message);
      assert.ok(error.message.includes("docs/a.md"), error.message);
      assert.ok(error.message.includes("repository root"), error.message);
      return true;
    },
  );
  assert.throws(() => resolveRepoPath("..", "README.md"), /repository root/);
});

/* --- rewriteLinks: images and other files ------------------------------- */

test("a relative image points at the tag on raw.githubusercontent.com", () => {
  assert.equal(
    rewritten('<img src="docs/img/diagram.png" alt="diagram" width="400">'),
    `<img src="${rawUrl("docs/img/diagram.png")}" alt="diagram" width="400">`,
  );
  assert.equal(
    rewritten('<img src="../logo.svg">', { fromPath: "docs/a.md" }),
    `<img src="${rawUrl("logo.svg")}">`,
  );
});

test("any other relative file points at the repo at that tag", () => {
  assert.equal(
    rewritten('<a href="examples/flockfile.toml">flockfile</a>'),
    `<a href="https://github.com/${OWNER}/${REPO}/blob/${TAG}/examples/flockfile.toml">flockfile</a>`,
  );
  assert.equal(
    rewritten('<a href="LICENSE">license</a>'),
    `<a href="https://github.com/${OWNER}/${REPO}/blob/${TAG}/LICENSE">license</a>`,
  );
  assert.equal(
    rewritten('<a href="src/main.rs">source</a>'),
    `<a href="https://github.com/${OWNER}/${REPO}/blob/${TAG}/src/main.rs">source</a>`,
  );
});

test("a src names bytes, so it gets the file at the tag whatever its extension", () => {
  assert.equal(
    rewritten('<video src="docs/demo.mp4" controls></video>'),
    `<video src="${rawUrl("docs/demo.mp4")}" controls></video>`,
  );
  assert.equal(rewritten('<source src="clip.webm">'), `<source src="${rawUrl("clip.webm")}">`);
  assert.equal(
    rewritten('<video poster="docs/preview.png"></video>'),
    `<video poster="${rawUrl("docs/preview.png")}"></video>`,
  );
});

test("an href names a page, so a file gets the page GitHub renders for it", () => {
  assert.equal(
    rewritten('<a href="docs/demo.mp4">watch</a>'),
    `<a href="https://github.com/${OWNER}/${REPO}/blob/${TAG}/docs/demo.mp4">watch</a>`,
  );
  assert.equal(
    rewritten('<a href="docs/logo.png">logo</a>'),
    `<a href="https://github.com/${OWNER}/${REPO}/blob/${TAG}/docs/logo.png">logo</a>`,
  );
});

test("a srcset is rewritten candidate by candidate, descriptors and spacing kept", () => {
  assert.equal(
    rewritten(
      '<picture><source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png 1x, docs/logo-dark@2x.png 2x">' +
        '<img src="docs/logo.png" alt="logo"></picture>',
    ),
    `<picture><source media="(prefers-color-scheme: dark)" srcset="${rawUrl("docs/logo-dark.png")} 1x, ` +
      `${rawUrl("docs/logo-dark@2x.png")} 2x"><img src="${rawUrl("docs/logo.png")}" alt="logo"></picture>`,
  );
});

test("an absolute candidate in a srcset stays as written beside a relative one", () => {
  assert.equal(
    rewritten('<img srcset="https://cdn.example.com/a.png 2x, docs/b.png 1x" src="docs/b.png">'),
    `<img srcset="https://cdn.example.com/a.png 2x, ${rawUrl("docs/b.png")} 1x" src="${rawUrl("docs/b.png")}">`,
  );
});

test("a width descriptor in a srcset is kept with the URL it describes", () => {
  assert.equal(
    rewritten('<img srcset="docs/a-480.png 480w, docs/a-960.png 960w" sizes="(max-width: 600px) 480px, 960px">'),
    `<img srcset="${rawUrl("docs/a-480.png")} 480w, ${rawUrl("docs/a-960.png")} 960w" sizes="(max-width: 600px) 480px, 960px">`,
  );
});

/* --- rewriteLinks: character references --------------------------------- */

test("a character reference in an href is decoded before the path is used", () => {
  const html = '<a href="docs/q&amp;a.md">q</a> <a href="docs/&#38;b.md">b</a>';
  assert.deepEqual(findRelativeMdLinks(html, "README.md"), ["docs/q&a.md", "docs/&b.md"]);
  assert.equal(
    rewritten(html),
    '<a href="/docs/dogs/rex/docs/q&amp;a">q</a> <a href="/docs/dogs/rex/docs/&amp;b">b</a>',
  );
  // An unknown reference is left as written rather than guessed at.
  assert.deepEqual(findRelativeMdLinks('<a href="docs/&foo;.md">x</a>', "README.md"), ["docs/&foo;.md"]);
});

test("a linked page named with a character reference is fetched by its real name", async () => {
  const { fetchFn, calls } = stubFetch({
    [API_URL]: releaseJson(TAG),
    [rawUrl(README_PATH)]: body('<a href="docs/q&amp;a.md">q</a>'),
    [rawUrl("docs/q&a.md")]: body("no links here"),
  });
  const result = await fetchDogPages(firstParty, render, fetchFn, true);
  assert.ok(result !== undefined);
  assert.deepEqual([...result.pages.keys()], [README_PATH, "docs/q&a.md"]);
  assert.equal(hits(calls, rawUrl("docs/q&a.md")), 1);
});

test("an unquoted or single-quoted href is rewritten and re-quoted", () => {
  assert.equal(rewritten("<a href=docs/a.md>a</a>"), '<a href="/docs/dogs/rex/docs/a">a</a>');
  assert.equal(rewritten("<a href='docs/a.md'>a</a>"), "<a href='/docs/dogs/rex/docs/a'>a</a>");
});

test("dogDocRoute maps a repo path to this site's route", () => {
  assert.equal(dogDocRoute(ADOPT_AS, "README.md"), "/docs/dogs/rex");
  assert.equal(dogDocRoute(ADOPT_AS, "docs/a.md"), "/docs/dogs/rex/docs/a");
  assert.equal(dogDocRoute(ADOPT_AS, "docs/deep/deeper.md"), "/docs/dogs/rex/docs/deep/deeper");
});

/* --- findRelativeMdLinks ------------------------------------------------ */

test("findRelativeMdLinks returns the pages to fetch, and only those", () => {
  const html =
    '<a href="docs/a.md">a</a> <a href="docs/a.md#x">again</a> <a href="img.png">img</a> ' +
    '<a href="LICENSE">file</a> <a href="https://example.com/x/y.md">off-site</a> ' +
    '<a href="#top">top</a> <a href="/docs/terminology">site</a> <a href="./README.md">home</a>';
  assert.deepEqual(findRelativeMdLinks(html, "README.md"), ["docs/a.md", "docs/a.md", "README.md"]);
  assert.deepEqual(findRelativeMdLinks(html, "docs/inner.md"), [
    "docs/docs/a.md",
    "docs/docs/a.md",
    "docs/README.md",
  ]);
});

test("the links that get a page and the links that get rewritten are the same links", () => {
  const html = '<a href="docs/a.md">a</a> <a href="docs/b.md#x">b</a> <a href="img.png">img</a>';
  const fetched = findRelativeMdLinks(html, page.fromPath);
  const hrefs = [...rewritten(html).matchAll(/href="([^"]+)"/g)]
    .map((match) => match[1])
    .map((href) => href.split("#")[0]);
  for (const path of fetched) {
    assert.ok(hrefs.includes(dogDocRoute(ADOPT_AS, path)), `${path} was fetched but not rewritten`);
  }
  assert.equal(hrefs.length, 3);
});

/* --- labelTableCells ---------------------------------------------------- */

/** A three-column table, the shape a markdown pipe table renders to. */
const grid = `<table><thead><tr><th>code</th><th>means</th><th>the flock is</th></tr></thead>` +
  `<tbody><tr><td><code>12</code></td><td>the deploy was rejected</td><td>healthy</td></tr></tbody></table>`;

test("labelTableCells() puts the header text on every cell past the first", () => {
  const out = labelTableCells(grid);
  assert.ok(out.includes(`<td data-label="means">`), out);
  assert.ok(out.includes(`<td data-label="the flock is">`), out);
});

test("labelTableCells() leaves the first cell of a row unlabelled", () => {
  const out = labelTableCells(grid);
  assert.ok(out.includes("<td><code>12</code></td>"), out);
  assert.equal(out.match(/data-label/g)?.length, 2, out);
});

test("labelTableCells() leaves a table with no header cells alone", () => {
  const html = `<table><tbody><tr><td>one</td><td>two</td></tr></tbody></table>`;
  assert.equal(labelTableCells(html), html);
  // A header row of `td` is still no header row.
  const rows = `<table><tr><td>one</td><td>two</td></tr><tr><td>three</td><td>four</td></tr></table>`;
  assert.equal(labelTableCells(rows), rows);
});

test("labelTableCells() labels every body row, not only the first", () => {
  const html = `<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody>` +
    `<tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody></table>`;
  const out = labelTableCells(html);
  assert.equal(out.match(/data-label="b"/g)?.length, 2, out);
});

test("labelTableCells() keeps a label the README already wrote", () => {
  const html = `<table><thead><tr><th>a</th><th>b</th></tr></thead><tr><td>1</td><td data-label="own">2</td></tr></table>`;
  assert.equal(labelTableCells(html), html);
});

test("labelTableCells() leaves a column the header row does not name unlabelled", () => {
  // A row header: the `th` names the row, and the header row has nothing for it.
  const html = `<table><thead><tr><th></th><th>bark</th></tr></thead><tr><td>alerts</td><td>yes</td></tr></table>`;
  const out = labelTableCells(html);
  assert.ok(out.includes(`<td data-label="bark">yes</td>`), out);
  assert.ok(out.includes("<td>alerts</td>"), out);
});

test("labelTableCells() strips a header cell's tags and collapses its whitespace", () => {
  const html = `<table><thead><tr><th>a</th><th>the\n  <em>flock</em> is</th></tr></thead><tr><td>x</td><td>y</td></tr></table>`;
  assert.ok(labelTableCells(html).includes(`<td data-label="the flock is">y</td>`), labelTableCells(html));
});

test("labelTableCells() decodes a header cell's character references and re-escapes them", () => {
  const html = `<table><thead><tr><th>a</th><th>Q&amp;a</th><th>&quot;quoted&quot;</th></tr></thead>` +
    `<tr><td>x</td><td>y</td><td>z</td></tr></table>`;
  const out = labelTableCells(html);
  assert.ok(out.includes(`<td data-label="Q&amp;a">y</td>`), out);
  assert.ok(out.includes(`<td data-label="&quot;quoted&quot;">z</td>`), out);
});

test("labelTableCells() copies out everything outside a cell's opening tag unchanged", () => {
  const html = `<p>See the <code>&lt;table&gt;</code> below.</p>` +
    `<table class="wide" style="text-align:left"><thead><tr><th class="h">a</th><th>b</th></tr></thead>` +
    `<tr><td style="text-align:center">1</td><td>2</td></tr></table>`;
  const out = labelTableCells(html);
  assert.ok(out.includes(`<td style="text-align:center">1</td>`), out);
  assert.ok(out.includes(`<td data-label="b">2</td>`), out);
  assert.ok(out.includes(`<table class="wide" style="text-align:left">`), out);
  assert.ok(out.includes(`<th class="h">a</th>`), out);
  assert.ok(out.includes(`<p>See the <code>&lt;table&gt;</code> below.</p>`), out);
});

test("labelTableCells() reads a tag whose quoted attribute holds >", () => {
  const html = `<table><thead><tr><th>a</th><th title="x > y">b</th></tr></thead>` +
    `<tr><td title="p > q">1</td><td>2</td></tr></table>`;
  assert.equal(
    labelTableCells(html),
    `<table><thead><tr><th>a</th><th title="x > y">b</th></tr></thead>` +
      `<tr><td title="p > q">1</td><td data-label="b">2</td></tr></table>`,
  );
});

test("labelTableCells() ends a table at its own closing tag, not a nested one's", () => {
  const html = `<table><thead><tr><th>outer</th><th>col</th></tr></thead><tr><td><table>` +
    `<thead><tr><th>in</th></tr></thead><tr><td>ner</td></tr></table></td><td>tail</td></tr></table>`;
  assert.ok(labelTableCells(html).includes(`<td data-label="col">tail</td>`), labelTableCells(html));
});

test("labelTableCells() labels a table inside a cell from its own header row", () => {
  // `dog-readme.css` stacks a nested table too, so its cells need labels or a
  // phone reader sees values with nothing naming their column.
  const html = `<table><thead><tr><th>outer</th><th>col</th></tr></thead><tr><td><table>` +
    `<thead><tr><th>in</th><th>ner</th></tr></thead><tr><td>a</td><td>b</td></tr></table>` +
    `</td><td>tail</td></tr></table>`;
  const out = labelTableCells(html);
  assert.ok(out.includes(`<td data-label="ner">b</td>`), out);
  assert.ok(out.includes(`<td data-label="col">tail</td>`), out);
});

test("labelTableCells() never reads a nested table's cells as the outer table's", () => {
  // The inner table's `b` is not a column of the outer one, and the outer's
  // `col` is not a column of the inner one.
  const html = `<table><thead><tr><th>outer</th><th>col</th></tr></thead><tr><td><table>` +
    `<thead><tr><th>in</th><th>ner</th></tr></thead><tr><td>a</td><td>b</td></tr></table>` +
    `</td><td>tail</td></tr></table>`;
  const out = labelTableCells(html);
  assert.ok(!out.includes(`<td data-label="col">b`), out);
  assert.ok(!out.includes(`<td data-label="ner">tail`), out);
  assert.ok(out.includes("<td>a</td>"), out);
});

test("labelTableCells() returns the document untouched when it has no table", () => {
  const html = `<h2 id="x">X</h2><p>text with a <code>table</code> in it</p>`;
  assert.equal(labelTableCells(html), html);
});

/* --- the shape of the modules themselves --------------------------------- */

test("no dog-readme module imports anything from astro, and none reads the environment", async () => {
  const fetchSource = await readFile(new URL("../src/data/dog-readme-fetch.ts", import.meta.url), "utf8");
  const linksSource = await readFile(new URL("../src/data/dog-readme-links.ts", import.meta.url), "utf8");
  const tablesSource = await readFile(new URL("../src/data/dog-readme-tables.ts", import.meta.url), "utf8");
  for (const source of [fetchSource, linksSource, tablesSource]) {
    assert.ok(!/from\s+["']astro:/.test(source), "a data module the test runner loads cannot import from astro:");
    assert.ok(!/from\s+["'][^"']*\?raw/.test(source), "nor from a build-time ?raw import");
    assert.ok(!source.includes("import.meta"), "neither module reads the build environment");
  }
  assert.equal((fetchSource.match(/process\.env/g) ?? []).length, 1);
  assert.ok(fetchSource.includes('const TOKEN_ENV_VAR = "GITHUB_TOKEN"'), fetchSource);
  assert.ok(!linksSource.includes("process.env"), "the link rewriter needs no environment at all");
});
