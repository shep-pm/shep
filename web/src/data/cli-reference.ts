/*
 * CLI reference page data — every verb's usage line, about text and full
 * `--help` output, parsed from a real run of the binary rather than
 * hand-typed. Same shape as web/src/data/docs-lexicon.ts (parse a checked-in
 * generated text file into structured rows at Astro build time), and same
 * reason: a hand-written CLI reference drifts from crates/shep-cli/src/cli.rs
 * the first time a flag changes and nobody remembers to update prose too.
 *
 * Source of truth: web/src/data/cli-reference.generated.txt, produced by
 * web/scripts/generate-cli-reference.sh running `shep --help` against
 * target/release/shep once per verb, and once more per subcommand of a verb
 * that hosts them. Re-run that script after any change to the verb list, its
 * aliases, or any verb's flags — see the script's own header for the exact
 * command.
 */
// `?raw` (see web/src/data/lexicon.ts's header comment) inlines the file's
// text content at build time.
import generatedSource from "./cli-reference.generated.txt?raw";
import { htmlEscape } from "./html-escape";

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
  /** `shep help flags`, byte-for-byte: each global flag once, with its long text. */
  globalFlagsHelp: string;
  /** Each other `shep help <topic>`, byte-for-byte, in the generator's order. */
  helpTopics: { name: string; text: string }[];
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
  throw new Error(`web/src/data/cli-reference.ts: ${message}`);
}

/** Escapes HTML, then applies `code` and **bold** inline spans. */
function inlineToHtml(text: string): string {
  const escaped = htmlEscape(text);
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
  // shep's top-level --help lists each verb on its own line, its aliases
  // after a comma in the first column, then its about:
  //
  //   flock, list, ls    List the flock
  //
  // Pinned on the Rust side by `aliases_sit_on_their_verbs_line`, which
  // derives the expected content from clap itself, so this parser and the
  // CLI cannot drift apart without that test failing first. A line whose
  // first name is no verb (an example, a topic, an adopted dog) is skipped.
  const nameSet = new Set<string>(names);
  const result = new Map<string, string[]>();

  for (const line of topLevelHelp.split("\n")) {
    if (!line.startsWith("  ") || line.startsWith("   ")) {
      continue;
    }
    const label = line.trim().split(/\s{2,}/)[0];
    const [verb, ...aliases] = label.split(",").map((n) => n.trim());
    if (!nameSet.has(verb) || aliases.length === 0 || result.has(verb)) {
      continue;
    }
    result.set(verb, aliases);
  }

  if (result.size === 0) {
    fail("top-level --help has no verb line with aliases to read them from.");
  }
  return result;
}

// The generator emits nothing ahead of it, so this marker opens the file
// rather than being something to search for. There is deliberately no
// version section any more — see the generator's own comment for why, and
// web/src/data/workspace-version.ts for where the page reads the version now.
const TOP_LEVEL_MARKER = "@@TOPLEVEL@@\n";

// `shep help flags` follows the top-level help. No verb's page carries the
// global flags' long text, so this is the one place it is published.
const FLAGS_MARKER = "\n@@FLAGS@@\n";

// The other `shep help <topic>` pages follow the flags, one marker each.
const TOPIC_MARKER = /\n@@TOPIC:([a-z-]+)@@\n/;

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
  const flagsAt = topLevelSection.indexOf(FLAGS_MARKER);
  if (flagsAt === -1) {
    fail("has no @@FLAGS@@ section — re-run generate-cli-reference.sh.");
  }
  const topLevelHelp = topLevelSection.slice(TOP_LEVEL_MARKER.length, flagsAt).trim();
  // Splitting on the one-capture pattern gives [flags, name, text, name, ...].
  const [globalFlagsText, ...topicParts] = topLevelSection
    .slice(flagsAt + FLAGS_MARKER.length)
    .split(new RegExp(TOPIC_MARKER, "g"));
  const globalFlagsHelp = globalFlagsText.trim();
  const helpTopics: { name: string; text: string }[] = [];
  for (let i = 0; i < topicParts.length; i += 2) {
    helpTopics.push({ name: topicParts[i], text: topicParts[i + 1].trim() });
  }

  const verbs: CliVerb[] = [];
  for (let i = 0; i < sections.length; i += 2) {
    verbs.push(parseVerbBlock(sections[i], sections[i + 1]));
  }

  const aliasesByVerb = parseAliasesFromTopLevel(
    topLevelHelp,
    verbs.map((v) => v.name),
  );
  for (const verb of verbs) {
    verb.aliases = aliasesByVerb.get(verb.name) ?? [];
  }

  return { topLevelHelp, globalFlagsHelp, helpTopics, verbs };
}

export const cliReference: CliReferenceData = parse(generatedSource);
