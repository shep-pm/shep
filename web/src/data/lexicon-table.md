# Lexicon table

<!--
  The Terminology page's table, parsed by web/src/data/docs-lexicon.ts.

  It lived in README.md until that page was rewritten as a landing page
  rather than a reference, which left the parser reading a heading that was
  no longer there and the site build failing outright. It is a document
  rather than a data file so the shape stays the one the parser already
  knew: the lexicon heading below, then a four-column table. Do not repeat
  that heading's text up here, in this comment or any other: the parser
  finds the first occurrence in the file and would read this instead.

  docs/terminology.md is the design lexicon and a different table: it is
  keyed on the conventional word, carries the rulings behind each choice,
  and has no "built" column. This one is the operator's.
-->

## The lexicon

The whole vocabulary, and whether it exists yet.

| shep says | Means | Where you meet it | Built? |
|---|---|---|---|
| the shepherd | the daemon | log messages, docs | yes |
| the flock | every managed process, as a set | `shep flock` (aliases `list`, `ls`) | yes |
| a sheep | one managed process (singular only) | `shep describe <name>` | yes |
| a fold | a namespace or group | `shep fold backend`, `fold =` in config | yes |
| Flockfile | the app config file | `Flockfile.toml` / `.yaml` / `.json` / `.json5` | yes |
| bleats | logs | `shep bleats` (alias `logs`) | yes |
| muster | bring a saved flock back | `shep muster` | yes |
| the shepherd channel | a private pipe on fd 3 between daemon and app | `channel = true`, `shep trigger` | yes |
| a lamb | a child process of a sheep | tree-kill, `describe`'s tree view | yes |
| a dog | a plugin process the shepherd supervises | `shep enable metrics`, `shep dogs` | yes |
| a bark | a webhook alert | `[bark.sinks]` config in `dogs.toml`, `shep barks` | yes |
| a smit | a short mark a dog paints on a sheep | the SMIT column in `shep flock` and the lookout | yes |
| the whistle | the MCP interface agents talk to | `shep whistle` | yes |
| the lookout | the terminal dashboard | `shep lookout` (alias `dash`) | partly |
| adopt / delete | register a third-party dog, or drop one for good | `shep adopt <path> [--name <name>]`, `shep delete <name>` | yes |
| that'll do | graceful stop, after the real herding command | `shep thatlldo` | yes |
| stock | change how many instances of an app run (the stocking rate) | `shep stock <name> <count>` (alias `scale`) | yes |
| signal | send a signal to one sheep's own process | `shep signal <selector> <signal>` | yes |
| whisper | write a line to a sheep's stdin | `shep whisper <selector> <line>` (alias `sendline`) | yes |
| attach | show a pty sheep's live terminal and type into it | `shep attach <name>` (detach with ctrl-]) | yes |
| kv store | the flat key-value junk drawer | `shep kv get`, `shep kv set`, `shep kv unset` | yes |
| get | read one sheep's settings, or one dog's own section, as TOML | `shep get <name>[.<path>]` | yes |
| set | change one setting of a sheep or a dog | `shep set <name>[.<path>] <value>` | yes |
| unset | drop a sheep field's override, or remove a key from a dog's settings | `shep unset <name>[.<path>]` | yes |
| copy | move one setting, one dog's table for a sheep, or a sheep's env, from one sheep onto another | `shep copy <from>.<path> <to>.<path>` | yes |

Sheepdogs and sheep were separate ideas from the start, so "dog" never means
the daemon. The shepherd is the shepherd. Dogs are plugins that work for it.
