/*
 * CLI reference page data — every verb's usage line, about text and full
 * `--help` output, parsed from a real run of the binary rather than
 * hand-typed. Same shape as web/src/data/docsLexicon.ts (parse a checked-in
 * generated text file into structured rows at Astro build time), and same
 * reason: a hand-written CLI reference drifts from crates/shep-cli/src/cli.rs
 * the first time a flag changes and nobody remembers to update prose too.
 *
 * Source of truth: web/src/data/cli-reference.generated.txt, produced by
 * web/scripts/generate-cli-reference.sh running `shep --help` and
 * `shep <verb> --help` for every verb against target/release/shep. Re-run
 * that script after any change to the verb list, its aliases, or any verb's
 * flags — see the script's own header for the exact command.
 */
// `?raw` (see web/src/data/lexicon.ts's header comment) inlines the file's
// text content at build time.
import generatedSource from "./cli-reference.generated.txt?raw";

export interface CliVerb {
  name: string;
  /** Visible aliases only — a verb with none is `[]`. */
  aliases: string[];
  /** About text, unwrapped into flowing paragraphs, HTML-escaped with `code`/`strong` spans applied. */
  aboutHtml: string[];
  /** e.g. "shep start [OPTIONS] <TARGET>" — the "Usage: " prefix stripped. */
  usage: string;
  /** This verb's own `--help` output, byte-for-byte as clap rendered it. */
  helpText: string;
}

export interface CliReferenceData {
  /** `shep --help`, byte-for-byte, for the page's own top-level block. */
  topLevelHelp: string;
  verbs: CliVerb[];
}

// Splits the generated file into its `@@VERB:<name>@@` blocks, in the order
// the generator wrote them: the declaration order of the Commands enum, and
// the order `shep --help` itself lists them. A quoted entry in the
// generator's `VERBS` array is a path into the command tree, so a name here
// can be two words. "secret set" is its own block, with its own flags.
//
// The verb list is derived from these markers rather than kept here as a
// second copy of that array. The copy agreed with the generator only by
// hand, and for months it did not: short by three from whenever `init`,
// `style` and `welcome` shipped until 2026-09-03, caught by nothing,
// because this list is what the page renders FROM. Two Rust tests hold the
// chain now. `every_visible_verb_reaches_the_docs_site_generator` walks the
// binary's command tree against the generator's array, and
// `every_listed_verb_has_a_block_in_the_committed_reference` fails when that
// array names something the generated text below does not carry.
const VERB_MARKER = /^@@VERB:(.+)@@$/m;

function fail(message: string): never {
  throw new Error(`web/src/data/cliReference.ts: ${message}`);
}

/** Escapes HTML, then applies `code` and **bold** inline spans. */
function inlineToHtml(text: string): string {
  const escaped = text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  return escaped
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/`([^`]+)`/g, "<code>$1</code>");
}

/**
 * clap hard-wraps prose to a fixed column width when stdout isn't a tty.
 * Un-wraps it back into flowing paragraphs: blank lines split paragraphs,
 * single line breaks within a paragraph are just wrap points and get
 * joined with a space.
 */
function unwrapParagraphs(block: string): string[] {
  return block
    .split(/\n\s*\n/)
    .map((para) =>
      para
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean)
        .join(" "),
    )
    .filter(Boolean);
}

function parseVerbBlock(name: string, block: string): CliVerb {
  const usageIndex = block.indexOf("\nUsage: ");
  if (usageIndex === -1) {
    fail(`verb "${name}" has no "Usage: " line — cli-reference.generated.txt may be stale or truncated.`);
  }
  const aboutBlock = block.slice(0, usageIndex).trim();
  const rest = block.slice(usageIndex + 1).trim();
  const usageLine = rest.split("\n", 1)[0];
  const usage = usageLine.replace(/^Usage:\s*/, "");

  return {
    name,
    aliases: [],
    aboutHtml: unwrapParagraphs(aboutBlock).map(inlineToHtml),
    usage,
    helpText: block.trim(),
  };
}

function parseAliasesFromTopLevel(topLevelHelp: string, names: readonly string[]): Map<string, string[]> {
  // shep's top-level --help uses a hand-written template with grouped verb
  // lines, not clap's generated `Commands:` block, so there is no per-verb
  // description line carrying `[aliases: ...]` to read. The template names
  // them on a single line instead:
  //
  //   Aliases          flock: list, ls   bleats: logs   stock: scale
  //
  // Pinned on the Rust side by `the_help_template_names_every_visible_alias`,
  // which derives the expected content from clap itself, so this parser and
  // the CLI cannot drift apart without that test failing first.
  const line = topLevelHelp
    .split("\n")
    .find((l) => l.startsWith("Aliases"));
  if (line === undefined) {
    fail("top-level --help has no Aliases line to read aliases from.");
  }

  const nameSet = new Set<string>(names);
  const result = new Map<string, string[]>();

  // `verb: a, b` groups, separated by runs of whitespace. Splitting on the
  // colon rather than on whitespace keeps multi-alias lists together.
  const body = line.replace(/^Aliases\s*/, "");
  const parts = body.split(/\s{2,}/).filter((p) => p.trim().length > 0);
  for (const part of parts) {
    const colon = part.indexOf(":");
    if (colon === -1) {
      continue;
    }
    const verb = part.slice(0, colon).trim();
    if (!nameSet.has(verb)) {
      fail(`top-level --help names aliases for "${verb}", which is not a known verb.`);
    }
    const aliases = part
      .slice(colon + 1)
      .split(",")
      .map((a) => a.trim())
      .filter((a) => a.length > 0);
    result.set(verb, aliases);
  }

  if (result.size === 0) {
    fail("top-level --help has an Aliases line but no verb: alias entries in it.");
  }
  return result;
}

// The generator emits nothing ahead of it, so this marker opens the file
// rather than being something to search for. There is deliberately no
// version section any more — see the generator's own comment for why, and
// web/src/data/workspaceVersion.ts for where the page reads the version now.
const TOP_LEVEL_MARKER = "@@TOPLEVEL@@\n";

function parse(source: string): CliReferenceData {
  if (!source.startsWith(TOP_LEVEL_MARKER)) {
    fail("does not open with the @@TOPLEVEL@@ marker — re-run generate-cli-reference.sh.");
  }

  // Splitting on a pattern with one capture group interleaves the captures
  // with the text between them, so this is [before, name, block, name, ...].
  const [topLevelSection, ...sections] = source.split(VERB_MARKER);
  if (sections.length === 0) {
    fail("has no @@VERB:...@@ markers — re-run generate-cli-reference.sh.");
  }
  const topLevelHelp = topLevelSection.slice(TOP_LEVEL_MARKER.length).trim();

  const verbs: CliVerb[] = [];
  for (let i = 0; i < sections.length; i += 2) {
    verbs.push(parseVerbBlock(sections[i], sections[i + 1] ?? ""));
  }

  const aliasesByVerb = parseAliasesFromTopLevel(
    topLevelHelp,
    verbs.map((v) => v.name),
  );
  for (const verb of verbs) {
    verb.aliases = aliasesByVerb.get(verb.name) ?? [];
  }

  return { topLevelHelp, verbs };
}

export const cliReference: CliReferenceData = parse(generatedSource);
