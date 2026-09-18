//! Operator-preference resolution: `shep.toml`'s `[interpreters]` table, the
//! style level in force and which layer chose it, and the hard rule that
//! forces bare rendering.

use cli::GlobalArgs;

use crate::home::resolve_paths;
use crate::{cli, style};

/// Parses `shep.toml`'s `[interpreters]` table into an extension to
/// interpreter map, for `lifecycle::start` to fold onto every resolved app
/// whose own `interpreter` is still unset. Precedence: `shep.toml`, then a
/// Flockfile's own field, then `--interpreter`.
///
/// Empty covers every way this layer can say nothing, a file that will not
/// parse included: `shep start` must still start a script by path while an
/// operator is mid-edit.
pub(crate) fn interpreters_from_config(
    shep_toml: Option<&str>,
) -> std::collections::BTreeMap<String, String> {
    shep_core::config::DaemonConfig::load(shep_toml, &|_| None)
        .map(|cfg| cfg.interpreters)
        .unwrap_or_default()
}

pub(crate) fn style_from_config(shep_toml: Option<&str>) -> Option<style::StyleLevel> {
    shep_core::config::DaemonConfig::load(shep_toml, &|_| None)
        .ok()?
        .style
        .level
        .and_then(|raw| style::StyleLevel::parse(&raw))
}

/// Resolves the level in force and which layer chose it: `--style`, then
/// `$SHEP_STYLE`, then `shep.toml`'s `[style] level`, then `full`.
///
/// Reads `shep.toml` via [`resolve_paths`] rather than
/// [`crate::home::ensure_home`], so
/// `--style` still works with no `$SHEP_HOME` resolvable and nothing here
/// creates a directory. An unreadable `shep.toml` reads as an empty config.
///
/// Unforced: the hard rule that `--format json` or a piped stdout means
/// [`style::StyleLevel::Bare`] is applied in [`crate::entry::run_argv`], so
/// `shep style`'s
/// report says what is configured.
pub(crate) fn resolve_style(global: &GlobalArgs) -> (style::StyleLevel, style::StyleSource) {
    let config_text = resolve_paths(global)
        .ok()
        .and_then(|paths| std::fs::read_to_string(paths.daemon_config).ok());
    style::resolve(
        global.style,
        std::env::var("SHEP_STYLE").ok().as_deref(),
        style_from_config(config_text.as_deref()),
    )
}

/// Whether a level [`crate::cli::Commands::Style`]'s set form just wrote to
/// `shep.toml`
/// is actually the level that will run.
///
/// Only `Flag` and `Env` can say no: they are the two layers
/// [`style::resolve`] puts above `shep.toml`, and so the two spellings an
/// operator needs named when the write keeps being overridden. `Config` is
/// the value this call just wrote, and `Default` cannot follow a write.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn style_write_is_overridden(source: style::StyleSource) -> bool {
    matches!(source, style::StyleSource::Flag | style::StyleSource::Env)
}

/// The hard rule: piped output and `--format json` render with no boxes, no
/// colour and no sheep, whatever `--style`/`$SHEP_STYLE`/`shep.toml` asked
/// for. `shep completions` writes shell a stray escape would execute as code.
///
/// Terminal-ness is a parameter: the real `is_terminal()` call happens once,
/// in [`crate::entry::run_argv`].
pub(crate) fn must_render_bare(stdout_is_terminal: bool, fmt: cli::Format) -> bool {
    !stdout_is_terminal || fmt == cli::Format::Json
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn must_render_bare_is_true_exactly_for_a_piped_stdout_or_a_json_format() {
        assert!(
            !must_render_bare(true, cli::Format::Table),
            "a real terminal asking for a table gets to render one"
        );
        assert!(
            must_render_bare(false, cli::Format::Table),
            "piped stdout must render bare even under --format table"
        );
        assert!(
            must_render_bare(true, cli::Format::Json),
            "--format json must render bare even at a real terminal"
        );
        assert!(must_render_bare(false, cli::Format::Json));
    }

    #[test]
    fn style_from_config_reads_the_level_and_is_lenient_about_everything_else() {
        assert_eq!(
            style_from_config(Some("[style]\nlevel = \"plain\"\n")),
            Some(style::StyleLevel::Plain)
        );
        assert_eq!(style_from_config(None), None, "no file at all");
        assert_eq!(style_from_config(Some("")), None, "an empty file");
        assert_eq!(
            style_from_config(Some("[style")),
            None,
            "a file that will not parse"
        );
        assert_eq!(
            style_from_config(Some("[daemon]\nlog_level = \"info\"\n")),
            None,
            "a config with no [style] table at all"
        );
        assert_eq!(
            style_from_config(Some("[style]\nlevel = \"loud\"\n")),
            None,
            "a level this build does not recognise"
        );
    }

    /// Both must go through [`style::StyleLevel::parse`] rather than
    /// `clap::ValueEnum::from_str`, which does not trim.
    #[test]
    fn style_from_config_trims_the_same_way_shep_style_does() {
        for raw in ["full", " full ", "\tfull\n", "FULL", " FuLl "] {
            assert_eq!(
                style_from_config(Some(&format!("[style]\nlevel = {raw:?}\n"))),
                Some(style::StyleLevel::Full),
                "shep.toml's own level must accept {raw:?} exactly as \
                 $SHEP_STYLE would"
            );
            assert_eq!(
                style::resolve(None, Some(raw), None),
                (style::StyleLevel::Full, style::StyleSource::Env),
                "$SHEP_STYLE must accept {raw:?}"
            );
        }
    }

    /// About the wiring of the flag and a real file into `style::resolve`,
    /// not about the precedence rule itself, which `style.rs` pins.
    #[test]
    fn resolve_style_reads_the_flag_and_the_real_shep_toml_it_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shep.toml"), "[style]\nlevel = \"plain\"\n").unwrap();
        let global = cli::GlobalArgs {
            home: Some(dir.path().to_path_buf()),
            format: cli::Format::Table,
            quiet: false,
            style: None,
        };
        assert_eq!(
            resolve_style(&global),
            (style::StyleLevel::Plain, style::StyleSource::Config),
            "with no flag, shep.toml's own level answers"
        );

        let global = cli::GlobalArgs {
            style: Some(style::StyleLevel::Bare),
            ..global
        };
        assert_eq!(
            resolve_style(&global),
            (style::StyleLevel::Bare, style::StyleSource::Flag),
            "the flag wins over the very shep.toml that set plain above"
        );
    }

    #[test]
    fn style_write_is_overridden_only_by_flag_or_env() {
        assert!(style_write_is_overridden(style::StyleSource::Flag));
        assert!(style_write_is_overridden(style::StyleSource::Env));
        assert!(!style_write_is_overridden(style::StyleSource::Config));
        assert!(!style_write_is_overridden(style::StyleSource::Default));
    }
}
