//! The TUI (design 5): a state machine in `store`, panes that only draw, and
//! one main loop that owns everything.

pub mod app;
pub mod effects;
pub mod keymap;
pub mod store;
pub mod terminal;
pub mod ui;

pub mod features;

#[cfg(test)]
mod snapshot_tests;
