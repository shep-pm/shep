/*
 * One slug function, for anything that becomes a URL fragment.
 *
 * It existed as `anchorOf` in dogs.ts, for dog names, and was about to be
 * written twice more for headings that are rendered in a loop and so cannot
 * carry a hand-written id. Slugging is lossy, which is exactly why it wants
 * one definition: two callers that disagree by a character produce two
 * anchors for the same heading and neither of them resolves.
 */

/** Lowercase, non-alphanumerics to single hyphens, no leading or trailing hyphen. */
export function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
