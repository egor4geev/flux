//! Тема — данные: цвета интерфейса и стили областей подсветки. Ставится глобально
//! (`cx.set_global`), читается через [`Theme::get`] и [`Theme::ui`]. Метрики шрифта,
//! табуляция и мигание — константы: это будущие настройки, а не тема.
//!
//! Цвета интерфейса — токены дизайн-системы (вики: «Design System»): поверхности стекла
//! (рамка окна, острова, всплывающие панели), текст трёх уровней, акцент, состояния и
//! палитра оттенков для смысла (типы файлов, категории, счётчики). Компоненты — в `ui.rs`.

use std::sync::OnceLock;
use std::time::Duration;

use flux_syntax::Highlight;
use gpui::{App, Global, Hsla, rgb, rgba};

/// Шрифт интерфейса — системный (SF Pro на macOS).
pub const UI_FONT: &str = ".SystemUIFont";
/// Шрифт кода — первый установленный из списка ([`init_fonts`]).
const CODE_FONTS: [&str; 4] = [
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "SF Mono",
    "Menlo",
];
pub const FONT_SIZE: f32 = 14.;
pub const LINE_HEIGHT: f32 = 21.;
pub const TAB_WIDTH: usize = 4;
/// Отступ текста от гаттера.
pub const TEXT_PADDING: f32 = 8.;
/// Сколько строк держать между курсором и краем окна при автоскролле.
pub const SCROLL_MARGIN_LINES: usize = 3;
/// Период мигания курсора; `None` — курсор не мигает.
pub const CURSOR_BLINK: Option<Duration> = Some(Duration::from_millis(500));

/// Кегли интерфейса (шрифт [`UI_FONT`]).
pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 12.;
pub const TEXT_MD: f32 = 13.;
pub const TEXT_LG: f32 = 15.;
pub const TEXT_DISPLAY: f32 = 34.;

static CODE_FONT: OnceLock<&'static str> = OnceLock::new();

/// Выбирает шрифт кода: первый установленный из [`CODE_FONTS`]. Без такой проверки
/// gpui молча подставил бы пропорциональный системный шрифт.
pub fn init_fonts(cx: &App) {
    let installed = cx.text_system().all_font_names();
    let family = CODE_FONTS
        .into_iter()
        .find(|family| installed.iter().any(|name| name == family))
        .unwrap_or("Menlo");
    CODE_FONT.set(family).ok();
}

/// Шрифт кода: редактор, поля поиска, строки результатов.
pub fn code_font() -> &'static str {
    CODE_FONT.get().copied().unwrap_or("Menlo")
}

/// Цвета интерфейса. `Copy`: читаются из глобальной темы одним значением.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiColors {
    // --- Поверхности (стекло). Альфа — сквозь неё видно размытый рабочий стол. ---
    /// Рамка окна: фон под островами, шапка и статус-бар.
    pub frame: Hsla,
    /// Цветной отсвет рамки (градиент из верхнего левого угла).
    pub frame_glow: Hsla,
    /// Остров — самостоятельная панель: дерево, редактор.
    pub island: Hsla,
    pub island_border: Hsla,
    /// Блик стекла по верхнему краю острова и всплывающей панели.
    pub sheen: Hsla,
    /// Всплывающие панели: меню, списки выбора, поиск по проекту, подсказки.
    pub elevated: Hsla,
    pub elevated_border: Hsla,
    pub shadow: Hsla,
    /// Тонкие разделители внутри островов.
    pub divider: Hsla,

    // --- Текст ---
    pub foreground: Hsla,
    /// Вторичный текст: пути, подписи, неактивные вкладки.
    pub text_muted: Hsla,
    /// Третичный: номера строк, подсказки, плейсхолдеры.
    pub dim: Hsla,
    pub text_disabled: Hsla,

    // --- Взаимодействие ---
    pub accent: Hsla,
    /// Акцент для текста и значков на тёмном (светлее основного).
    pub accent_text: Hsla,
    /// Подложка акцента: включённый переключатель, значок действия.
    pub accent_soft: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    /// Выбранная строка списка в фокусе.
    pub list_selected: Hsla,
    /// Выбранная строка без фокуса: файл активной вкладки в дереве.
    pub list_selected_inactive: Hsla,
    pub input_background: Hsla,
    pub input_border: Hsla,
    /// Рамка поля в фокусе и кольцо вокруг него.
    pub focus_border: Hsla,
    pub focus_ring: Hsla,
    /// Каталог, на который сейчас бросят перетаскиваемый файл (дерево файлов).
    pub drop_target: Hsla,
    /// Клавиша в подсказке сочетания.
    pub keycap: Hsla,
    pub keycap_border: Hsla,

    // --- Состояния ---
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub info: Hsla,
    /// Несохранённые изменения: точка на вкладке, отметка в статус-баре.
    pub modified: Hsla,

    // --- Редактор ---
    pub current_line: Hsla,
    pub selection: Hsla,
    pub cursor: Hsla,
    /// Совпавшие символы в списках: нечёткий поиск, результаты поиска по проекту.
    pub match_text: Hsla,
    /// Фон найденных вхождений в тексте и текущего из них.
    pub search_match: Hsla,
    pub search_match_active: Hsla,

    // --- Палитра оттенков: смысл, а не украшение (типы файлов, категории, счётчики). ---
    pub blue: Hsla,
    pub indigo: Hsla,
    pub violet: Hsla,
    pub pink: Hsla,
    pub red: Hsla,
    pub orange: Hsla,
    pub amber: Hsla,
    pub lime: Hsla,
    pub green: Hsla,
    pub teal: Hsla,
    pub cyan: Hsla,
    /// Значок каталога.
    pub folder: Hsla,
}

impl UiColors {
    /// Цвет `color` с непрозрачностью `alpha` — подложки оттенков (бейджи, плитки значков).
    pub fn tint(color: Hsla, alpha: f32) -> Hsla {
        Hsla { a: alpha, ..color }
    }
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

    /// Тёмная тема «Flux Night»: стеклянная рамка и острова в холодных сине-фиолетовых
    /// нейтралях, акцент — индиго. Подсветка кода — палитра GitHub Dark (Primer,
    /// «prettylights»): ключевые слова и операторы — красный, функции — фиолетовый, типы и
    /// конструкторы — оранжевый, строки — светло-голубой, константы, числа, свойства и
    /// встроенное — голубой, теги, регулярки и ключи JSON — зелёный, комментарии — серый.
    /// Обычные переменные и пунктуация — цветом текста, чтобы не шуметь; параметры —
    /// оранжевым (цвет `variable` у GitHub), чтобы отличать их от локальных.
    pub fn flux_night() -> Self {
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

        // Оттенки подобраны под тёмное стекло: близкая светлота, разный тон.
        const INDIGO: u32 = 0x8590ff;
        const AMBER: u32 = 0xffc560;
        Self {
            ui: UiColors {
                frame: rgba(0x07090fc7).into(),
                frame_glow: rgba(0x8590ff24).into(),
                island: rgba(0x0d1018e6).into(),
                island_border: rgba(0xffffff14).into(),
                sheen: rgba(0xffffff2e).into(),
                elevated: rgba(0x171b28fa).into(),
                elevated_border: rgba(0xffffff1f).into(),
                shadow: rgba(0x00000080).into(),
                divider: rgba(0xffffff12).into(),

                foreground: rgb(0xe6e9f2).into(),
                text_muted: rgb(0xa3abc3).into(),
                dim: rgb(0x6b7391).into(),
                text_disabled: rgb(0x4a516a).into(),

                accent: rgb(INDIGO).into(),
                accent_text: rgb(0xaab2ff).into(),
                accent_soft: rgba(0x8590ff2e).into(),
                hover: rgba(0xffffff0f).into(),
                pressed: rgba(0xffffff17).into(),
                list_selected: rgba(0x8590ff3d).into(),
                list_selected_inactive: rgba(0xffffff14).into(),
                input_background: rgba(0x00000052).into(),
                input_border: rgba(0xffffff17).into(),
                focus_border: rgba(0x8590ffd9).into(),
                focus_ring: rgba(0x8590ff3d).into(),
                drop_target: rgba(0x8590ff29).into(),
                keycap: rgba(0xffffff0f).into(),
                keycap_border: rgba(0xffffff1a).into(),

                success: rgb(0x4fd18b).into(),
                warning: rgb(AMBER).into(),
                error: rgb(0xff6b6b).into(),
                info: rgb(0x5aa9ff).into(),
                modified: rgb(0xffb35c).into(),

                current_line: rgba(0xffffff0a).into(),
                selection: rgba(0x8590ff4d).into(),
                cursor: rgb(0xaab2ff).into(),
                match_text: rgb(0xaab2ff).into(),
                search_match: rgba(0xffc56038).into(),
                search_match_active: rgba(0xffc5608f).into(),

                blue: rgb(0x5aa9ff).into(),
                indigo: rgb(INDIGO).into(),
                violet: rgb(0xb48cff).into(),
                pink: rgb(0xff7eb6).into(),
                red: rgb(0xff6b6b).into(),
                orange: rgb(0xff9c5b).into(),
                amber: rgb(AMBER).into(),
                lime: rgb(0xb8e06a).into(),
                green: rgb(0x4fd18b).into(),
                teal: rgb(0x3cd3c4).into(),
                cyan: rgb(0x5ccfff).into(),
                folder: rgb(0x7fa8ff).into(),
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
        let theme = Theme::flux_night();
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
        let theme = Theme::flux_night();
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
