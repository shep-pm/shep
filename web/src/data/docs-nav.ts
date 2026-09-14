/*
 * The docs book: seven parts, twenty-seven chapters, in reading order.
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
 * written. Two are, deliberately: numbering them now is what stops every
 * chapter after them shifting on the day they land, and DocsSidebar draws
 * them as inert text with a "soon" tag rather than as links to a 404.
 *
 * `source`/`spec`/`api` back the reference pills under each page's title
 * (see ReferencePills.astro) — one shared component driven by this data so
 * a new page can't ship without at least a Source pill.
 */

/** A docs/specs/shep-v1.md section this page is drawn from. */
export interface SpecRef {
  /** GitHub's own heading-slug algorithm, e.g. "5-configuration" for "## 5. Configuration". */
  anchor: string;
  /** Short label, e.g. "§5 Configuration". */
  label: string;
}

/** A docs.rs type this page is genuinely about — only where shep-core's own API is the subject. */
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
  built: boolean;
  /**
   * Repo-relative path this page's material is drawn from — the Source pill.
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
        built: true,
        source: "README.md",
      },
      {
        slug: "from-pm2",
        label: "Coming from pm2",
        built: true,
        source: "docs/migration.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "startup",
        label: "Surviving a reboot",
        built: true,
        source: "docs/migration.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "examples",
        label: "Examples",
        built: true,
        source: "examples/",
        spec: { anchor: "7-readiness--health", label: "§7 Readiness & health" },
      },
      { slug: "terminology", label: "The words", built: true, source: "docs/terminology.md" },
    ],
  },
  {
    label: "Day to day",
    items: [
      {
        slug: "logs",
        label: "Reading logs",
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
        built: true,
        source: "crates/shep-cli/src/commands/daemon.rs",
      },
      {
        slug: "lookout",
        label: "The lookout",
        built: true,
        source: "docs/lookout/README.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "output",
        label: "Terminal output",
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
        built: true,
        source: "crates/shep-core/src/config/app.rs",
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
      },
      {
        slug: "boot-order",
        label: "Boot order",
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
        built: true,
        source: "docs/dogs.md",
        spec: { anchor: "8-dogs-plugins", label: "§8 Dogs" },
      },
      {
        slug: "writing-a-dog",
        label: "Writing a dog",
        built: true,
        source: "docs/dogs.md",
      },
      {
        slug: "community-dogs",
        label: "Community dogs",
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
        built: true,
        source: "docs/whistle/README.md",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "json-output",
        label: "JSON output",
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
        built: true,
        source: "crates/shep-cli/src/commands/runtime.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "serve",
        label: "Serve",
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
        built: true,
        source: "crates/shep-cli/src/cli.rs",
        spec: { anchor: "9-cli-surface-sheep-native", label: "§9 CLI surface" },
      },
      {
        slug: "first-flockfile",
        label: "Flockfile reference",
        built: true,
        source: "crates/shep-core/src/config/flockfile.rs",
        spec: { anchor: "5-configuration", label: "§5 Configuration" },
        api: {
          path: "config/struct.Flockfile.html",
          label: "shep_core::config::Flockfile",
        },
      },
      {
        slug: "not-built",
        label: "What's not built",
        built: true,
        source: "docs/specs/deferred.md",
        spec: { anchor: "2-versioned-scope", label: "§2 Versioned scope" },
      },
    ],
  },
];

/**
 * Whether each pill *kind* has anywhere real to send a reader yet. Both
 * start false: the repo is private (a GitHub link 404s for anyone without
 * access) and no crate has published (docs.rs has nothing to show). The
 * pills still render — with the real, final URL already in their href —
 * dimmed and inert instead of clickable, rather than either shipping a
 * confident-looking link that 404s or hiding the sourcing entirely.
 *
 * Flip one flag the day it stops being true and every pill of that kind
 * goes live with no other code change: the repo going public is one
 * boolean, shep-core's first docs.rs publish is the other.
 */
export const pillTargetsLive = {
  // The repository went public on 2026-08-16, so every Source and Spec pill
  // resolves. docs.rs stays gated until the first `cargo publish`: the crate
  // has no page there yet, and a pill that looks authoritative and 404s is
  // worse than one that says why it is waiting.
  github: true,
  docsRs: false,
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
