//! The output contract (design 4.4): one view per command, rendered as a
//! table, as JSON or as CSV.

pub mod csv;
pub mod format;
#[cfg(test)]
mod golden_tests;
pub mod json;
pub mod table;
pub mod views;

use std::io::IsTerminal;

use serde::Serialize;
use serde_json::Value;

use hypervctl_core::{HyperVError, RawError};

use crate::cli::args::{ColorChoice, Format, GlobalArgs};

use self::table::Cell;

/// Everything a renderer needs to know about where it is writing (design 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub format: Format,
    /// `--no-header`
    pub no_header: bool,
    /// `--no-trunc`: identifiers in full, names never cut short
    pub no_trunc: bool,
    /// `--color`, `NO_COLOR` and the terminal, already decided
    pub color: bool,
    /// Terminal width to stay inside; `None` disables name truncation
    pub max_width: Option<usize>,
}

/// A list as the renderers see it: ordered for output, and shaped the way the
/// table wants to print it (design 4.4).
pub trait ListTable {
    /// Column headers, in display order.
    const HEADERS: &'static [&'static str];

    /// The rows exactly as the table shows them, indentation included.
    fn table_rows(&self, opts: &RenderOptions) -> Vec<Vec<Cell>>;
}

/// One record shown as key/value pairs (design 4.4).
pub trait DetailTable {
    fn detail_rows(&self, opts: &RenderOptions) -> Vec<(String, Cell)>;
}

/// Decides colour and truncation from the flags, the terminal and the
/// environment (design 4.4).
pub fn options(global: &GlobalArgs) -> RenderOptions {
    build(
        global,
        std::io::stdout().is_terminal(),
        terminal_width(),
        std::env::var_os("NO_COLOR").is_none(),
    )
}

fn terminal_width() -> Option<u16> {
    crossterm::terminal::size().ok().map(|(columns, _)| columns)
}

fn build(
    global: &GlobalArgs,
    is_tty: bool,
    width: Option<u16>,
    colour_allowed: bool,
) -> RenderOptions {
    let color = match global.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_tty && colour_allowed,
    };
    let max_width = if is_tty && !global.no_trunc {
        width.map(usize::from)
    } else {
        None
    };
    RenderOptions {
        format: global.output,
        no_header: global.no_header,
        no_trunc: global.no_trunc,
        color,
        max_width,
    }
}

/// Renders a list in the requested format (design 4.4).
pub fn render_list<L>(list: &L, opts: &RenderOptions) -> Result<String, HyperVError>
where
    L: ListTable + Serialize,
{
    match opts.format {
        Format::Table => Ok(table::grid(L::HEADERS, &list.table_rows(opts), opts)),
        Format::Json => json::render(list),
        Format::Csv => Ok(csv::render(&to_value(list)?, opts.no_header)),
    }
}

/// Renders one record in the requested format (design 4.4).
pub fn render_detail<D>(detail: &D, opts: &RenderOptions) -> Result<String, HyperVError>
where
    D: DetailTable + Serialize,
{
    match opts.format {
        Format::Table => Ok(table::key_value(&detail.detail_rows(opts), opts)),
        Format::Json => json::render(detail),
        Format::Csv => Ok(csv::render(&to_value(detail)?, opts.no_header)),
    }
}

fn to_value<T>(value: &T) -> Result<Value, HyperVError>
where
    T: ?Sized + Serialize,
{
    serde_json::to_value(value).map_err(|error| {
        HyperVError::backend(RawError::new(format!(
            "could not encode the output: {error}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(color: ColorChoice) -> GlobalArgs {
        GlobalArgs {
            output: Format::Table,
            no_header: false,
            no_trunc: false,
            color,
            yes: false,
            verbose: 0,
        }
    }

    #[test]
    fn a_piped_output_is_neither_coloured_nor_truncated() {
        let opts = build(&global(ColorChoice::Auto), false, Some(120), true);
        assert!(!opts.color);
        assert_eq!(opts.max_width, None);
    }

    #[test]
    fn a_terminal_output_is_coloured_and_fits_the_width() {
        let opts = build(&global(ColorChoice::Auto), true, Some(120), true);
        assert!(opts.color);
        assert_eq!(opts.max_width, Some(120));
    }

    #[test]
    fn colour_can_be_forced_off_and_on() {
        assert!(!build(&global(ColorChoice::Never), true, Some(80), true).color);
        assert!(build(&global(ColorChoice::Always), false, None, false).color);
    }

    #[test]
    fn no_colour_env_disables_auto_but_not_always() {
        assert!(!build(&global(ColorChoice::Auto), true, Some(80), false).color);
        assert!(build(&global(ColorChoice::Always), true, Some(80), false).color);
    }

    #[test]
    fn no_trunc_disables_the_width_even_on_a_terminal() {
        let mut opts = global(ColorChoice::Auto);
        opts.no_trunc = true;
        assert_eq!(build(&opts, true, Some(120), true).max_width, None);
        // A terminal that will not say how wide it is cannot be fitted to.
        assert_eq!(
            build(&global(ColorChoice::Auto), true, None, true).max_width,
            None
        );
    }

    #[test]
    fn no_header_survives_the_render_options() {
        let mut opts = global(ColorChoice::Auto);
        opts.no_header = true;
        assert!(build(&opts, false, None, true).no_header);
    }
}
