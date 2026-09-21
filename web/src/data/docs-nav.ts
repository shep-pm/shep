/*
 * The docs book: seven parts, twenty-nine chapters, in reading order.
 *
 * This is the shape of the docs shell itself, and it is the only place a
 * chapter is named, numbered or ordered. DocsLayout derives each page's
 * crumb and <title> from it, DocsSidebar draws it, and ChapterBar reads
 * the neighbours off it, so moving a chapter is one edit here rather than
 * one edit per page.
 *
 * Parts are ordered by when a reader needs a thing rather than by what
 * kind of thing it is. That is the whole point of the arrangement: the
 * five groups this replaced were ordered the other way, and the group
 * called "Concepts" had collected eleven items with no order inside it.
 *
 * `built: false` marks a chapter that is planned and numbered but not yet
 * written. None are today: the two that were, Upgrading and Writing a dog,
 * landed in this same branch. Numbering a chapter before it is written is
 * what stops every chapter after it shifting on the day it lands, and
 * DocsSidebar draws one as inert text with a "soon" tag rather than as a
 * link to a 404.
 *
 * `source`/`spec`/`api` back the reference pills under each page's title
 * (see ReferencePills.astro), one shared component driven by this data so
 * a new page can't ship without at least a Source pill.
 */

/** A docs/specs/shep-v1.md section this page is drawn from. */
export interface SpecRef {
  /** GitHub's own heading-slug algorithm, e.g. "5-configuration" for "## 5. Configuration". */
  anchor: string;
  /** Short label, e.g. "§5 Configuration". */
  label: string;
}

/** A docs.rs type this page is genuinely about, only where shep-core's own API is the subject. */
export interface ApiRef {
  /** Path under docs.rs/shep-core/latest/shep_core/, e.g. "config/flockfile/struct.Flockfile.html". */
  path: string;
  /** Short label, e.g. "shep_core::config::Flockfile". */
  label: string;
}

export interface DocsNavItem {
  slug: string;
  /** Route is always `/docs/${slug}`. */
  label: string;
  /**
   * The ordinary word for the same thing, appended to the <title> in
   * brackets and used nowhere else.
   *
   * docs/terminology.md's vocabulary is deliberate and the sidebar, the
   * crumb and the prose all keep it. But nobody searching for a process
   * manager types "lookout" or "dogs", so thirty pages were titled in
   * words that only make sense once you already use shep. This is the
   * additive half: "The lookout (TUI dashboard)" keeps the shep term
   * first and gives a search engine something to match.
   *
   * Left off wherever the label is already plain English, which is about
   * half of them, and off `whistle` because its label already carries one.
   */
  plain?: string;
  /**
   * One line for /llms.txt and for the page's own meta description.
   *
   * Lives here rather than on the page for the same reason the label does:
   * one place names a chapter. verify-docs-nav.ts refuses an entry without
   * one, so a new page cannot ship missing from the index an agent reads.
   */
  summary: string;
  built: boolean;
  /**
   * Repo-relative path this page's material is drawn from: the Source pill.
   *
   * An array where a page's verbs genuinely span more than one module, which
   * renders one Source pill per file. The Logs page is the case that forced
   * it: `bleats` lives in `commands/bleats.rs` and `reopen`/`flush` in
   * `commands/logs.rs`, so either path alone under-represents the page.
   */
  source: string | string[];
  spec?: SpecRef;
  api?: ApiRef;
}

export interface DocsNavGroup {
  label: string;
  items: DocsNavItem[];
}

export const docsNav: DocsNavGroup[] = [
  {
    label: "Get it running",
    items: [
      {
        slug: "getting-started",
        label: "Quickstart",
        summary:
          "Install shep, write a two-field Flockfile, and start your first flock.",
        built: true,
        source: "README.md",
      },
      {
        slug: "from-pm2",
        label: "Coming from pm2",
        summary:
          "shep import pm2 reads a real dump.pm2 and writes a Flockfile. It starts nothing, and names on stderr everything that could not survive the trip unchanged.",
        built: true,
        source: "docs/migration.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "pm2-verbs",
        label: "pm2 verb reference",
        summary:
          "Every pm2 verb and the shep one to type instead, with a note wherever the behaviour differs rather than the spelling.",
        built: true,
        source: "docs/migration.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "startup",
        label: "Surviving a reboot",
        plain: "startup on boot",
        summary:
          "shep startup installs the init unit that brings the shepherd, and the flock it last saved, back after a reboot.",
        built: true,
        source: "docs/migration.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "examples",
        label: "Examples",
        summary:
          "Seven small Rust programs and a Flockfile that need only cargo and a Unix host, plus a second, polyglot layer over the top for Node, Bun, Python and Go, to watch shep's supervision behaviours happen instead of reading about them.",
        built: true,
        source: "examples/",
        spec: { anchor: "7-readiness--health", label: "§7 Readiness & health" },
      },
      {
        slug: "terminology",
        label: "The words",
        plain: "glossary",
        summary:
          "The full shep lexicon: what each themed word means, where you meet it, and whether it's built yet.",
        built: true,
        source: "docs/terminology.md",
      },
    ],
  },
  {
    label: "Day to day",
    items: [
      {
        slug: "logs",
        label: "Reading logs",
        summary:
          "Where a sheep's output lands, how to read it, and the two verbs that act on the files themselves: reopen for an external rotator, flush to empty them.",
        built: true,
        source: [
          "crates/shep-cli/src/commands/bleats.rs",
          "crates/shep-cli/src/commands/logs.rs",
        ],
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "overrides",
        label: "Changing a setting",
        plain: "config overrides",
        summary:
          "A Flockfile is a template your app's repository owns. What you tune on a running flock lives somewhere shep owns, and a load appends rather than overwrites.",
        built: true,
        source: [
          "crates/shep-core/src/overrides.rs",
          "crates/shep-core/src/config/apply.rs",
        ],
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
        api: {
          path: "overrides/struct.AppOverrides.html",
          label: "shep_core::overrides::AppOverrides",
        },
      },
      {
        slug: "lifecycle",
        label: "Stopping and replacing",
        plain: "stop, restart, reload, delete",
        summary:
          "What shep stop, restart, reload and delete each do to a running sheep and to the muster roll: one kill ladder shared by three of them, two different orders a reload can run, and the one difference between stop and delete that decides what comes back after a restart.",
        built: true,
        source: [
          "crates/shep-daemon/src/kill.rs",
          "crates/shep-daemon/src/supervisor.rs",
          "crates/shep-daemon/src/snapshot.rs",
        ],
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "talking-to-a-sheep",
        label: "Talking to a sheep",
        plain: "signals, stdin and IPC",
        summary:
          "Reaching an app that is already running: a unix signal, a line on its stdin, or a named action over the shepherd channel. None of the three restarts it, though a signal it does not handle can still kill it.",
        built: true,
        source: [
          "crates/shep-core/src/signals.rs",
          "crates/shep-cli/src/commands/signal.rs",
          "crates/shep-cli/src/commands/whisper.rs",
        ],
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
        api: {
          path: "signals/enum.OperatorSignal.html",
          label: "shep_core::signals::OperatorSignal",
        },
      },
      {
        slug: "upgrading",
        label: "Upgrading",
        summary:
          "cargo install replaces the binary on disk and changes nothing that is running. shep daemon reload is what moves a live flock onto it.",
        built: true,
        source: "crates/shep-cli/src/commands/daemon.rs",
      },
      {
        slug: "lookout",
        label: "The lookout",
        plain: "TUI dashboard",
        summary:
          "A terminal dashboard over the shepherd: the flock table, a host-usage strip, and a selected sheep's detail pane and bleats feed.",
        built: true,
        source: "docs/lookout/README.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "lookout-config",
        label: "Editing config in lookout",
        plain: "TUI config editor",
        summary:
          "The four panes lookout edits through: the shepherd's settings, secrets, a sheep's Flockfile fields, and a dog's own schema.",
        built: true,
        source: "crates/shep-cli/src/lookout/field.rs",
      },
      {
        slug: "output",
        label: "Terminal output",
        summary:
          "shep draws boxes, colour and sheep at a terminal, and plain columns everywhere else. One dial changes how much, and a pipe always gets the plain version.",
        built: true,
        source: "crates/shep-cli/src/style.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
    ],
  },
  {
    label: "Configuration",
    items: [
      {
        slug: "folds",
        label: "Folds",
        plain: "namespaces",
        summary:
          "A fold is a namespace: fold = in a Flockfile puts a sheep in one, and fold:<name> reaches it from any verb.",
        built: true,
        source: "crates/shep-core/src/config/app.rs",
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
      },
      {
        slug: "boot-order",
        label: "Boot order",
        summary:
          "depends_on names what a sheep waits for. shep sorts the flock into stages and starts them in order.",
        built: true,
        source: [
          "crates/shep-core/src/config/graph.rs",
          "crates/shep-daemon/src/boot_order.rs",
        ],
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
      },
      {
        slug: "secrets",
        label: "Secrets",
        summary:
          "A value per key per environment, read by a {{secret:NAME}} reference instead of written into the Flockfile itself.",
        built: true,
        source: [
          "crates/shep-core/src/secrets.rs",
          "crates/shep-cli/src/commands/secret.rs",
        ],
        api: {
          path: "secrets/struct.SecretView.html",
          label: "shep_core::secrets::SecretView",
        },
      },
      {
        slug: "kv",
        label: "The KV store",
        summary:
          "Three verbs, a file-locked JSON store, and no shepherd required: for the small stuff that has nowhere else to live.",
        built: true,
        source: "docs/kv.md",
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
      },
    ],
  },
  {
    label: "Dogs",
    items: [
      {
        slug: "dogs",
        label: "Dogs",
        plain: "plugins",
        summary:
          "The shepherd's own plugins: built-in metrics and alerting, and how to adopt a binary of your own.",
        built: true,
        source: "docs/dogs.md",
        spec: { anchor: "8-dogs-plugins", label: "§8 Dogs" },
      },
      {
        slug: "writing-a-dog",
        label: "Writing a dog",
        plain: "writing a plugin",
        summary:
          "A dog is an ordinary binary that answers --version and --schema on stdout and speaks the dog protocol on a socket the shepherd hands it.",
        built: true,
        source: "docs/dogs.md",
      },
      {
        slug: "community-dogs",
        label: "Community dogs",
        plain: "community plugins",
        summary:
          "Dogs other people wrote and adopted with shep adopt, listed for anyone who doesn't want to write their own.",
        built: true,
        source: "docs/dogs.md",
      },
    ],
  },
  {
    label: "Machine surfaces",
    items: [
      {
        slug: "whistle",
        label: "Whistle (MCP)",
        summary:
          "An MCP server over stdio that hands an AI agent the same flock a person reaches with shep flock.",
        built: true,
        source: "docs/whistle/README.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "json-output",
        label: "JSON output",
        summary:
          "Every shep command answers under --format json too, in a small versioned envelope built for piping rather than scraping.",
        built: true,
        source: "crates/shep-cli/src/output/mod.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
        api: {
          path: "protocol/request/struct.ProcessInfo.html",
          label: "shep_core::protocol::request::ProcessInfo",
        },
      },
      {
        slug: "shepherd-channel",
        label: "The shepherd channel",
        plain: "app IPC",
        summary:
          "A plain file descriptor carrying newline JSON: readiness, custom metrics, and answering shep trigger.",
        built: true,
        source: "docs/shepherd-channel.md",
        spec: { anchor: "7-readiness--health", label: "§7 Readiness & health" },
        api: {
          path: "protocol/channel/index.html",
          label: "shep_core::protocol::channel",
        },
      },
    ],
  },
  {
    label: "Other places it runs",
    items: [
      {
        slug: "containers",
        label: "Containers",
        plain: "Docker and PID 1",
        summary:
          "shep runtime is the PID-1 entrypoint for a container. shep dev is the same idea for a laptop: an isolated session that tidies up after itself.",
        built: true,
        source: "crates/shep-cli/src/commands/runtime.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "serve",
        label: "Serve",
        plain: "static file server",
        summary:
          "A static file server, hand-rolled and run as a managed sheep. Loopback by default, and every unsafe default is opt-in.",
        built: true,
        source: "crates/shep-cli/src/commands/serve.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
    ],
  },
  {
    label: "Reference",
    items: [
      {
        slug: "cli",
        label: "CLI reference",
        summary:
          "Every verb, its aliases, its flags, and its exit codes: generated from the same clap tree the binary parses with.",
        built: true,
        source: "crates/shep-cli/src/cli.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "first-flockfile",
        label: "Flockfile reference",
        plain: "config file format",
        summary:
          "Every field a Flockfile understands, the ten filenames config discovery searches, and the strict grammars that catch typos before they reach a running sheep.",
        built: true,
        source: "crates/shep-core/src/config/flockfile.rs",
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
        api: {
          // The canonical page, not the `config` re-export: rustdoc gives a
          // re-exported item no page of its own, so config/struct.Flockfile.html
          // is a 404. The label stays the path a caller actually writes.
          path: "config/flockfile/struct.Flockfile.html",
          label: "shep_core::config::Flockfile",
        },
      },
      {
        slug: "not-built",
        label: "What's not built",
        summary:
          "What the Windows tier does not do, plus a short list of deliberate cuts and open design questions: sourced from docs/specs/deferred.md.",
        built: true,
        source: "docs/specs/deferred.md",
        spec: { anchor: "2-versioned-scope", label: "§2 Versioned scope" },
      },
    ],
  },
];

/**
 * Whether each pill *kind* has anywhere real to send a reader yet. Both
 * started false: the repo was private (a GitHub link 404s for anyone
 * without access) and no crate had published (docs.rs had nothing to show).
 * A pill of a dead kind still renders, with the real, final URL already in
 * its href, dimmed and inert instead of clickable, rather than either
 * shipping a confident-looking link that 404s or hiding the sourcing
 * entirely.
 *
 * Nothing here checks the network, so a flag is a claim someone has to
 * verify by hand. Both were checked with `curl` on 2026-09-14: every
 * `api.path` in this file answers 200 under
 * docs.rs/shep-core/latest/shep_core/.
 */
export const pillTargetsLive = {
  // The repository went public on 2026-08-16, so every Source and Spec pill
  // resolves. shep-core published on crates.io and docs.rs built it, so the
  // API pills resolve too: they were dimmed for a publish that had already
  // happened.
  github: true,
  docsRs: true,
};

/** One chapter, flattened out of its group and numbered from 1. */
export interface DocsChapter {
  /** 1-based position across the whole book, unbuilt chapters included. */
  number: number;
  /** The part this chapter sits in, e.g. "Get it running". */
  part: string;
  item: DocsNavItem;
}

/**
 * Every chapter in reading order, numbered.
 *
 * Unbuilt chapters are numbered alongside the rest on purpose: a number
 * that shifts when a page lands would invalidate every "chapter 12" a
 * reader has already seen.
 */
export const chapters: DocsChapter[] = docsNav.flatMap((group) =>
  group.items.map((item) => ({ number: 0, part: group.label, item })),
).map((chapter, index) => ({ ...chapter, number: index + 1 }));

/**
 * A chapter and its neighbours, for the crumb, the title and the next bar.
 *
 * `previous` and `next` skip unbuilt chapters, because a bar offering a
 * page that does not exist is worse than one that skips a number.
 *
 * Returns `undefined` for a slug that is not in the nav at all, which is a
 * page that exists but was never filed. `DocsLayout` turns that into a
 * build failure rather than a silently blank crumb.
 */
export function chapterFor(slug: string):
  | { chapter: DocsChapter; previous?: DocsChapter; next?: DocsChapter }
  | undefined {
  const index = chapters.findIndex((c) => c.item.slug === slug);
  if (index === -1) return undefined;
  const built = (c: DocsChapter) => c.item.built;
  return {
    chapter: chapters[index],
    previous: chapters.slice(0, index).findLast(built),
    next: chapters.slice(index + 1).find(built),
  };
}
