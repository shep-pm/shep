<!--
Two headings, both wanted. The short summary is for whoever reviews this. The
precise details are for the coding agents and scripts that also read pull
requests, and they are the half people skip.

Delete a bullet that genuinely does not apply. Do not delete a bullet because
the answer is awkward: "not tested" and "no gate run" are useful answers.
-->

## Short summary

<!-- What changed and why, in a few sentences or a short list. Link the issue
it closes: "Closes #N". -->

## Precise details

- Repo/branch: <!-- fork/branch, the commits, and the base you cut from -->
- Files touched: <!-- paths, and roughly +N/-N -->
- Crates touched: <!-- each crate, and whether its public API changed -->
- New or changed surface: <!-- verb, flag, Flockfile key, JSON field, exit code,
  default; or none -->
- Docs updated: <!-- which pages, or why none needed it -->
- Gate run locally: <!-- the commands from CONTRIBUTING.md and what they said.
  For web/ that is `npm ci`, `npx astro check`, `npm run build`, in that order. -->
- Not tested: <!-- platforms, paths and cases you did not exercise. Naming one
  costs nothing and saves a reviewer guessing. -->
- No ported source from pm2 or any other process manager. <!-- shep is
  clean-room. If you read another project's source, say so here. -->
- Written with AI assistance: <!-- yes and which tool, or delete this line.
  Either answer is fine; an unstated one is not. -->

## Before you ask for review

- [ ] Every commit subject is a conventional one, `type(scope): summary`. release-plz reads individual commits and silently drops what it cannot parse, so an unreadable subject contributes nothing to the changelog and nothing to the version bump. Accepted types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`.
- [ ] Anything that breaks a caller carries a `!`, on the commit that breaks it, in the crate that breaks. A `!` on this title is read by nobody.
- [ ] If this changes anything an operator types or sees, `web/` says so too. Regenerate with `./web/scripts/generate-cli-reference.sh`, then read the hand-written pages under `web/src/pages/docs/`.
- [ ] I can explain every line of this. Using an AI to write it is fine and common here. Opening a pull request you cannot answer questions about is not, whoever or whatever wrote it.
