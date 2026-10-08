//! Файлы проекта без UI:
//! - [`rules`] — какие файлы принадлежат проекту: служебные каталоги VCS и мусор не видны,
//!   скрытые — видны, `.gitignore` — по правилам ripgrep ([`project_walker`]); те же правила
//!   у поиска (`flux-search`);
//! - [`list`] — содержимое одного каталога для дерева: каталоги первыми, естественный
//!   порядок имён, игнорируемые помечены ([`list_dir`]);
//! - [`tree`] — состояние дерева файлов: прочитанные и раскрытые каталоги, видимые строки;
//! - [`ops`] — операции над файлами: создать, переименовать, переместить, скопировать,
//!   удалить в Корзину — без перезаписи существующих файлов;
//! - [`watch`] — наблюдение за изменениями на диске ([`Watcher`]).
//!
//! Чтение каталогов и операции блокируют поток — их зовут из фона (`background_spawn` gpui).

pub mod list;
pub mod ops;
pub mod rules;
pub mod tree;
pub mod watch;

pub use list::{DirEntry, EntryKind, compare_names, list_dir};
pub use ops::{copy_into, create_dir, create_file, move_into, remap, rename, trash, validate_name};
pub use rules::{is_skipped, project_walker};
pub use watch::{FsChange, Watcher};
