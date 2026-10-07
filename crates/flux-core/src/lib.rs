//! Ядро редактора: текст, выделения, транзакции, история.
//!
//! Ничего не знает про UI. Все позиции — индексы символов (`char`) в документе.

pub mod document;
pub mod edit;
pub mod history;
pub mod movement;
pub mod selection;
pub mod text;
pub mod transaction;

pub use document::{Document, EditKind};
pub use ropey::{Rope, RopeSlice};
pub use selection::{Range, Selection};
pub use transaction::{Assoc, ChangeSet, Transaction};
