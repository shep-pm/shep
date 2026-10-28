//! `Request::DogConfigResolved`: a dog's section with its `{{secret:...}}`
//! references replaced, and `Request::SheepDogSettingsResolved`: a sheep's
//! table for a dog, the same.

use shep_core::config::DogTable;
use shep_core::config::key_path::push_key;
use shep_core::protocol::{DogSectionToml, RpcError};
use shep_core::secrets::SecretView;

use crate::dogs::{
    DogError, resolve_section, resolve_table, section_references, store_unreadable,
    table_store_unreadable,
};

use super::context::RpcContext;
use super::error::invalid_config;

/// `name`'s section, read fresh from `dogs.toml` and resolved in the
/// shepherd's own environment.
///
/// The raw read comes first, so the text is the operator's own, and the
/// store is read only when the section names a reference: a corrupt
/// `secrets.json` cannot break a dog that asks for none. Nothing here
/// writes `dogs.toml`. Every refusal names a reference, never a value.
pub(super) fn resolved_section(ctx: &RpcContext, name: &str) -> Result<DogSectionToml, RpcError> {
    let raw = crate::dogs::dog_section(&ctx.dogs_config, name).map_err(invalid_config)?;
    let view = if section_references(&raw).is_empty() {
        SecretView::empty(ctx.host_environment.clone())
    } else {
        let store = shep_core::secrets::all(&ctx.paths.secrets)
            .map_err(|err| invalid_config(format!("[{name}] {}", store_unreadable(&raw, &err))))?;
        SecretView::new(
            ctx.host_environment.clone(),
            store,
            ctx.provider_secrets.snapshot(),
        )
    };
    resolve_section(&raw, &view).map_err(|err| match err {
        DogError::Reference { .. } => invalid_config(format!("[{name}] {err}")),
        other => invalid_config(other),
    })
}

/// `table`, `sheep`'s table for `dog`, resolved in `environment`.
///
/// The rules of [`resolved_section`]: the store is read only when the table
/// names a reference, a table with none is still resolved against an empty
/// view so a malformed `{{secret:` is refused, and nothing is written.
/// Every refusal names the sheep, the dog, the key path and the reference,
/// never a value.
pub(super) fn resolved_sheep_table(
    ctx: &RpcContext,
    sheep: &str,
    dog: &str,
    environment: &str,
    table: &DogTable,
) -> Result<DogTable, RpcError> {
    let mut root = String::from("dogs");
    push_key(&mut root, dog);
    let view = if table.references().is_empty() {
        SecretView::empty(environment.to_string())
    } else {
        let store = shep_core::secrets::all(&ctx.paths.secrets).map_err(|err| {
            invalid_config(format!(
                "[{sheep}] {}",
                table_store_unreadable(table, &root, &err)
            ))
        })?;
        SecretView::new(
            environment.to_string(),
            store,
            ctx.provider_secrets.snapshot(),
        )
    };
    resolve_table(table, &root, &view).map_err(|err| match err {
        DogError::Reference { .. } => invalid_config(format!("[{sheep}] {err}")),
        other => invalid_config(other),
    })
}
