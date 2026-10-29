//! Keeping a comment when the header under it is struck from `shep.toml`.
//!
//! `toml_edit` stores a comment above a header in that header's own decor, so
//! removing the table removes the comment. [`rescue`] reads those comments
//! before the removal and [`restore`] writes them onto whatever now follows.

use toml_edit::{DocumentMut, Item, RawString, Table};

/// A comment that sat above a header about to be struck, and the render
/// position of that header.
pub(super) struct Rescued {
    at: usize,
    text: String,
}

/// The comment above `[dog]` and above every header under it that holds
/// nothing, one per header, in document order.
///
/// A header with a comment carries its whole prefix, blank lines included. A
/// header without one carries nothing, so a bare header leaves no blank line.
pub(super) fn rescue(dog: &Item) -> Vec<Rescued> {
    let mut tables: Vec<&Table> = Vec::new();
    if let Item::Table(table) = dog {
        tables.push(table);
        for (_, child) in table {
            match child {
                Item::Table(inner) if inner.is_empty() => tables.push(inner),
                Item::ArrayOfTables(array) => tables.extend(array.iter().filter(|t| t.is_empty())),
                _ => {}
            }
        }
    }
    let mut found: Vec<Rescued> = tables
        .into_iter()
        .filter_map(|table| {
            let prefix = table.decor().prefix().and_then(RawString::as_str)?;
            prefix.contains('#').then_some(())?;
            Some(Rescued {
                at: table.position()?,
                text: prefix.to_owned(),
            })
        })
        .collect();
    found.sort_by_key(|rescued| rescued.at);
    found
}

/// Puts each rescued comment above the first table that renders after its
/// header, or at the end of the file when none does.
///
/// Last first, so two comments landing on one table keep their order.
pub(super) fn restore(doc: &mut DocumentMut, rescued: Vec<Rescued>) {
    for Rescued { at, text } in rescued.into_iter().rev() {
        let next = next_position(doc.as_table(), at);
        let placed = next.is_some_and(|position| prepend(doc.as_table_mut(), position, &text));
        if !placed {
            let trailing = doc.trailing().as_str().unwrap_or("");
            let joined = format!("{text}{trailing}");
            doc.set_trailing(joined);
        }
    }
}

/// The smallest table position above `at` anywhere under `table`.
fn next_position(table: &Table, at: usize) -> Option<usize> {
    let own = table.position().filter(|position| *position > at);
    let nested = table.iter().filter_map(|(_, child)| match child {
        Item::Table(inner) => next_position(inner, at),
        Item::ArrayOfTables(array) => array
            .iter()
            .filter_map(|inner| next_position(inner, at))
            .min(),
        Item::Value(_) | Item::None => None,
    });
    own.into_iter().chain(nested).min()
}

/// Prepends `text` to the prefix of the table holding `position`, and says
/// whether there was one.
fn prepend(table: &mut Table, position: usize, text: &str) -> bool {
    if table.position() == Some(position) {
        let decor = table.decor_mut();
        let joined = format!(
            "{text}{}",
            decor.prefix().and_then(RawString::as_str).unwrap_or("")
        );
        decor.set_prefix(joined);
        return true;
    }
    table.iter_mut().any(|(_, child)| match child {
        Item::Table(inner) => prepend(inner, position, text),
        Item::ArrayOfTables(array) => array.iter_mut().any(|inner| prepend(inner, position, text)),
        Item::Value(_) | Item::None => false,
    })
}
