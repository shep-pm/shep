//! The `dog_config` attribute, and nothing else.
//!
//! A dog publishes a JSON Schema for its config so shep can render a
//! settings pane. A credential field needs marking, shown as `<set>` rather
//! than its value. `schemars` can mark it by hand with
//! `#[schemars(extend("x-shep-secret" = true))]`. That key is a string an
//! author can misspell silently. This attribute writes it, and turns a
//! misspelled option into a compile error.
//!
//! Depend on `shep-client`, not this crate: it re-exports the attribute.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Field, Meta, Token, parse_macro_input, parse_quote};

/// The attribute this macro claims on a field. Only `#[shep(secret)]` is
/// spelled with it today.
const ATTR: &str = "shep";

/// The one option `#[shep(...)]` accepts.
const SECRET: &str = "secret";

/// The derive a marked field's extension rides on, checked by name because a
/// macro cannot resolve a path.
const JSON_SCHEMA: &str = "JsonSchema";

/// Marks which of a dog's config fields are credentials, so shep can redact
/// them. The author never types the `schemars` extension key that says so.
///
/// Put it above the derives on the type a dog deserializes its config into.
/// Mark each credential field with `#[shep(secret)]`. Structs and enums both
/// work. A `JsonSchema` derive listed above this attribute is refused.
///
/// # Example
///
/// ```rust
/// use shep_client::dogs::dog_config;
///
/// #[dog_config]
/// #[derive(serde::Deserialize, schemars::JsonSchema)]
/// #[serde(tag = "kind", rename_all = "snake_case")]
/// enum Sink {
///     Discord {
///         #[shep(secret)]
///         url: String,
///     },
///     Slack {
///         #[shep(secret)]
///         url: String,
///     },
/// }
/// ```
///
/// The mark rides the field rather than naming it, so it reaches a property
/// `schemars` renamed and one nested inside another type's subschema. A type
/// with no marked field is unaffected: it takes no new attribute and needs no
/// derive, which is what keeps `shep-client`'s `schema` feature optional.
/// Every deliberate refusal is a compile error with its own message.
///
/// The expansion, with each `#[shep(secret)]` replaced by the `schemars`
/// extension and an `impl shep_client::dogs::DogConfig` added:
///
/// ```rust,ignore
/// #[derive(serde::Deserialize, schemars::JsonSchema)]
/// #[serde(tag = "kind", rename_all = "snake_case")]
/// enum Sink {
///     Discord {
///         #[schemars(extend("x-shep-secret" = true))]
///         url: String,
///     },
///     // ...
/// }
///
/// impl ::shep_client::dogs::DogConfig for Sink {}
/// ```
#[proc_macro_attribute]
pub fn dog_config(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = TokenStream2::from(args);
    let mut input = parse_macro_input!(input as DeriveInput);
    match expand(&args, &mut input) {
        Ok(tokens) => tokens.into(),
        Err(err) => {
            // The type is emitted even on the error path, stripped of
            // attributes only this macro knows. Dropping it leaves the derives
            // above holding an impl for a type that no longer exists. One
            // error, not a cascade.
            strip_shep_attributes(&mut input);
            let refusal = err.into_compile_error();
            let carry_on = dog_config_impl(&input);
            quote! {
                #input
                #carry_on
                #refusal
            }
            .into()
        }
    }
}

/// The macro's whole body, in a form that can return an error instead of
/// a token stream.
///
/// Mutates `input`: a marked field loses its `#[shep(secret)]` and gains the
/// `schemars` extension. The attribute has to go, since nothing downstream
/// registers it and rustc refuses an attribute no macro claims.
fn expand(args: &TokenStream2, input: &mut DeriveInput) -> syn::Result<TokenStream2> {
    if !args.is_empty() {
        return Err(syn::Error::new_spanned(
            args,
            "`#[dog_config]` takes no arguments: the marking goes on the \
             fields, as `#[shep(secret)]`",
        ));
    }

    if let Some(attr) = find_shep_attribute(&input.attrs) {
        return Err(syn::Error::new_spanned(
            attr,
            "`#[shep(secret)]` marks a field, not the type: move it onto the \
             field that holds the credential",
        ));
    }

    // Every refusal is decided before a single extension goes on, because
    // the error path emits the type: a half-marked one would carry a
    // `schemars` attribute with no derive left to claim it, and rustc would
    // report that ahead of the refusal that explains it.
    let derives_json_schema = derives_json_schema(&input.attrs);
    let mut marked: Vec<&mut Field> = Vec::new();
    for field in fields_of_mut(input)? {
        if !take_secret(field)? {
            continue;
        }
        if field.ident.is_none() {
            return Err(syn::Error::new_spanned(
                field,
                "`#[shep(secret)]` cannot mark an unnamed field: `schemars` \
                 puts it in `prefixItems`, where a settings pane reading \
                 properties by name has nothing to find. Name the field.",
            ));
        }
        marked.push(field);
    }

    if !marked.is_empty() && !derives_json_schema {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "a `#[shep(secret)]` field needs `#[derive(schemars::JsonSchema)]` \
             BELOW `#[dog_config]` on this type. The mark is written as a \
             `schemars` attribute, so a derive listed above this one has \
             already expanded without it and the credential would go out \
             unmarked. Move `#[dog_config]` above the derives, or add the \
             derive.",
        ));
    }

    for field in marked {
        field.attrs.push(secret_extension());
    }

    let carry_on = dog_config_impl(input);
    Ok(quote! {
        #input

        #carry_on
    })
}

/// The impl the attribute exists to write.
///
/// Emitted on the error path too, so a refusal does not also break every
/// `probe::<T>` and `config_schema::<T>` the dog calls. The build still
/// fails on the refusal; what this buys is that it fails once, naming the
/// real problem.
fn dog_config_impl(input: &DeriveInput) -> TokenStream2 {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        impl #impl_generics ::shep_client::dogs::DogConfig
            for #name #ty_generics #where_clause {}
    }
}

/// Removes every `#[shep(...)]` from the type, its variants and its fields.
///
/// Nothing downstream registers the attribute, so one left behind is an
/// error of its own on top of whatever refusal is being reported.
fn strip_shep_attributes(input: &mut DeriveInput) {
    let is_shep = |attr: &Attribute| attr.path().is_ident(ATTR);
    input.attrs.retain(|attr| !is_shep(attr));
    let fields: Vec<&mut Field> = match &mut input.data {
        Data::Struct(data) => data.fields.iter_mut().collect(),
        Data::Enum(data) => data
            .variants
            .iter_mut()
            .flat_map(|variant| {
                variant.attrs.retain(|attr| !is_shep(attr));
                variant.fields.iter_mut()
            })
            .collect(),
        Data::Union(data) => data.fields.named.iter_mut().collect(),
    };
    for field in fields {
        field.attrs.retain(|attr| !is_shep(attr));
    }
}

/// Whether the type still has a `derive` naming `JsonSchema` for the
/// extension on a marked field to reach.
///
/// False for a type that derives it ABOVE `#[dog_config]` as well as for one
/// that does not derive it at all: rustc expands the attributes above this
/// one first and leaves none of them in the input, so the two are the same
/// picture from here and have the same fix.
fn derives_json_schema(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            let mut found = false;
            // A `derive` that will not parse as a path list is rustc's error to
            // report, not this macro's, so a failure here says only "not found".
            let _ = attr.parse_nested_meta(|meta| {
                if meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|last| last.ident == JSON_SCHEMA)
                {
                    found = true;
                }
                Ok(())
            });
            found
        })
}

/// The `schemars` extension a marked field carries.
///
/// The key is a literal because `schemars` accepts only a literal in that
/// position, which is the whole reason this crate exists. It must stay equal
/// to `shep_core::dogs::SECRET_KEY`, and
/// `the_extension_key_is_the_one_shep_core_publishes` in `shep-client` is
/// what fails when it does not.
fn secret_extension() -> Attribute {
    parse_quote!(#[schemars(extend("x-shep-secret" = true))])
}

/// Every field the type has, a struct's directly and an enum's
/// gathered from its variants.
///
/// Unit variants and unmarked tuples have no fields to collect, so they
/// come back empty. A union is refused outright: it has no serde
/// representation for a mark to land on. [`expand`] checks each
/// collected field once it knows which carry a mark.
fn fields_of_mut(input: &mut DeriveInput) -> syn::Result<Vec<&mut Field>> {
    match &mut input.data {
        Data::Struct(data) => Ok(data.fields.iter_mut().collect()),
        Data::Enum(data) => {
            let mut fields = Vec::new();
            for variant in &mut data.variants {
                if let Some(attr) = find_shep_attribute(&variant.attrs) {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "`#[shep(secret)]` marks a field, not a variant: move \
                         it onto the field inside that holds the credential",
                    ));
                }
                fields.extend(variant.fields.iter_mut());
            }
            Ok(fields)
        }
        Data::Union(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "`#[dog_config]` cannot be used on a union: a union has no serde \
             representation, so there is no schema for a mark to go into",
        )),
    }
}

/// Whether a field carries `#[shep(secret)]`, removing every `#[shep(...)]`
/// it finds.
///
/// Repeating the attribute on one field is accepted and marks it
/// once. It is redundant rather than wrong. A second error message
/// would be noise next to the one that matters: a misspelled option.
fn take_secret(field: &mut Field) -> syn::Result<bool> {
    let mut secret = false;
    for attr in &field.attrs {
        if !attr.path().is_ident(ATTR) {
            continue;
        }
        if !matches!(&attr.meta, Meta::List(list) if !list.tokens.is_empty()) {
            return Err(syn::Error::new_spanned(
                attr,
                "`#[shep]` needs an option in parentheses: the only one is \
                 `secret`, as in `#[shep(secret)]`",
            ));
        }
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident(SECRET) {
                return Err(meta.error(
                    "unknown `shep` option: the only one is `secret`, as in \
                     `#[shep(secret)]`",
                ));
            }
            if meta.input.peek(Token![=]) {
                return Err(meta.error(
                    "`secret` is a flag and takes no value: write \
                     `#[shep(secret)]`",
                ));
            }
            secret = true;
            Ok(())
        })?;
    }
    field.attrs.retain(|attr| !attr.path().is_ident(ATTR));
    Ok(secret)
}

/// The first `#[shep(...)]` in a list, for the places one does not belong.
fn find_shep_attribute(attrs: &[Attribute]) -> Option<&Attribute> {
    attrs.iter().find(|attr| attr.path().is_ident(ATTR))
}
