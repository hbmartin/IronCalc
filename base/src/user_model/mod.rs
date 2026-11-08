#![deny(missing_docs)]

mod autofill;
mod border;
mod border_utils;
mod clipboard;
mod clipboard_plan;
mod common;
mod conditional_formatting;
pub(crate) mod history;
mod links;
mod merged_cells;
mod named_cell_styles;
mod sequence_detector;
mod ui;
mod undo_redo;

pub use common::UserModel;

#[cfg(test)]
pub use ui::SelectedView;

pub use clipboard::ClipboardData;
pub use common::BorderArea;

#[cfg(test)]
#[path = "../test/test_clipboard_follow_up.rs"]
mod clipboard_follow_up_tests;
mod display;
#[cfg(test)]
#[path = "../test/test_display_replay.rs"]
mod display_replay_tests;

#[cfg(test)]
#[path = "../test/test_clipboard_replay.rs"]
mod clipboard_replay_tests;

#[cfg(test)]
#[path = "../test/test_formula_replay_follow_up.rs"]
mod formula_replay_follow_up_tests;

#[cfg(test)]
#[path = "../test/test_sparse_replay.rs"]
mod sparse_replay_tests;
