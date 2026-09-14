/*
 * /llms.txt, generated from docs-nav.ts.
 *
 * A map of the docs for an agent asked about shep, so it fetches the one
 * chapter it needs rather than crawling the whole book. The format is the
 * llms.txt convention: an H1, a blockquote summary, then links grouped
 * under headings, one line each.
 *
 * Generated rather than written, so a new chapter cannot be missing from
 * it: docs-nav.ts is the only place a chapter is named, and
 * verify-docs-nav.ts refuses an entry with no summary.
 *
 * Per-page .md is deliberately not served. These pages are hand-written
 * .astro with components, so markdown would mean either converting all of
 * them or maintaining a second copy that drifts.
 */
import type { APIRoute } from "astro";
import { docsNav } from "../data/docs-nav";

const SITE = "https://shep-pm.com";

export const GET: APIRoute = () => {
  const lines: string[] = [
    "# shep",
    "",
    "> A process manager written in Rust. One binary runs a daemon called the",
    "> shepherd, which keeps a flock of long-running processes alive, captures",
    "> what they print, and says plainly when something is wrong.",
    "",
  ];

  for (const group of docsNav) {
    lines.push(`## ${group.label}`, "");
    for (const item of group.items) {
      if (!item.built) continue;
      lines.push(`- [${item.label}](${SITE}/docs/${item.slug}): ${item.summary}`);
    }
    lines.push("");
  }

  return new Response(lines.join("\n"), {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
};
