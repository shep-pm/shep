/*
 * Rewriting a fetched dog README's links so they work on this site.
 *
 * A first-party dog's docs page is that dog's own README, fetched at the tag
 * of its latest release (see `dog-readme-fetch.ts`). The README's links are
 * written for GitHub — `docs/setup.md`, `img/diagram.png`, `LICENSE` — and on
 * GitHub a relative link resolves against the directory of the file it sits
 * in. On this site the same page lives at `/docs/dogs/<adopt_as>`, so a
 * relative link left as written resolves against the site root and 404s.
 *
 * The rules are the ones decided in `docs/decisions.md` under "A first-party
 * dog's docs page":
 *
 * - an absolute URL, a `mailto:` and a bare `#fragment` stay as written, so a
 *   dog keeps a link off this site by making it absolute;
 * - a relative `.md` link becomes a page under this dog's page, at any depth,
 *   and keeps its `#fragment`;
 * - a relative file in `src`, `srcset` or `poster` is fetched at the tag from
 *   `raw.githubusercontent.com`, because those attributes name bytes a browser
 *   loads, and the page GitHub renders for a file is HTML, not those bytes;
 * - a relative file in `href` points at the repo at that tag, on the page
 *   GitHub renders for it, because an `href` is a place a reader goes.
 *
 * So raw versus blob is decided by the attribute, not by the file extension.
 * An extension list cannot get this right: `docs/logo.png` in an `<img src>`
 * needs the bytes and `docs/logo.png` in an `<a href>` needs the page GitHub
 * renders, and one list of "image extensions" would answer the same file two
 * ways depending on where it was written.
 *
 * Why a tag walker rather than a regex over the document: the rule is about
 * attributes of tags, and a regex over the whole document cannot tell an
 * attribute from prose that happens to look like one. A README that shows a
 * reader an example `<a href="docs/a.md">` — inside a code fence, or as
 * escaped text in a paragraph — must come out unchanged, and rewriting it
 * would put a link into the reader's example. So this walks tags, edits only
 * `href` and `src`, and never touches anything between a tag's `>` and the
 * next `<`.
 *
 * It is a scanner, not an HTML parser: it knows tags, attributes, quoted
 * values and comments, which is everything rendered Markdown emits. It does
 * not build a tree, and nothing here needs one.
 */

/**
 * The attributes this file rewrites, and nothing else.
 *
 * `srcset` is in the list because GitHub's light/dark logo pattern puts the
 * dark image there and the light one in `src`: rewriting only `src` leaves the
 * `<source srcset="docs/logo-dark.png">` relative, which resolves against the
 * site path and 404s for every reader whose media query selects it. `poster`
 * is the same defect on a `<video>`.
 */
const LINK_ATTRIBUTES = new Set(["href", "src", "srcset", "poster"]);

/** Of those, the ones that name bytes a browser loads rather than a page a reader goes to. */
const FETCH_ATTRIBUTES = new Set(["src", "srcset", "poster"]);

/**
 * Elements whose CONTENTS are never rewritten.
 *
 * A correct Markdown renderer escapes a `<` inside a code span or fence to
 * `&lt;`, which this scanner cannot read as a tag, so an escaped example is
 * already safe. This set is the belt to that brace: it holds even for a
 * renderer that hands back an unescaped `<a href="...">` inside a `<code>`,
 * and a code sample is exactly where a README puts a literal tag.
 */
const NO_REWRITE_INSIDE = new Set(["pre", "code", "script", "style"]);

/** `https:`, `mailto:`, `tel:`, `data:` — anything with a scheme is already absolute. */
const SCHEME = /^[a-zA-Z][a-zA-Z0-9+.-]*:/;

/** Where a file lives at a tag, for a browser: the page GitHub renders for it. */
function blobFileUrl(owner: string, repo: string, tag: string, path: string): string {
  return `https://github.com/${owner}/${repo}/blob/${tag}/${path}`;
}

/**
 * Where a file lives at a tag, as bytes. `dog-readme-fetch.ts` imports this
 * so the URL it fetches a README from and the URL a rewritten image points at
 * cannot ever be spelled two ways.
 */
export function rawFileUrl(owner: string, repo: string, tag: string, path: string): string {
  return `https://raw.githubusercontent.com/${owner}/${repo}/${tag}/${path}`;
}

/** A path is a Markdown page when its extension is `.md`, whatever its case. */
function hasMdExtension(path: string): boolean {
  return path.toLowerCase().endsWith(".md");
}

/**
 * Which page of this site a repo-relative `.md` path becomes: the dog's own
 * page for `README.md`, and a page under it for anything deeper. A `path`
 * with no `.md` is passed through as written, because only a Markdown page
 * has a route here.
 */
export function dogDocRoute(adoptAs: string, path: string): string {
  const base = `/docs/dogs/${adoptAs}`;
  // The root README is the dog's page itself, not a page under it, and a link
  // that resolves up to the repository root means the same thing.
  if (path === "" || path === "README.md") return base;
  if (!hasMdExtension(path)) return `${base}/${path}`;
  return `${base}/${path.slice(0, path.length - 3)}`;
}

/**
 * The character references an attribute value can hold, which is what a
 * Markdown renderer writes for the five characters HTML reserves.
 */
const NAMED_ENTITIES = new Map([
  ["amp", "&"],
  ["apos", "'"],
  ["gt", ">"],
  ["lt", "<"],
  ["quot", '"'],
]);

/**
 * Decode the character references in an attribute value, so `docs/q&amp;a.md`
 * is read as the file `docs/q&a.md`.
 *
 * A renderer escapes `&` in an href, and a path read as written asks
 * raw.githubusercontent.com for a file called `q&amp;a.md`, which is not in
 * the repository: a 404 that, in strict mode, stops a deploy over a filename.
 * An unknown reference is left as written, because guessing what `&foo;` meant
 * is how a wrong path gets made.
 */
function decodeCharacterReferences(value: string): string {
  if (!value.includes("&")) return value;
  return value.replace(
    /&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g,
    (whole: string, body: string) => {
      if (body.startsWith("#")) {
        const hex = body[1] === "x" || body[1] === "X";
        const code = Number.parseInt(hex ? body.slice(2) : body.slice(1), hex ? 16 : 10);
        if (Number.isNaN(code) || code < 0 || code > 0x10ffff) return whole;
        return String.fromCodePoint(code);
      }
      return NAMED_ENTITIES.get(body.toLowerCase()) ?? whole;
    },
  );
}

/**
 * Resolve a link written inside `fromPath` to a path in the repository, using
 * the rule GitHub uses: relative to the DIRECTORY of the file, not to the
 * file itself, so `b/c.md` inside `docs/a.md` is `docs/b/c.md`.
 *
 * @returns the repository-relative path, or `""` when the target resolves to
 * the repository root (`.` from a root file, `..` from a top-level one).
 * @throws {Error} when the target climbs above the repository root, which no
 * file in the repository can be.
 */
export function resolveRepoPath(target: string, fromPath: string): string {
  const segments = fromPath.split("/").slice(0, -1);
  for (const part of target.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (segments.length === 0) {
        throw new Error(
          `"${target}" in ${fromPath} climbs above the repository root, so it names no file in the repository.`,
        );
      }
      segments.pop();
      continue;
    }
    segments.push(part);
  }
  return segments.join("/");
}

/** A link relative to the page it sits on, split into its path and its fragment. */
interface RelativeLink {
  target: string;
  fragment: string;
}

/**
 * `undefined` for every link this file leaves alone: an empty one, a bare
 * `#fragment`, a protocol-relative `//host/path`, anything with a scheme
 * (`https:`, `mailto:`), and a root-relative `/docs/terminology`, which is
 * already a working link on this site — the site writes all its own links that
 * way.
 */
function relativeLink(value: string): RelativeLink | undefined {
  const raw = decodeCharacterReferences(value.trim());
  if (raw === "") return undefined;
  if (raw.startsWith("#") || raw.startsWith("/")) return undefined;
  if (SCHEME.test(raw)) return undefined;
  const hash = raw.indexOf("#");
  if (hash === -1) return { target: raw, fragment: "" };
  return { target: raw.slice(0, hash), fragment: raw.slice(hash) };
}

/** Where the page being rewritten lives, which is what a relative link resolves against. */
export interface DogPageAddress {
  owner: string;
  repo: string;
  /** The release tag every repo file is addressed at. */
  tag: string;
  /** The `adopt_as` of the dog, which is the slug its page is filed under. */
  adoptAs: string;
  /** This page's own path in the repository, e.g. `README.md` or `docs/setup.md`. */
  fromPath: string;
}

/**
 * One relative link, resolved to a URL on this site or at the tag.
 *
 * @param fetched true when the attribute names bytes a browser loads, which is
 * the file itself, and false when it names a page a reader goes to, which is
 * the page GitHub renders for the file.
 */
function rewriteTarget(
  value: string,
  page: DogPageAddress,
  fetched: boolean,
): string | undefined {
  const link = relativeLink(value);
  if (link === undefined) return undefined;
  const path = resolveRepoPath(link.target, page.fromPath);
  if (path === "" || hasMdExtension(path)) {
    return dogDocRoute(page.adoptAs, path) + link.fragment;
  }
  if (fetched) return rawFileUrl(page.owner, page.repo, page.tag, path) + link.fragment;
  return blobFileUrl(page.owner, page.repo, page.tag, path) + link.fragment;
}

/**
 * One `srcset`, which is a comma-separated list of candidates, each a URL plus
 * an optional `2x` or `300w` descriptor. Every candidate is rewritten on its
 * own, so an absolute one in the same list stays as written, and the
 * descriptors and the spacing around them are copied out as they were.
 */
function rewriteSrcset(value: string, page: DogPageAddress): string | undefined {
  const candidates = value.split(",");
  const rewritten: string[] = [];
  let changed = false;
  for (const candidate of candidates) {
    const lead = candidate.slice(0, candidate.length - candidate.trimStart().length);
    const trimmed = candidate.trim();
    const trail = candidate.slice(candidate.trimEnd().length);
    const space = trimmed.search(/\s/);
    const url = space === -1 ? trimmed : trimmed.slice(0, space);
    const descriptor = space === -1 ? "" : trimmed.slice(space);
    const replacement = rewriteTarget(url, page, true);
    if (replacement === undefined) {
      rewritten.push(candidate);
      continue;
    }
    changed = true;
    rewritten.push(`${lead}${replacement}${descriptor}${trail}`);
  }
  return changed ? rewritten.join(",") : undefined;
}

/** One `href`, `src`, `srcset` or `poster` the walker reports, and what the visitor returns for it. */
interface LinkAttribute {
  /** Which of the four it was. */
  attribute: string;
  value: string;
}

/**
 * Rewrite every relative link in rendered HTML.
 *
 * Text is never touched, only the `href`, `src`, `srcset` and `poster` of
 * tags, and never inside `pre`, `code`, `script` or `style`. Everything else
 * in the document is copied out byte for byte, including comments and
 * attributes this file does not rewrite.
 *
 * @throws {Error} when a relative link climbs above the repository root.
 */
export function rewriteLinks(html: string, page: DogPageAddress): string {
  return walkLinkAttributes(html, ({ attribute, value }) =>
    attribute === "srcset"
      ? rewriteSrcset(value, page)
      : rewriteTarget(value, page, FETCH_ATTRIBUTES.has(attribute)),
  );
}

/**
 * The repository paths of every relative `.md` link in rendered HTML, in
 * document order, resolved against `fromPath` and without their fragments.
 * Duplicates are kept; the caller that follows them dedupes.
 *
 * This is how `fetchDogPages` finds the sub-pages to fetch, and it shares the
 * walker and the resolution rule with `rewriteLinks` so the links that get a
 * page and the links that get rewritten cannot disagree about which links they
 * are.
 *
 * @throws {Error} when a relative link climbs above the repository root.
 */
export function findRelativeMdLinks(html: string, fromPath: string): string[] {
  const paths: string[] = [];
  walkLinkAttributes(html, ({ attribute, value }) => {
    if (attribute !== "href") return undefined;
    const link = relativeLink(value);
    if (link === undefined) return undefined;
    const path = resolveRepoPath(link.target, fromPath);
    if (path !== "" && hasMdExtension(path)) paths.push(path);
    return undefined;
  });
  return paths;
}

/* --- the scanner ------------------------------------------------------- */

/** An attribute's value, located by index so it can be replaced in place. */
interface Attribute {
  name: string;
  /** First character of the value itself, quotes excluded. */
  valueStart: number;
  valueEnd: number;
  /**
   * The whole value token, quotes included, which is what a replacement
   * overwrites: replacing between the quotes and leaving the quotes in place
   * would write one too many.
   */
  tokenStart: number;
  tokenEnd: number;
  /** The quote the value was written in, or `""` when it was written bare. */
  quote: string;
}

interface Tag {
  /** Lowercase element name. `""` means this was not a tag at all. */
  name: string;
  closing: boolean;
  selfClosing: boolean;
  start: number;
  /** Index just past the tag's own text. */
  end: number;
  attributes: Attribute[];
}

function charAt(html: string, index: number): string {
  return index < html.length ? html[index] : "";
}

function isAsciiLetter(char: string): boolean {
  return (char >= "a" && char <= "z") || (char >= "A" && char <= "Z");
}

function isNameChar(char: string): boolean {
  return isAsciiLetter(char) || char === "-" || (char >= "0" && char <= "9");
}

function isSpaceChar(char: string): boolean {
  return char === " " || char === "\t" || char === "\n" || char === "\r" || char === "\f";
}

/**
 * Read the tag starting at `start`, where `html[start]` is `<`.
 *
 * A comment, a doctype, and a `<` that starts nothing (`<` in prose, `<3`)
 * all come back with an empty `name`, which is how the walker knows to copy
 * them out untouched. A quoted value may contain `>`, so the tag's end is
 * found by parsing its attributes rather than by scanning for `>`.
 */
function scanTag(html: string, start: number): Tag {
  if (html.startsWith("<!--", start)) {
    const close = html.indexOf("-->", start + 4);
    return {
      name: "",
      closing: false,
      selfClosing: false,
      start,
      end: close === -1 ? html.length : close + 3,
      attributes: [],
    };
  }

  let i = start + 1;
  let closing = false;
  if (charAt(html, i) === "/") {
    closing = true;
    i += 1;
  }
  if (!isAsciiLetter(charAt(html, i))) {
    return { name: "", closing: false, selfClosing: false, start, end: start + 1, attributes: [] };
  }

  const nameStart = i;
  while (isNameChar(charAt(html, i))) i += 1;
  const name = html.slice(nameStart, i).toLowerCase();

  const attributes: Attribute[] = [];
  let selfClosing = false;
  let end = html.length;

  while (i < html.length) {
    while (isSpaceChar(charAt(html, i))) i += 1;
    const char = charAt(html, i);
    if (char === "") break;
    if (char === ">") {
      end = i + 1;
      break;
    }
    if (char === "/") {
      if (charAt(html, i + 1) === ">") {
        selfClosing = true;
        end = i + 2;
        break;
      }
      i += 1;
      continue;
    }

    const nameStartAt = i;
    while (i < html.length) {
      const next = charAt(html, i);
      if (isSpaceChar(next) || next === "=" || next === ">" || next === "/") break;
      i += 1;
    }
    const attribute: Attribute = {
      name: html.slice(nameStartAt, i).toLowerCase(),
      valueStart: i,
      valueEnd: i,
      tokenStart: i,
      tokenEnd: i,
      quote: "",
    };

    let equals = i;
    while (isSpaceChar(charAt(html, equals))) equals += 1;
    if (charAt(html, equals) === "=") {
      let value = equals + 1;
      while (isSpaceChar(charAt(html, value))) value += 1;
      const quote = charAt(html, value);
      if (quote === '"' || quote === "'") {
        const close = html.indexOf(quote, value + 1);
        attribute.valueStart = value + 1;
        attribute.valueEnd = close === -1 ? html.length : close;
        attribute.tokenStart = value;
        attribute.tokenEnd = close === -1 ? html.length : close + 1;
        attribute.quote = quote;
        i = attribute.tokenEnd;
      } else {
        let valueEnd = value;
        while (valueEnd < html.length) {
          const next = charAt(html, valueEnd);
          if (isSpaceChar(next) || next === ">") break;
          valueEnd += 1;
        }
        attribute.valueStart = value;
        attribute.valueEnd = valueEnd;
        attribute.tokenStart = value;
        attribute.tokenEnd = valueEnd;
        i = valueEnd;
      }
    } else {
      // A valueless attribute (`<details open>`): its "value" is empty, and
      // the scan resumes at whatever stopped it, which the loop reads next.
      i = Math.max(i, equals);
    }
    attributes.push(attribute);
  }

  return { name, closing, selfClosing, start, end, attributes };
}

/**
 * A replacement value, re-quoted the way the value it replaces was quoted.
 *
 * `&` goes first, because every other escape writes one: a path that decoded
 * from `q&amp;a.md` to `q&a.md` has to come back out as `&amp;` or the route
 * this file wrote is one the browser will read as an entity.
 */
function quotedAttributeValue(value: string, quote: string): string {
  const escaped = value.replace(/&/g, "&amp;");
  if (quote === "'") return `'${escaped.replace(/'/g, "&#39;")}'`;
  return `"${escaped.replace(/"/g, "&quot;")}"`;
}

function spliceEdits(html: string, tag: Tag, edits: Array<[Attribute, string]>): string {
  const ordered = edits.slice().sort((a, b) => a[0].tokenStart - b[0].tokenStart);
  const parts: string[] = [];
  let at = tag.start;
  for (const [attribute, replacement] of ordered) {
    parts.push(html.slice(at, attribute.tokenStart));
    parts.push(quotedAttributeValue(replacement, attribute.quote));
    at = attribute.tokenEnd;
  }
  parts.push(html.slice(at, tag.end));
  return parts.join("");
}

/**
 * Walk the document tag by tag, ask `visit` about every attribute in
 * `LINK_ATTRIBUTES` outside a `pre`, `code`, `script` or `style`, and splice in
 * whatever it returns. A `visit` that returns `undefined`, or the value
 * unchanged, edits nothing.
 */
function walkLinkAttributes(
  html: string,
  visit: (link: LinkAttribute) => string | undefined,
): string {
  const pieces: string[] = [];
  let cursor = 0;
  let index = 0;
  let insideNoRewrite = 0;

  while (index < html.length) {
    const start = html.indexOf("<", index);
    if (start === -1) break;
    const tag = scanTag(html, start);
    if (tag.name !== "" && insideNoRewrite === 0) {
      const edits: Array<[Attribute, string]> = [];
      for (const attribute of tag.attributes) {
        if (!LINK_ATTRIBUTES.has(attribute.name)) continue;
        const value = html.slice(attribute.valueStart, attribute.valueEnd);
        const replacement = visit({ attribute: attribute.name, value });
        if (replacement !== undefined && replacement !== value) {
          edits.push([attribute, replacement]);
        }
      }
      if (edits.length > 0) {
        pieces.push(html.slice(cursor, tag.start));
        pieces.push(spliceEdits(html, tag, edits));
        cursor = tag.end;
      }
    }
    if (tag.name !== "" && NO_REWRITE_INSIDE.has(tag.name) && !tag.selfClosing) {
      if (tag.closing) {
        if (insideNoRewrite > 0) insideNoRewrite -= 1;
      } else {
        insideNoRewrite += 1;
      }
    }
    index = tag.end;
  }

  pieces.push(html.slice(cursor));
  return pieces.join("");
}
