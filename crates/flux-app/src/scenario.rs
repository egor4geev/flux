//! Сценарии проверки UI (фича `scenario`, в обычную сборку не входит).
//!
//! Агент не может нажимать клавиши в окне: нет прав Accessibility. Поэтому нажатия
//! подаются изнутри приложения тем же путём, что и настоящие: `Window::dispatch_keystroke`
//! проходит через keymap и контексты клавиш, а печатные символы — через input handler.
//!
//! - `FLUX_SCENARIO` — шаги через пробел:
//!   - `cmd-s`, `enter`, `space`, `left` — нажатие в синтаксисе keymap gpui;
//!   - `type:текст` — набрать текст по символу (пробел — отдельным шагом `space`);
//!   - `wait:500` — пауза в мс;
//!   - `shot:имя` — напечатать `SHOT имя` и подождать, пока окно снимут снаружи.
//!
//!   После последнего шага печатается `END`.
//! - `FLUX_ANSWERS=0,1` — системные диалоги заменяются автоответчиком: номера кнопок
//!   по порядку; когда ответы кончились — последняя кнопка (обычно Cancel). Каждый
//!   диалог печатает `PROMPT вопрос -> ответ` и метку `SHOT prompt-N`.
//!
//! Диалоги выбора файла (Cmd+O, «Сохранить как») — системные панели, автоответчик их
//! не заменяет. Обвязка со скриншотами — `scripts/ui-scenario.sh`.
//!
//! Окно сценария — поверх всех окон ([`window_options`]): перекрытое другим приложением
//! окно gpui не перерисовывает, и снимки вышли бы устаревшими, пока автор работает рядом.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AsyncApp, Context, EventEmitter, FocusHandle, Focusable, Keystroke,
    Modifiers, PromptButton, PromptHandle, PromptLevel, PromptResponse, Render,
    RenderablePromptHandle, Window, WindowKind, WindowOptions, div, prelude::*, rgb,
};

/// Перед первым шагом: окно открылось, файлы из командной строки прочитаны.
const START_DELAY: Duration = Duration::from_millis(1500);
/// После шага: окно успевает перерисоваться.
const STEP_PAUSE: Duration = Duration::from_millis(150);
/// На метке `shot:` и на диалоге: снаружи успевают снять окно.
const SHOT_PAUSE: Duration = Duration::from_millis(1500);

static ANSWERS: Mutex<VecDeque<usize>> = Mutex::new(VecDeque::new());
static PROMPTS: AtomicUsize = AtomicUsize::new(0);

/// С `FLUX_SCENARIO` окно — всплывающая панель поверх всех окон: её не перекроет
/// приложение, в котором работает автор, и кадры рисуются.
pub fn window_options(options: WindowOptions) -> WindowOptions {
    if std::env::var_os("FLUX_SCENARIO").is_none() {
        return options;
    }
    WindowOptions {
        kind: WindowKind::PopUp,
        ..options
    }
}

pub fn run(window: AnyWindowHandle, cx: &mut App) {
    if let Ok(answers) = std::env::var("FLUX_ANSWERS") {
        let answers = answers
            .split(',')
            .filter_map(|a| a.trim().parse::<usize>().ok());
        ANSWERS.lock().unwrap().extend(answers);
        cx.set_prompt_builder(auto_prompt);
    }
    let Ok(scenario) = std::env::var("FLUX_SCENARIO") else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(START_DELAY).await;
        for step in scenario.split_whitespace() {
            let pause = run_step(step, window, cx);
            cx.background_executor().timer(pause).await;
        }
        println!("END");
    })
    .detach();
}

/// Выполняет шаг и возвращает паузу после него.
fn run_step(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    if let Some(name) = step.strip_prefix("shot:") {
        println!("SHOT {name}");
        return SHOT_PAUSE;
    }
    if let Some(ms) = step.strip_prefix("wait:") {
        return Duration::from_millis(ms.parse().unwrap_or(0));
    }
    let keystrokes = match step.strip_prefix("type:") {
        Some(text) => text.chars().map(typed).collect(),
        None => match Keystroke::parse(step) {
            Ok(keystroke) => vec![keystroke],
            Err(err) => {
                println!("BAD STEP {step}: {err}");
                return STEP_PAUSE;
            }
        },
    };
    let mut handled = true;
    for keystroke in keystrokes {
        handled &= window
            .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
            .unwrap_or(false);
    }
    println!(
        "KEY {step} -> {}",
        if handled { "handled" } else { "ignored" }
    );
    STEP_PAUSE
}

/// Нажатие, которое печатает символ `c`, — как ввод с клавиатуры без модификаторов.
fn typed(c: char) -> Keystroke {
    Keystroke {
        modifiers: Modifiers::default(),
        key: c.to_lowercase().to_string(),
        key_char: Some(c.to_string()),
    }
}

/// Плашка вместо системного диалога: на скриншоте видно вопрос и выбранный ответ.
struct AutoPrompt {
    focus_handle: FocusHandle,
    text: String,
}

impl EventEmitter<PromptResponse> for AutoPrompt {}

impl Focusable for AutoPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AutoPrompt {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .track_focus(&self.focus_handle)
                    .p_4()
                    .bg(rgb(0x30363d))
                    .text_color(rgb(0xffffff))
                    .child(self.text.clone()),
            )
    }
}

fn auto_prompt(
    _: PromptLevel,
    message: &str,
    _: Option<&str>,
    buttons: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let last = buttons.len().saturating_sub(1);
    let answer = ANSWERS
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(last)
        .min(last);
    let label = buttons
        .get(answer)
        .map_or("?", |button| button.label().as_ref());
    let number = PROMPTS.fetch_add(1, Ordering::Relaxed) + 1;
    println!("PROMPT {message} -> {label}");
    println!("SHOT prompt-{number}");
    let text = format!("{message} → {label}");
    let view = cx.new(|cx| {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHOT_PAUSE).await;
            this.update(cx, |_, cx| cx.emit(PromptResponse(answer)))
                .ok();
        })
        .detach();
        AutoPrompt {
            focus_handle: cx.focus_handle(),
            text,
        }
    });
    handle.with_view(view, window, cx)
}
