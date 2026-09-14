/*
 * Where the project lives, in one place.
 *
 * The URL was written out in five components and two pages before this. It
 * is not going to change often, which is exactly why five copies survived
 * unnoticed: nothing ever forced them to disagree, and nothing would have
 * told anyone if a sixth had.
 *
 * One literal is deliberately left alone. getting-started.astro's terminal
 * block shows `git clone https://github.com/shep-pm/shep.git`, which a
 * reader selects and pastes. A command a reader copies should read as the
 * command they will run, not as an interpolation.
 */

/** The repository root. */
export const REPO = "https://github.com/shep-pm/shep";

/** The "file an issue" form, offered at the foot of every docs page. */
export const REPO_ISSUES_NEW = `${REPO}/issues/new`;

/** A file on the default branch, e.g. repoFile("docs/whistle/tools.md"). */
export function repoFile(path: string): string {
  return `${REPO}/blob/main/${path}`;
}
