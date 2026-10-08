//! The editor core: text, selections, transactions, history.
//!
//! Knows nothing about the UI. All positions are character (`char`) indices in the document.

pub mod document;
pub mod edit;
pub mod history;
pub mod movement;
pub mod selection;
pub mod text;
pub mod transaction;

pub use document::{Document, EditKind, TextChange};
pub use ropey::{Rope, RopeSlice};
pub use selection::{Range, Selection};
pub use transaction::{Assoc, ChangeSet, Transaction};
