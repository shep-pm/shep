# shep — name & terminology

Approved by the maintainer 2026-08-07. **shep** = the project, the binary, the brand. A sheepdog
watching your processes. Playful shepherd/sheep/sheepdog terminology runs through the
CLI, docs, and type names.

Crates: `shep-core`, `shep-daemon`, `shep-client`, `shep`. Binary: `shep`.
Free on crates.io (verified 2026-08-07): `shep`, `shepd`, plus reserves `bleat`,
`sheepdog`, `fleece`, `crook` if satellite crates ever want them.

## The lexicon

| Concept | Conventional | shep says | Where it applies |
|---|---|---|---|
| the daemon | daemon/supervisor | **the shepherd** — ONLY the shepherd; "dog" now means plugins (collision removed 2026-08-07) | docs, log messages, TUI header |
| managed processes (plural/list) | process list | **the flock** — ALWAYS the plural term; never bare plural "sheep" in docs/CLI (kills sg/pl ambiguity; ruled 2026-08-07) | `shep flock` (list), `Flock` type, docs |
| one managed process (singular) | process/app | **a sheep** (singular ONLY) / process (precise) | docs may say sheep; API types stay `Process`-clear. RESERVED for managed processes |
| plugin process (first-party: published by shep-pm, built into the binary or from its own `shep-pm` repo; or third-party, speaking the client protocol) | plugin/module | **dog** (pl. dogs) | `shep enable metrics` (built-in); `shep adopt <name> <path>` (third-party — `enable --exec` is a hidden pm2-spelled alias); `shep dogs` (list), hidden `shep dog <name>` runs one; `dog`-tagged in flock listing. Decided 2026-08-07 |
| child process of a sheep (process-tree member) | child process | **lamb** (pl. lambs) | `shep describe` tree view ("the sheep and her lambs"), tree-kill docs. Decided 2026-08-07 |
| one of an app's N running copies (`instances` > 1) | instance/worker | **instance**, plain, not themed, and not a lamb. A lamb is a child process of one sheep; an instance is a sibling copy of a sheep, spawned by the daemon rather than forked by its parent. `web:2` selects instance 2 of `web`; `ProcessInfo.instance` carries the slot on the wire | `shep flock`'s `web ×3` group row, `name:slot` selector, `SHEP_INSTANCE`/`{{instance}}` |
| namespace / group | namespace | **fold** (also: paddock) | `shep fold <name>`, `Fold` type |
| app config file | ecosystem.config.js | **Flockfile** (`Flockfile.toml` / `.yaml` / `.json`) | config discovery, docs |
| logs | logs | **bleats** | `shep bleats [--follow]`; `shep logs` stays as alias |
| webhook alert | alert/notification | **bark** 🐕 | `[bark]` config section, `shep barks` history, alert module |
| MCP agentic interface | MCP server | **the whistle** | `shep whistle` (serves MCP), docs metaphor: agents whistle, the shepherd and dogs respond |
| graceful shutdown | stop | **`shep thatlldo [target]`** | easter-egg alias for graceful stop — real herding command for "work's done" |
| freeze the running list | save | **save** | `shep save` writes the muster roll |
| register a third-party plugin | install | **adopt** | `shep adopt <name> <path>`; `enable --exec` is a hidden alias |
| drop a third-party plugin for good | uninstall | **delete** | `shep delete <name>` — unlike `disable`, it forgets the registration; the hidden `shep rehome <name>` runs the same removal |
| refuse a sheep or turn a dog on or off | disable/enable | **disable** / **enable**, general verbs for a sheep or a dog | `shep disable api` stops a sheep and refuses to start it until `shep enable api`; `shep enable metrics` turns a dog on. Straight verbs, not themed |
| resurrect saved state | resurrect | **muster** | `shep muster` assembles the flock from the roll |
| TUI dashboard | monit/dash | **lookout** | `shep lookout`; `shep dash` alias |
| host machine | host | **the heft** (sheep bound to their hill) | subtle: docs + host-metrics naming |
| another machine whose shepherd the CLI talks to, reached over ssh | remote | **host** (pl. hosts), plain, not themed: it appears in flags, config and error text (rule 2). Each host's shepherd owns its own flock; no shepherd supervises a sheep on another host. **heft** stays the docs word for one machine's ground | `--host <name>`, `hosts = [...]` in `shep.toml`, `shep host add/remove` |
| every host's flock, listed together | fleet view | **the flocks**, the plural of flock, in prose only; not a type or a verb | `shep flock` on a machine with a hosts list, docs |
| graceful reload (an overlap, not zero downtime) | reload | reload (verb stays) — strategies **come-bye** / **away** if we ever name them | reload internals, maybe strategy flags |
| kill escalation | kill | kill (stays — clarity beats cuteness on destructive ops) | — |
| change instance count | scale | **stock** — "stocking rate" is the real husbandry term for how many animals a piece of land runs, not a pun; `shep scale` stays as an alias | `shep stock <name> <count>` |
| send a signal to one process | signal | **signal** — stays plain, deliberately: `shep signal web SIGKILL` is a loaded gun, and rule 2 below keeps destructive/precise operations free of whimsy, the same reason `kill` stayed `kill` | `shep signal <selector> <signal>` |
| write to a process's stdin | sendline | **whisper** — completes the pair `bleats` already started: bleats is what the sheep says to you, whisper is what you say to the sheep, down a channel nobody else hears; `shep sendline` stays as an alias | `shep whisper <selector> <line>` |
| ad-hoc key/value store | kv store | **kv** (plain, not themed), hosting **get** / **set** / **unset** under it | `shep kv get`, `shep kv set`, `shep kv unset`; `shep get`, `shep set` and `shep unset` are the settings read, write and undo below |
| read any settings in the flock, as TOML | none | **get** (plain, not themed), the reader of the same address grammar `shep copy` writes through | `shep get <name>[.<path>]`, such as `shep get api.max_memory`, `shep get api.dogs.kelpie.git` or `shep get kelpie.worker` |
| change one setting of a sheep or a dog | none | **set** (plain, not themed), the writer of that same grammar, and not the same verb as `kv set`: `shep kv set` writes a key in the KV store, `shep set` writes a sheep's or a dog's own settings through the shepherd | `shep set <name>[.<path>] <value>`, such as `shep set api.max_memory 512M` or `shep set kelpie.worker.timeout 60` |
| undo one setting of a sheep or a dog | none | **unset** (plain, not themed), the undo of `set` and not the same verb as `kv unset`: `shep kv unset` removes a key from the KV store, `shep unset` drops a sheep field's override so the Flockfile's value returns, or removes a key from a sheep's table for a dog or from a dog's own section so the dog's default applies | `shep unset <name>[.<path>]`, such as `shep unset api.max_memory` or `shep unset kelpie.worker.timeout` |
| one value a dog's per-sheep tables share | none | **defaults address** (plain, not themed): `<dog>.defaults[.<key>]` names the operator-set default table for a dog, layered under the table of every sheep that already carries `[app.dogs.<dog>]`, and is written with `shep set` only (`shep copy` refuses it, `shep unset` points back to `shep set`). It is stored in `$SHEP_HOME/dog-defaults.json` and is not a dog's own shipped defaults (#356), which a dog would publish and the operator never writes | `shep set kelpie.defaults.model opus` |
| move one sheep's setting, or one dog's table for it, onto another sheep | none | **copy** (plain, not themed), and not the same verb as `kv set`: `shep kv set` writes a key in the KV store, `shep copy` writes a sheep's own config through the shepherd | `shep copy <from>.<path> <to>.<path>`, such as `shep copy web.dogs.kelpie api.dogs.kelpie` |
| the store of credential values a config refers to | secrets manager | **secrets store** (plain, not themed): `$SHEP_HOME/secrets.json` | `shep secret set/get/unset/list`, `{{secret:NAME}}` |
| a provider dog's own slice of the secrets store | none | **namespace** (plain, not themed, and a different concept from fold: a namespace keeps one provider dog's pushed values apart from another's, a fold groups sheep) | `{{secret:namespace/NAME}}`, `Request::PutSecrets` |
| what a dog declares about itself before it runs: its settings, verbs, dependencies and the permissions it wants | none | **manifest** (plain) | the dog's probe answer, cached per binary |
| one named thing a dog may do through the shepherd | capability | **permission** (plain, since it appears in refusals); a dog **declares** the permissions it wants in its manifest | "log-rotate declares `logs.read`, which you haven't granted" |
| the permissions the operator has approved for one dog | allowlist | **grants** (plain); the operator **grants** a permission | a request outside a dog's grants is refused |
| a marker a dog attaches to a sheep | deploy tag | **smit** (a farmer's paint mark on a sheep) | `ProcessInfo.smit`, the SMIT column in `shep flock`/lookout; shep stores and paints the string verbatim and never parses it |

## Usage rules (readability > theme)

1. **Straight verbs always work.** `start`, `stop`, `restart`, `list`, `logs`, `delete`
   are first-class aliases forever. Sheep terms are the personality layer, not a wall.
   (Open question in goals.md: which set leads in `--help`.)
2. **Destructive/precise operations keep plain names.** `kill`, `delete`, exit codes,
   error messages — zero whimsy where misreading costs a process.
3. **Types may be themed when self-evident** (`Flock`, `Fold`, `Bark`), never when
   opaque (`Heft` as a struct name = no; "host" it is).
4. **Docs voice**: playful in prose and examples, exact in reference material. The
   README can say "shep keeps your flock alive"; the config reference says "process".
5. **Log/error output**: technical register. The dog barks in webhooks, not in stderr.
