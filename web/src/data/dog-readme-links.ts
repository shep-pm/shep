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
 * - a relative image is fetched at the tag from `raw.githubusercontent.com`;
 * - any other relative file points at the repo at that tag, on the page
 *   GitHub renders for it.
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

/** The only two attributes this file rewrites. */
const LINK_ATTRIBUTES = new Set(["href", "src"]);

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

/**
 * Extensions treated as an image, and so fetched rather than linked to a
 * GitHub page. Anything outside this set is a file, and gets the blob URL.
 */
const IMAGE_EXTENSIONS = new Set([
  "avif",
  "bmp",
  "gif",
  "ico",
  "jpeg",
  "jpg",
  "png",
  "svg",
  "tif",
  "tiff",
  "webp",
]);

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

function isImagePath(path: string): boolean {
  const dot = path.lastIndexOf(".");
  if (dot === -1) return false;
  return IMAGE_EXTENSIONS.has(path.slice(dot + 1).toLowerCase());
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
  const raw = value.trim();
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

function rewriteTarget(value: string, page: DogPageAddress): string | undefined {
  const link = relativeLink(value);
  if (link === undefined) return undefined;
  const path = resolveRepoPath(link.target, page.fromPath);
  if (path === "") return dogDocRoute(page.adoptAs, "") + link.fragment;
  if (hasMdExtension(path)) return dogDocRoute(page.adoptAs, path) + link.fragment;
  const at = rawFileUrl(page.owner, page.repo, page.tag, path);
  if (isImagePath(path)) return at + link.fragment;
  return blobFileUrl(page.owner, page.repo, page.tag, path) + link.fragment;
}

/** One `href` or `src` the walker reports, and what the visitor returns for it. */
interface LinkAttribute {
  /** Which of the two it was. */
  attribute: string;
  value: string;
}

/**
 * Rewrite every relative link in rendered HTML.
 *
 * Text is never touched, only the `href` and `src` of tags, and never inside
 * `pre`, `code`, `script` or `style`. Everything else in the document is
 * copied out byte for byte, including comments and attributes this file does
 * not rewrite.
 *
 * @throws {Error} when a relative link climbs above the repository root.
 */
export function rewriteLinks(html: string, page: DogPageAddress): string {
  return walkLinkAttributes(html, ({ value }) => rewriteTarget(value, page));
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

/** A replacement value, re-quoted the way the value it replaces was quoted. */
function quotedAttributeValue(value: string, quote: string): string {
  if (quote === "'") return `'${value.replace(/'/g, "&#39;")}'`;
  return `"${value.replace(/"/g, "&quot;")}"`;
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
 * Walk the document tag by tag, ask `visit` about every `href` and `src`
 * outside a `pre`, `code`, `script` or `style`, and splice in whatever it
 * returns. A `visit` that returns `undefined`, or the value unchanged, edits
 * nothing.
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
