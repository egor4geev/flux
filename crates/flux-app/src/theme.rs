//! Тема — данные: цвета интерфейса и стили областей подсветки. Ставится глобально
//! (`cx.set_global`), читается через [`Theme::get`] и [`Theme::ui`]. Метрики шрифта,
//! табуляция и мигание — константы: это будущие настройки, а не тема.

use std::time::Duration;

use flux_syntax::Highlight;
use gpui::{App, Global, Hsla, rgb, rgba};

pub const FONT_FAMILY: &str = "Menlo";
pub const FONT_SIZE: f32 = 14.;
pub const LINE_HEIGHT: f32 = 21.;
pub const TAB_WIDTH: usize = 4;
/// Отступ текста от гаттера.
pub const TEXT_PADDING: f32 = 8.;
/// Сколько строк держать между курсором и краем окна при автоскролле.
pub const SCROLL_MARGIN_LINES: usize = 3;
/// Период мигания курсора; `None` — курсор не мигает.
pub const CURSOR_BLINK: Option<Duration> = Some(Duration::from_millis(500));

/// Цвета интерфейса. `Copy`: читаются из глобальной темы одним значением.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiColors {
    pub background: Hsla,
    pub foreground: Hsla,
    /// Второстепенный текст: номера строк, статус-бар, неактивные вкладки.
    pub dim: Hsla,
    pub current_line: Hsla,
    pub selection: Hsla,
    pub cursor: Hsla,
    pub status_bar: Hsla,
    pub border: Hsla,
    pub tab_bar: Hsla,
    /// Полоска сверху активной вкладки.
    pub tab_accent: Hsla,
    pub error: Hsla,
    /// Фон панелей и всплывающих окон: строка поиска, поиск по проекту, палитра.
    pub panel: Hsla,
    pub input_background: Hsla,
    pub input_border: Hsla,
    /// Рамка поля ввода в фокусе.
    pub focus_border: Hsla,
    /// Строка списка под мышью и выбранная строка (палитра, поиск файла, результаты поиска).
    pub list_hover: Hsla,
    pub list_selected: Hsla,
    /// Выбранная строка списка без фокуса: файл активной вкладки в дереве файлов.
    pub list_selected_inactive: Hsla,
    /// Каталог, на который сейчас бросят перетаскиваемый файл (дерево файлов).
    pub drop_target: Hsla,
    /// Совпавшие символы в списках: нечёткий поиск, результаты поиска по проекту.
    pub match_text: Hsla,
    /// Фон найденных вхождений в тексте и текущего из них.
    pub search_match: Hsla,
    pub search_match_active: Hsla,
    /// Фон включённого переключателя (Aa, ab, .*).
    pub toggle_active: Hsla,
}

/// Как рисовать область подсветки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntaxStyle {
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub ui: UiColors,
    /// Области подсветки — имена как у capture tree-sitter (`keyword`,
    /// `function.method`). Общее имя покрывает частные: `function` действует
    /// и для `function.method`, если у того нет своей строки.
    pub syntax: Vec<(String, SyntaxStyle)>,
}

impl Global for Theme {}

impl Theme {
    pub fn get(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    pub fn ui(cx: &App) -> UiColors {
        Self::get(cx).ui
    }

    /// Имена областей по порядку — для `flux_syntax::HighlightMap`, индекс в
    /// этом списке и есть [`Highlight`].
    pub fn syntax_scopes(&self) -> Vec<&str> {
        self.syntax
            .iter()
            .map(|(scope, _)| scope.as_str())
            .collect()
    }

    pub fn syntax_style(&self, highlight: Highlight) -> Option<SyntaxStyle> {
        self.syntax.get(highlight.0).map(|(_, style)| *style)
    }

    /// Тёмная тема в палитре GitHub Dark (Primer, «prettylights»). Роли цветов
    /// как на GitHub: ключевые слова и операторы — красный, функции —
    /// фиолетовый, типы и конструкторы — оранжевый, строки — светло-голубой,
    /// константы, числа, свойства и встроенное — голубой, теги, регулярки и
    /// ключи JSON — зелёный, комментарии — серый. Обычные переменные и
    /// пунктуация — цветом текста, чтобы не шуметь; параметры — оранжевым
    /// (цвет `variable` у GitHub), чтобы отличать их от локальных.
    pub fn github_dark() -> Self {
        const RED: u32 = 0xff7b72;
        const PURPLE: u32 = 0xd2a8ff;
        const ORANGE: u32 = 0xffa657;
        const BLUE: u32 = 0x79c0ff;
        const LIGHT_BLUE: u32 = 0xa5d6ff;
        const GREEN: u32 = 0x7ee787;
        const GRAY: u32 = 0x8b949e;
        const TEXT: u32 = 0xc9d1d9;

        let plain = |color: u32| SyntaxStyle {
            color: rgb(color).into(),
            bold: false,
            italic: false,
        };
        let bold = |color: u32| SyntaxStyle {
            bold: true,
            ..plain(color)
        };
        let syntax = [
            ("attribute", plain(BLUE)),
            ("boolean", plain(BLUE)),
            ("comment", plain(GRAY)),
            ("constant", plain(BLUE)),
            ("constructor", plain(ORANGE)),
            // Код внутри `${…}` и f-строк — не строка.
            ("embedded", plain(TEXT)),
            ("escape", plain(BLUE)),
            ("function", plain(PURPLE)),
            ("keyword", plain(RED)),
            ("label", plain(ORANGE)),
            ("number", plain(BLUE)),
            ("operator", plain(RED)),
            ("property", plain(BLUE)),
            ("punctuation", plain(TEXT)),
            ("punctuation.special", plain(RED)),
            ("string", plain(LIGHT_BLUE)),
            ("string.escape", plain(BLUE)),
            ("string.special", plain(GREEN)),
            ("tag", plain(GREEN)),
            ("text.literal", plain(BLUE)),
            ("text.reference", plain(LIGHT_BLUE)),
            ("text.title", bold(BLUE)),
            ("text.uri", plain(LIGHT_BLUE)),
            ("type", plain(ORANGE)),
            ("type.builtin", plain(BLUE)),
            ("variable", plain(TEXT)),
            ("variable.builtin", plain(BLUE)),
            ("variable.parameter", plain(ORANGE)),
        ];
        Self {
            ui: UiColors {
                background: rgb(0x0d1117).into(),
                foreground: rgb(TEXT).into(),
                dim: rgb(0x6e7681).into(),
                current_line: rgb(0x161b22).into(),
                selection: rgba(0x388bfd55).into(),
                cursor: rgb(0x58a6ff).into(),
                status_bar: rgb(0x010409).into(),
                border: rgb(0x21262d).into(),
                tab_bar: rgb(0x010409).into(),
                tab_accent: rgb(0xf78166).into(),
                error: rgb(0xf85149).into(),
                panel: rgb(0x161b22).into(),
                input_background: rgb(0x0d1117).into(),
                input_border: rgb(0x30363d).into(),
                focus_border: rgb(0x1f6feb).into(),
                list_hover: rgba(0xb1bac41f).into(),
                list_selected: rgba(0x388bfd40).into(),
                list_selected_inactive: rgba(0x6e768140).into(),
                drop_target: rgba(0x388bfd26).into(),
                match_text: rgb(0x58a6ff).into(),
                // Как `editor.findMatch*` в теме GitHub Dark для VS Code.
                search_match: rgba(0xf2cc6040).into(),
                search_match_active: rgb(0x9e6a03).into(),
                toggle_active: rgba(0x388bfd66).into(),
            },
            syntax: syntax
                .into_iter()
                .map(|(scope, style)| (scope.to_string(), style))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_syntax::{HighlightMap, languages};

    /// Каждый capture каждого языка находит область темы — свою или более
    /// общую по откату через точки. `none` (служебный в markdown) — не красим.
    #[test]
    fn dark_theme_covers_every_capture() {
        let theme = Theme::github_dark();
        let scopes = theme.syntax_scopes();
        for language in languages() {
            let map = HighlightMap::new(language, &scopes);
            for (i, name) in language.capture_names().iter().enumerate() {
                let highlight = map.get(i as u32);
                if *name == "none" {
                    assert_eq!(highlight, None, "{}", language.name());
                } else {
                    assert!(highlight.is_some(), "{}: @{name}", language.name());
                }
            }
        }
    }

    #[test]
    fn scopes_are_unique_and_styles_line_up() {
        let theme = Theme::github_dark();
        let scopes = theme.syntax_scopes();
        let mut sorted = scopes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), scopes.len());
        let title = scopes.iter().position(|s| *s == "text.title").unwrap();
        assert!(theme.syntax_style(Highlight(title)).unwrap().bold);
        assert_eq!(theme.syntax_style(Highlight(scopes.len())), None);
    }
}
