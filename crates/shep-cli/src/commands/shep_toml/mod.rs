//! `$SHEP_HOME/shep.toml`, the daemon's own config file, read and rewritten
//! by [`ShepToml`], the one writer this binary has for it.
//!
//! Edits go through `toml_edit`'s [`DocumentMut`], so an operator's comments
//! and key order survive. [`shep_core::config::DaemonConfig::load`] decides
//! what a key means; this module only adds or removes the ones each verb owns.
//!
//! [`ShepToml::edit`] and [`ShepToml::try_edit`] are the whole write path:
//! `$SHEP_HOME` at `0700`, an exclusive advisory lock on a sibling
//! `shep.toml.lock` across the read-modify-write, and the document staged
//! `0600`, `fsync`ed and `rename`d. A `try_edit` closure's own `Err` leaves
//! `path` untouched.

// Fires only on Windows: `ShepTomlError` crosses the lint's 128-byte
// threshold there and stays under it elsewhere.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::style::StyleLevel;

/// Extensions [`ShepToml::write_starter_interpreters`] maps, in the order
/// they land in `shep.toml`.
///
/// `py` maps to `python3` rather than bare `python`, which is absent or
/// still points at Python 2 on plenty of hosts shep runs on. `ts` is left
/// out: ts-node, tsx and deno disagree about how to run one, and guessing
/// wrong silently is worse than making the operator say so.
const STARTER_INTERPRETERS: &[(&str, &str)] = &[
    ("js", "node"),
    ("mjs", "node"),
    ("cjs", "node"),
    ("py", "python3"),
    ("rb", "ruby"),
    ("sh", "sh"),
    ("pl", "perl"),
    ("php", "php"),
];

/// The comment [`ShepToml::write_starter_interpreters`] writes directly
/// above the `[interpreters]` table it scaffolds.
///
/// Plain `#` TOML comment lines: this text lands inside `shep.toml` for an
/// operator to read, so the same copy rules govern it as `welcome.rs`'s.
const INTERPRETERS_STARTER_COMMENT: &str = "\
# Extension -> interpreter mapping. shep applies one of these to a script
# when nothing more specific already named an interpreter: not this app's
# own Flockfile entry, and not --interpreter on the command line, both of
# which win over anything here. shep never guesses beyond what is written
# below, so edit freely: change an interpreter, add an extension, or
# delete an entry (or this whole table) to turn the mapping off for it.
";

/// The one writer of `$SHEP_HOME/shep.toml` in this binary.
///
/// A missing file is created as an empty document, `$SHEP_HOME` with it; a
/// file that will not parse is refused rather than overwritten, since it may
/// hold every knob a daemon boots with, credentials included.
///
/// [`Self::edit`] and [`Self::try_edit`] are the only paths that write, and
/// they hold the document's lock for exactly as long as the closure runs.
/// Reading and writing are not separate public steps: a caller that could
/// read, think, and then write is the lost update this type takes a lock to
/// prevent.
pub struct ShepToml {
    path: PathBuf,
    doc: DocumentMut,
}

/// Manual, not derived: `doc` can hold a webhook URL with a bearer token in
/// an un-migrated `[dog.<name>]` table, so only the path is printed.
impl std::fmt::Debug for ShepToml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShepToml")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ShepToml {
    /// Reads `path`, hands the document to `f`, and writes it back under
    /// one exclusive advisory lock on a sibling `shep.toml.lock`
    /// ([`ConfigLock`]).
    ///
    /// `f`'s return value comes back on success. `f` is infallible;
    /// [`Self::try_edit`] takes one that can refuse.
    ///
    /// # Errors
    /// - [`ShepTomlError::Io`] if `$SHEP_HOME` could not be created, the
    ///   lock could not be taken, or the file could not be read or replaced.
    /// - [`ShepTomlError::Parse`] if the file is not valid TOML. Refused
    ///   rather than overwritten, and `f` never runs.
    pub fn edit<T>(path: &Path, f: impl FnOnce(&mut Self) -> T) -> Result<T, ShepTomlError> {
        let (mut doc, _lock) = Self::open_locked(path)?;
        let value = f(&mut doc);
        doc.save()?;
        Ok(value)
    }

    /// Like [`Self::edit`], but for a closure that can itself refuse: `f`'s
    /// own `Err` skips [`Self::save`] entirely. Saving anyway would stage
    /// and rename a byte-identical copy, and that rename still lands a fresh
    /// inode, forces [`shep_core::atomic_file::OWNER_ONLY_FILE_MODE`], and
    /// replaces a symlinked `path` with a plain file.
    ///
    /// `E: From<ShepTomlError>` is what lets `?` cover this method's own
    /// setup failures (home dir, lock, parse) as well as `f`'s.
    ///
    /// # Errors
    /// Everything [`Self::edit`] can fail with, converted through `E::from`,
    /// plus whatever `f` returns as `Err`. `path` is untouched either way.
    pub fn try_edit<T, E: From<ShepTomlError>>(
        path: &Path,
        f: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let (mut doc, _lock) = Self::open_locked(path)?;
        let value = f(&mut doc)?;
        doc.save()?;
        Ok(value)
    }

    /// Creates `$SHEP_HOME` if missing, takes `path`'s exclusive lock, and
    /// opens the document: the setup [`Self::edit`] and [`Self::try_edit`]
    /// share.
    ///
    /// The returned [`ConfigLock`] must outlive every use of the returned
    /// `Self`. It is what makes this read and the caller's eventual `save`
    /// one transaction as far as any other editor is concerned.
    fn open_locked(path: &Path) -> Result<(Self, ConfigLock), ShepTomlError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        create_home_dir(parent).map_err(|source| ShepTomlError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let lock = ConfigLock::acquire(path).map_err(|source| ShepTomlError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let doc = Self::open(path)?;
        Ok((doc, lock))
    }

    /// Reads `path`, treating a missing file as an empty document.
    ///
    /// Reached from [`Self::edit`]/[`Self::try_edit`] with the document's
    /// lock held, and from the read-only callers with no lock at all.
    ///
    /// # Errors
    /// - [`ShepTomlError::Io`] if the file exists and could not be read.
    /// - [`ShepTomlError::Parse`] if the file exists and is not valid TOML.
    fn open(path: &Path) -> Result<Self, ShepTomlError> {
        let doc = match std::fs::read_to_string(path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .map_err(|source| ShepTomlError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
            Err(source) => {
                return Err(ShepTomlError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            doc,
        })
    }

    /// Reads `path` for a caller that only wants the answer: the settings
    /// screen's door into this type, and the shape every reader below is
    /// reached through.
    ///
    /// Takes no lock. [`Self::save`]'s rename onto `path` is atomic, so a
    /// concurrent writer can only make this read observe the document just
    /// before or just after that write, never a torn one.
    ///
    /// # Errors
    /// [`ShepTomlError::Io`] if `path` exists and could not be read.
    /// [`ShepTomlError::Parse`] if `path` exists and is not valid TOML.
    pub fn read_only(path: &Path) -> Result<Self, ShepTomlError> {
        Self::open(path)
    }

    /// Renders the in-memory document exactly as [`Self::save`] would
    /// write it, without touching disk.
    ///
    /// `commands::settings::apply_setting` mutates, renders, and hands the
    /// text to [`DaemonConfig::load`](shep_core::config::DaemonConfig::load)
    /// before calling [`Self::save`], so a refusal never stages or renames.
    ///
    /// A named method rather than a `Display` impl: this text can carry a
    /// dog's webhook token, and `format!("{doc}")` is what a future caller
    /// reaches for without thinking about that.
    #[must_use]
    pub(crate) fn rendered(&self) -> String {
        self.doc.to_string()
    }

    /// Adds `name` to `[daemon] enabled_dogs` (idempotently), and writes
    /// nothing else anywhere.
    ///
    /// Never scaffold an empty `[dog.<name>]` here. A dog's configuration
    /// lives in `dogs.toml`, and `commands::dog_migration` refuses to boot
    /// when one name holds values in both files. An enabled dog with no
    /// section runs on its defaults, so there is nothing to scaffold.
    pub fn enable_dog(&mut self, name: &str) {
        let daemon = self.daemon_table_mut();
        let enabled_dogs = daemon
            .entry("enabled_dogs")
            .or_insert_with(|| Item::Value(Value::Array(Array::new())))
            .as_array_mut()
            .expect("enabled_dogs is only ever written as an array");
        if !enabled_dogs.iter().any(|v| v.as_str() == Some(name)) {
            enabled_dogs.push(name);
        }
    }

    /// Removes `name` from `[daemon] enabled_dogs` and touches nothing
    /// else: an operator who disables a dog to restart it must not lose the
    /// configuration they wrote for it.
    ///
    /// That configuration lives in `dogs.toml`, so keeping it takes doing
    /// nothing at all here. [`Self::rehome_dog`] is the half that forgets a
    /// dog for real.
    pub fn disable_dog(&mut self, name: &str) {
        if let Some(enabled_dogs) = self
            .doc
            .get_mut("daemon")
            .and_then(Item::as_table_mut)
            .and_then(|daemon| daemon.get_mut("enabled_dogs"))
            .and_then(Item::as_array_mut)
        {
            enabled_dogs.retain(|v| v.as_str() != Some(name));
        }
    }

    /// Records `name`'s binary in `[daemon] adopted_dogs` and enables it.
    ///
    /// Does no vetting of `exec` itself: `commands::dogs::adopt` has already
    /// run `vet_binary`.
    pub fn adopt_dog(&mut self, name: &str, exec: &Path) {
        let daemon = self.daemon_table_mut();
        let adopted_dogs = daemon
            .entry("adopted_dogs")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .expect("adopted_dogs is only ever written as a table");
        adopted_dogs.insert(
            name,
            Item::Value(exec.to_string_lossy().into_owned().into()),
        );
        self.enable_dog(name);
    }

    /// Removes the whole `[dog]` table and hands back what was under it,
    /// keyed by name with the `dog.` prefix dropped.
    ///
    /// Handed back as live [`Item`]s: a comment an operator wrote around
    /// `[dog.metrics]` travels with the section. Only table-like entries
    /// come back; `[dog] stray = 5` and `[[dog.x]]` are dropped. A document
    /// with no `[dog]` table yields an empty map, left byte-identical.
    ///
    /// Takes everything: a partial move would leave one key readable from
    /// two files.
    pub fn take_dog_sections(&mut self) -> BTreeMap<String, Item> {
        let Some(item) = self.doc.remove("dog") else {
            return BTreeMap::new();
        };
        // `[[dog]]` itself, the one shape with nothing table-like under it
        // to iterate.
        let Some(dog) = item.as_table_like() else {
            return BTreeMap::new();
        };
        dog.iter()
            .filter(|(_, value)| value.as_table_like().is_some())
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect()
    }

    /// The binary path recorded for `name` in `[daemon] adopted_dogs`, if
    /// any. `None` for a built-in dog, or a name this document never heard of.
    #[must_use]
    pub fn adopted_dog_path(&self, name: &str) -> Option<PathBuf> {
        self.doc
            .get("daemon")?
            .as_table()?
            .get("adopted_dogs")?
            .as_table()?
            .get(name)?
            .as_str()
            .map(PathBuf::from)
    }

    /// Every name `[daemon] adopted_dogs` records, in TOML document order.
    #[must_use]
    pub fn adopted_dog_names(&self) -> Vec<String> {
        self.doc
            .get("daemon")
            .and_then(Item::as_table)
            .and_then(|daemon| daemon.get("adopted_dogs"))
            .and_then(Item::as_table)
            .map(|adopted| adopted.iter().map(|(name, _)| name.to_string()).collect())
            .unwrap_or_default()
    }

    /// The names in `[daemon] enabled_dogs`, in file order.
    ///
    /// Distinct from [`Self::adopted_dog_names`]: a dog can be adopted and
    /// not enabled, or built in and enabled without ever being adopted.
    #[must_use]
    pub fn enabled_dog_names(&self) -> Vec<String> {
        self.table("daemon")
            .and_then(|daemon| daemon.get("enabled_dogs"))
            .and_then(Item::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// [`Self::adopted_dog_path`] without the write side, for `lib.rs`'s
    /// `dispatch_adopted_dog`, which runs on every unrecognized verb.
    ///
    /// Creates nothing: a missing `$SHEP_HOME` or `path` is an ordinary "no
    /// such dog" answer, never a reason to create either. Takes no lock,
    /// for the reason [`Self::read_only`] gives.
    ///
    /// # Errors
    /// [`ShepTomlError::Io`] if `path` exists and could not be read.
    /// [`ShepTomlError::Parse`] if `path` exists and is not valid TOML.
    pub fn adopted_dog_path_readonly(
        path: &Path,
        name: &str,
    ) -> Result<Option<PathBuf>, ShepTomlError> {
        Ok(Self::open(path)?.adopted_dog_path(name))
    }

    /// Forgets `name`'s adoption in this file: out of `enabled_dogs` and
    /// out of `adopted_dogs`.
    ///
    /// The whole of a rehome's config half. A `[dog.<name>]` an un-migrated
    /// `shep.toml` still carries stays, as the `[<name>]` in `dogs.toml`
    /// does: the settings an operator wrote are theirs, and `rehome`
    /// forgets only where the binary lived. The difference from
    /// [`Self::disable_dog`] is `adopted_dogs`, so recovery needs a fresh
    /// `shep adopt <path>` rather than an `enable`.
    pub fn rehome_dog(&mut self, name: &str) {
        self.disable_dog(name);
        if let Some(adopted_dogs) = self
            .doc
            .get_mut("daemon")
            .and_then(Item::as_table_mut)
            .and_then(|daemon| daemon.get_mut("adopted_dogs"))
            .and_then(Item::as_table_mut)
        {
            adopted_dogs.remove(name);
        }
    }

    /// Writes `[style] level = "<level>"`, creating the `[style]` table when
    /// this document has none yet.
    ///
    /// The value is `level`'s own `Display` spelling, the same string
    /// `style_from_config` (`lib.rs`) parses back through
    /// `clap::ValueEnum::from_str`, so a round trip stays one grammar.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `style` is already there as
    /// something other than a table, e.g. a hand-written `style = "full"` at
    /// the top level. Reported rather than `expect`ed, since that shape
    /// comes from data this process does not control.
    pub fn set_style_level(&mut self, level: StyleLevel) -> Result<(), ShepTomlError> {
        let item = self
            .doc
            .entry("style")
            .or_insert_with(|| Item::Table(Table::new()));
        let Some(style) = item.as_table_mut() else {
            return Err(ShepTomlError::WrongShape {
                path: self.path.clone(),
                key: "style",
                found: item.type_name(),
            });
        };
        style.insert("level", Item::Value(level.to_string().into()));
        Ok(())
    }

    /// `[style] level`, or `None` when the document never wrote it, as the
    /// raw string on disk.
    ///
    /// `[style]` is the one settings field whose value in force can come
    /// from a layer above the file (`--style`, `$SHEP_STYLE`), so the
    /// resolved level and the level the document declares are two different
    /// facts.
    #[must_use]
    pub fn style_level(&self) -> Option<String> {
        self.table("style")?
            .get("level")?
            .as_str()
            .map(String::from)
    }

    /// `[daemon] log_json`, or `None` when the document never wrote it.
    ///
    /// A key written to its own default is still `Some`.
    /// [`DaemonConfig::load`]'s `#[serde(default)]` loses that distinction,
    /// so this reader and its four siblings below read the document itself.
    #[must_use]
    pub fn daemon_log_json(&self) -> Option<bool> {
        self.table("daemon")?.get("log_json")?.as_bool()
    }

    /// `[daemon] log_level`, or `None` when the document never wrote it,
    /// as the raw string on disk. Whether it names a real `LogLevel` is
    /// [`DaemonConfig::load`]'s question, not this reader's.
    #[must_use]
    pub fn daemon_log_level(&self) -> Option<String> {
        self.table("daemon")?
            .get("log_level")?
            .as_str()
            .map(String::from)
    }

    /// `[daemon] socket`, or `None` when the document never wrote it.
    #[must_use]
    pub fn daemon_socket(&self) -> Option<PathBuf> {
        self.table("daemon")?
            .get("socket")?
            .as_str()
            .map(PathBuf::from)
    }

    /// `[daemon] max_cron_sleep`, or `None` when the document never wrote
    /// it, as the raw string on disk rather than a parsed `UpDuration`:
    /// parsing here would put a second opinion about the grammar next to
    /// `DaemonConfig`'s.
    #[must_use]
    pub fn daemon_max_cron_sleep(&self) -> Option<String> {
        self.table("daemon")?
            .get("max_cron_sleep")?
            .as_str()
            .map(String::from)
    }

    /// `[whistle] allow_control`, or `None` when the document never wrote
    /// it.
    #[must_use]
    pub fn whistle_allow_control(&self) -> Option<bool> {
        self.table("whistle")?.get("allow_control")?.as_bool()
    }

    /// Writes `[daemon] log_json = <value>`, creating `[daemon]` when this
    /// document has none yet.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `daemon` is already there as
    /// something other than a table.
    pub fn set_daemon_log_json(&mut self, value: bool) -> Result<(), ShepTomlError> {
        self.section_table_mut("daemon")?
            .insert("log_json", Item::Value(value.into()));
        Ok(())
    }

    /// Writes `[daemon] log_level = "<value>"`, creating `[daemon]` when
    /// this document has none yet. `value` is written as given, unchecked:
    /// [`DaemonConfig::load`] is what refuses a name that is not a real
    /// `LogLevel`.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `daemon` is already there as
    /// something other than a table.
    pub fn set_daemon_log_level(&mut self, value: &str) -> Result<(), ShepTomlError> {
        self.section_table_mut("daemon")?
            .insert("log_level", Item::Value(value.into()));
        Ok(())
    }

    /// Writes `[daemon] socket = "<value>"`, creating `[daemon]` when this
    /// document has none yet.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `daemon` is already there as
    /// something other than a table.
    pub fn set_daemon_socket(&mut self, value: &Path) -> Result<(), ShepTomlError> {
        self.section_table_mut("daemon")?.insert(
            "socket",
            Item::Value(value.to_string_lossy().into_owned().into()),
        );
        Ok(())
    }

    /// Writes `[daemon] max_cron_sleep = "<value>"`, creating `[daemon]`
    /// when this document has none yet. `value` is written as given,
    /// unchecked: [`DaemonConfig::load`] is what refuses a duration below
    /// the floor or one that does not parse at all.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `daemon` is already there as
    /// something other than a table.
    pub fn set_daemon_max_cron_sleep(&mut self, value: &str) -> Result<(), ShepTomlError> {
        self.section_table_mut("daemon")?
            .insert("max_cron_sleep", Item::Value(value.into()));
        Ok(())
    }

    /// Writes `[whistle] allow_control = <value>`, creating `[whistle]`
    /// when this document has none yet.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] if `whistle` is already there as
    /// something other than a table.
    pub fn set_whistle_allow_control(&mut self, value: bool) -> Result<(), ShepTomlError> {
        self.section_table_mut("whistle")?
            .insert("allow_control", Item::Value(value.into()));
        Ok(())
    }

    /// Removes `[daemon] socket` if it is set, and does nothing when
    /// `[daemon]` is absent or is not a table. No `Result`: removing a key
    /// from something that is not a table is already a no-op.
    pub fn unset_daemon_socket(&mut self) {
        if let Some(daemon) = self.doc.get_mut("daemon").and_then(Item::as_table_mut) {
            daemon.remove("socket");
        }
    }

    /// Removes `[daemon] max_cron_sleep` if it is set.
    pub fn unset_daemon_max_cron_sleep(&mut self) {
        if let Some(daemon) = self.doc.get_mut("daemon").and_then(Item::as_table_mut) {
            daemon.remove("max_cron_sleep");
        }
    }

    /// Writes the starter `[interpreters]` mapping, a script extension to
    /// the interpreter shep runs it with, under
    /// `INTERPRETERS_STARTER_COMMENT`.
    ///
    /// Written live rather than commented out: shep never infers an
    /// interpreter on its own, and a fresh `$SHEP_HOME` still has to run the
    /// `shep start server.js` that `welcome.rs` and `--help` advertise.
    ///
    /// A no-op when `[interpreters]` already exists, so a hand-edited
    /// `shep.toml` is never clobbered or duplicated.
    pub fn write_starter_interpreters(&mut self) {
        if self.doc.contains_key("interpreters") {
            return;
        }
        let mut table = Table::new();
        for (extension, interpreter) in STARTER_INTERPRETERS {
            table.insert(extension, Item::Value((*interpreter).into()));
        }
        table.decor_mut().set_prefix(INTERPRETERS_STARTER_COMMENT);
        self.doc.insert("interpreters", Item::Table(table));
    }

    /// Writes the document back: staged in a sibling temp file at
    /// [`shep_core::atomic_file::OWNER_ONLY_FILE_MODE`], `fsync`ed,
    /// `rename`d over `path`, then the directory `fsync`ed so the rename
    /// survives a power cut.
    ///
    /// Not `std::fs::write`: its `O_TRUNC` would leave an operator's whole
    /// `shep.toml` empty on a crash between truncate and write. The rename
    /// also re-tightens a `shep.toml` an older shep left at `0644`.
    ///
    /// # Errors
    /// - [`ShepTomlError::Io`] if the staging file could not be written, the
    ///   rename over `path` failed, or the directory could not be flushed.
    fn save(&self) -> Result<(), ShepTomlError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = create_config_file(parent).map_err(|source| self.io_error(source))?;
        tmp.write_all(self.doc.to_string().as_bytes())
            .map_err(|source| self.io_error(source))?;
        shep_core::atomic_file::publish(tmp, &self.path).map_err(|source| self.io_error(source))
    }

    fn io_error(&self, source: std::io::Error) -> ShepTomlError {
        ShepTomlError::Io {
            path: self.path.clone(),
            source,
        }
    }

    /// `section` as a table, or `None` if this document never wrote it or
    /// wrote it as something else. The read side every scalar reader above
    /// shares: a reader has nothing to refuse, unlike a setter.
    fn table(&self, section: &str) -> Option<&Table> {
        self.doc.get(section).and_then(Item::as_table)
    }

    /// `section` as a table, creating it empty if this document has none
    /// yet, and refusing with [`ShepTomlError::WrongShape`] if `section` is
    /// already occupied by something else. The write side every scalar
    /// setter above shares.
    fn section_table_mut(&mut self, section: &'static str) -> Result<&mut Table, ShepTomlError> {
        let item = self
            .doc
            .entry(section)
            .or_insert_with(|| Item::Table(Table::new()));
        let found = item.type_name();
        item.as_table_mut().ok_or(ShepTomlError::WrongShape {
            path: self.path.clone(),
            key: section,
            found,
        })
    }

    /// `[daemon]`, creating it (empty) if this document has none yet.
    fn daemon_table_mut(&mut self) -> &mut Table {
        self.doc
            .entry("daemon")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .expect("daemon is only ever written as a table")
    }
}

/// Creates `dir` (and any missing parent) at `boot::DIR_MODE` directly,
/// via `DirBuilderExt`, rather than `create_dir_all` and a later `chmod`.
///
/// `$SHEP_HOME` holds webhook URLs, and on a host that has never booted a
/// shepherd this call is the one that creates it: `boot::init_dirs`, which
/// force-chmods it, does not run until the first `shep muster`. A
/// `create_dir_all` would leave it at the ambient umask, typically `0755`,
/// until that boot, and asking for the mode at `mkdir` time leaves no window
/// in which the directory exists wider.
///
/// Reuses `shep_daemon::boot::DIR_MODE` rather than restating `0o700`.
fn create_home_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    // Windows has no scalar mode; `shep_daemon::boot::create_dir_at_dir_mode`
    // carries the argument for what protects `$SHEP_HOME` there instead.
    #[cfg(unix)]
    builder.mode(shep_daemon::boot::DIR_MODE);
    builder.create(dir)
}

/// `ConfigLock` and `create_config_file` moved to
/// [`shep_core::config_lock`] so shep-daemon can hold the same lock over
/// `dogs.toml` shep-cli does; both names still resolve here unchanged for
/// this crate's own callers.
pub(super) use shep_core::config_lock::{ConfigLock, create_config_file};

/// What [`ShepToml::edit`] can fail with.
///
/// Not `#[non_exhaustive]`: nothing outside this binary can match on it, and
/// this crate's own exhaustive matches are the ones the compiler should
/// break when a new failure mode lands.
pub enum ShepTomlError {
    /// A read or write of `path` itself failed.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying IO failure.
        source: std::io::Error,
    },
    /// `path` exists but is not valid TOML.
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The parser's own complaint.
        source: toml_edit::TomlError,
    },
    /// `path` parses, but `key` is already there as something other than a
    /// table, e.g. `style = "full"` at the top level instead of `[style]`.
    /// Legal TOML, but forcing it to a table would discard what the operator
    /// wrote there.
    WrongShape {
        /// The file that holds the wrongly-shaped value.
        path: PathBuf,
        /// The table key that was expected.
        key: &'static str,
        /// What TOML found there ([`Item::type_name`]); never `"table"`.
        found: &'static str,
    },
}

/// Manual, not derived: `toml_edit::TomlError` keeps the whole source
/// document for `Display`'s line-and-column rendering, so a derived `Debug`
/// would print `shep.toml` in full, secrets included. `Debug` is what a log
/// captures, so it carries the path and the parser's short `message()` only;
/// `Display` still shows the full message, for the operator to read.
impl std::fmt::Debug for ShepTomlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => f
                .debug_struct("Io")
                .field("path", path)
                .field("source", source)
                .finish(),
            Self::Parse { path, source } => f
                .debug_struct("Parse")
                .field("path", path)
                .field("message", &source.message())
                .finish(),
            Self::WrongShape { path, key, found } => f
                .debug_struct("WrongShape")
                .field("path", path)
                .field("key", key)
                .field("found", found)
                .finish(),
        }
    }
}

impl std::fmt::Display for ShepTomlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "{}: {source}", path.display()),
            Self::WrongShape { path, key, found } => write!(
                f,
                "{}: [{key}] must be a table, found a {found}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for ShepTomlError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::WrongShape { .. } => None,
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
