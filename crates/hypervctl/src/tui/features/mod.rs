//! The panes of the TUI: each owns its view and reads the state, never
//! changes it (design 5.1 and 5.9).

pub mod detail;
pub mod status_bar;
pub mod vm_list;
