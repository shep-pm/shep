# shep-macros

The `dog_config` attribute for [shep](https://github.com/shep-pm/shep), a
process manager written in Rust. A dog (a plugin process shep supervises)
publishes a JSON Schema for its own config, and this is how it marks which of
those fields is a credential.

```rust,ignore
use shep_client::dogs::dog_config;

#[dog_config]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Sink {
    Discord {
        #[shep(secret)]
        url: String,
    },
    Slack {
        #[shep(secret)]
        url: String,
    },
}
```

A field marked `#[shep(secret)]` reaches shep carrying the `x-shep-secret`
schema extension, and shep shows `<set>` in place of the value. A struct works
the same way; the enum is here because a bark sink is one, tagged by kind,
with a webhook URL in every variant.

The extension goes on the field rather than on a property named after it, so
the mark follows a `#[serde(rename)]` and reaches a field of this type nested
inside some other config's schema, at whatever depth `schemars` puts it.

Put it above the derives. rustc expands whatever sits above `#[dog_config]`
before reaching it, so a `JsonSchema` derive listed first has already built
its impl by the time the mark goes on the field. That is refused with a
message naming the fix.

Depend on `shep-client` rather than on this crate: it re-exports the attribute
next to the trait the expansion implements, so a dog takes one dependency.

## Why a macro and not a documented extension

`schemars` can already say the same thing by hand, with
`#[schemars(extend("x-shep-secret" = true))]`. The reason to ship a macro
anyway is that `x-shep-secret` is a string shep parses and the author types.
Transpose two of its letters and it still compiles, the schema still
validates, the field is simply not marked, and a webhook credential ends up
painted on screen. Nothing fails and nothing warns. It cannot be linted
either, because `schemars` takes a string literal for the extension key, so no
exported constant can go in that position. The macro is what turns that into
a compile error.

## License

MIT OR Apache-2.0, at your option.
