# Dogs step 5a: panes

Status: settled with the maintainer in a grill on 2026-11-03. The second
half of step 5 of [Dogs as plugins](2026-10-12-dogs-as-plugins-design.md),
tracked in #1075. Lands after [the catalogue](2026-11-03-dogs-step-5b-catalogue-design.md).
Spec 1's decisions stand except where this amends them.

## What stands from spec 1

- A dog's pane is data, and lookout draws it. Lookout decides layout, keys
  and styling, so a dog's pane feels like the rest of lookout.
- A new kind of section is added only when a real dog needs one.
- A pane only shows what the dog's grants let it read. The dog builds it
  from what it can reach, so lookout enforces nothing extra.

## First consumers

shep-paddock and shep-kelpie. Their needs set the starting kinds:

| Dog | Sections | Actions |
|---|---|---|
| paddock | host fields (VRAM, RAM), a models table | revoke a lease (row) |
| kelpie | board fields (slots, phase), work items and rulings tables | rule (row), pause the board (pane) |

Both build against a published shep-client, so their tickets wait for the
release that carries this step.

## Declaring a pane

- Manifest v3 adds `pane: bool`. `MANIFEST_VERSION` moves to 3. A v2
  manifest's `pane` is read as false, the way a v1 manifest's `events` is.
- `pane` without `channel` is read as false, with one line at adopt.
- Lookout asks for the pane with the reserved action `shep.pane`, the way
  a settings write asks `shep.validate`. Both ends live in
  `shep_core::dogs::pane`, beside `validate.rs`.
- No new request type. Lookout sends `Request::Trigger` as the operator,
  so the pull and every pane action reuse the trigger path and its
  timeout.

## What a pane holds

The answer to `shep.pane` is one object: an ordered list of sections, then
pane actions.

| Kind | Holds |
|---|---|
| `fields` | An id, a title, and label and value pairs |
| `table` | An id, a title, column names, rows each with a stable `key` and one cell per column, and the row actions |
| pane actions | Actions not tied to a row |

- Every value and cell is a string. Lookout formats nothing.
- An action names an action from the manifest's `actions`, a label, and
  `destructive: bool`. Lookout skips an action the manifest does not
  declare and says so in the pane's footer.
- A row action is triggered with `{"row": "<key>"}` as its params. A pane
  action is triggered with no params.
- Lookout gives each action the first free letter of its label in that
  pane, in declared order, shown in the pane's footer.
- A destructive action asks first: "Run <label> on <row key>?". Others
  fire on the key.
- An action that answers with an id (#1433) shows as running in the
  status bar until its result event arrives, then shows the result there.
- Not in the starting set: a log tail (no consumer) and schema forms
  (the settings tab already is one).

## Where it shows

- A dog whose manifest declares `pane` opens on a status tab holding its
  sections. The settings tab, today's form, is second. Tab switches.
- A dog without a pane keeps today's settings pane, unchanged.
- No answer within the trigger's timeout, or an answer lookout cannot
  read: the status tab says which, quoting the dog's reply with the cap
  `shep.validate` uses. The settings tab still works.

```text
┌ paddock ─────────────────────────────┐
│ [status]  settings          tab ⇆     │
│                                       │
│ host                                  │
│   VRAM   20 / 24 GiB                  │
│   RAM    41 / 63 GiB                  │
│                                       │
│ models                                │
│   MODEL        STATE    FOOTPRINT     │
│ ▸ qwen3-32b    loaded   19.8 GiB      │
│   strata-7b    idle      6.1 GiB      │
│                                       │
│ r revoke lease (row)                  │
└───────────────────────────────────────┘
```

## Live values

- Lookout adds `dog.*` to its subscription (`lookout/source.rs`'s
  `TOPICS`). The operator's link is not held to grants, so this needs no
  permission.
- Each section names the event kinds that change it, from the manifest's
  `events`. While the status tab is open, an event of a named kind makes
  lookout ask `shep.pane` again, at most once a second, trailing.
- Lookout also asks again after any of the pane's actions completes.
- The dog never builds partial updates. Its answer to `shep.pane` is the
  one source of truth.

## Where the code lives

No new crate. This amends spec 1, which planned a shared pane crate.

- Wire types and `PANE_ACTION` in `shep_core::dogs::pane`. Every type is
  `#[non_exhaustive]`. `Debug` prints section ids and row counts, never a
  value, since a dog may put anything in a cell.
- A typed builder in `shep_client::dogs`, beside `Probe`. Registering a
  pane on the dog's runtime sets `pane` in the manifest and answers
  `shep.pane`.
- Drawing stays in lookout. Moving it out would mean inverting
  `draw_pane(&App)` for no consumer outside lookout.

```rust
Pane::new()
    .fields("host", [("VRAM", "20 / 24 GiB"), ("RAM", "41 / 63 GiB")])
    .refresh_on(["lease.changed"])
    .table("models", ["MODEL", "STATE", "FOOTPRINT"], rows)
    .row_action("revoke", "Revoke lease", Destructive::Yes)
```

A dog in any language answers `shep.pane` with the JSON.

## Docs

`lookout.astro` gains the status tab, `writing-a-dog.astro` a section on
panes with the builder and the JSON, `shepherd-channel.astro` the reserved
`shep.pane` action, and the manifest reference its v3 field.
