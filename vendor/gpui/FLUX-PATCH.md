# gpui 0.2.2 with Flux's patch

This is gpui 0.2.2 from crates.io (`src/`, `build.rs`, `resources/`; the examples, tests and
dev-dependencies are left out of `Cargo.toml`), used by Flux through `[patch.crates-io]` in the
workspace's `Cargo.toml`. Every change is marked in the code with a `Flux patch` comment;
`flux.patch` is the whole difference from the published crate.

## Flux's changes

- **Warnings** (`src/taffy.rs`): `f32` literals in `to_grid_repeat` (two warnings of newer rustc).
- **Interface scale** (part 10.4, `crates/flux-app/src/ui_scale.rs`): the whole window larger or
  smaller, as zooming a web page — Zoom IDE of JetBrains IDEs. gpui 0.2.2 can only change the
  `rem` size, and Flux lays out in pixels.
  - `src/window.rs`: `Window::ui_scale` / `Window::set_ui_scale(scale, cx)`. The window lays out a
    viewport `scale` times smaller than its content size and draws it with the display's scale
    factor times `scale` (`bounds_changed`), so text, SVGs, images, borders and shadows scale
    together and are rasterized sharp at the new size. The pointer keeps its place on the screen.
  - Positions from the platform are in window points: the `on_input` callback divides them by the
    scale (`scale_platform_input`: mouse down / up / move / exit, the scroll wheel's position and its
    pixel delta — line deltas stay — and file drops; keys untouched). Positions given to the
    platform are multiplied: `resize`, `show_window_menu`, the IME position of
    `invalidate_character_coordinates`; in `src/platform.rs` the `PlatformInputHandler` multiplies
    `bounds_for_range` (the input method's candidate window) and divides the point of
    `character_index_for_point`. The window's bounds, the displays and `window_min_size` stay in
    the screen's points.
  - `dispatch_event` itself is not scaled: synthetic events (tests, `simulate_*`) are already in
    the window's pixels.
- **Traffic lights** (macOS): `PlatformWindow::set_traffic_light_position` (default: nothing; mac:
  stores the position and moves the buttons) and `Window::set_traffic_light_position`, in window
  points, as `TitlebarOptions::traffic_light_position` — Flux keeps them in the middle of its title
  bar at any scale. `move_traffic_light` (`src/platform/mac/window.rs`) keeps the buttons inside the
  system title bar: lower, they were cut off and didn't take clicks.
- **Input rate** (2026-10-16, from upstream gpui — zed-industries/zed#45369): gpui 0.2.2 kept a
  window presenting its unchanged scene at the display's rate for a second after *any* input, to
  keep ProMotion from lowering the refresh rate; an idle Flux window presented ~120 frames a second
  with no keyboard or mouse input for minutes (~33–52 % of the M5's GPU). `src/window.rs`:
  `InputRateTracker` — only input that invalidated the window counts (`WindowInvalidator::update_count`,
  compared before and after `dispatch_event`), and only fast input (6 within 100 ms, 60 a second)
  keeps the window presenting, for a second after the last burst.
- **Animation rate** (2026-10-16, `src/elements/animation.rs`): a repeating animation (`repeat()` —
  Flux's pulses that say something runs) asked for every frame, so a pulse redrew the whole window
  at the display's 120 Hz for as long as it showed (a working Claude session: ~23 % of the main
  thread, ~60 % of the GPU). Now it notifies its view on a timer, 30 times a second, on a grid shared
  by all repeating animations (one tick per view, however many pulses it has); the phase still comes
  from the clock. One-shot animations — transitions — keep every frame. `until_next_animation_tick`
  (public) gives the grid to redraws that should land in the same frames: Flux's Claude session
  redraws streamed text on it (`crates/flux-app/src/claude_session.rs`, `redraw_on_tick`).
- **Headless scenario frames** (`crates/flux-app/src/scenario.rs`, `FLUX_SCENARIO_HEADLESS`): a
  locked screen stops the display link of every window, and the window server won't capture
  them. `Window::headless_frame` does what the platform's frame callback does, without presenting
  (next-frame callbacks, `draw` when dirty); `Window::render_to_image` renders the current frame
  offscreen (`PlatformWindow::render_to_image`, default `None`; mac: `MetalRenderer::render_to_image`
  into a texture of the drawable's size, read back as RGBA with premultiplied alpha). For that
  `draw_primitives` and `new_command_encoder` take the target texture instead of the drawable.

- **Quitting from the Dock and at the end of the session** (part 10.5, `crates/flux-app/src/workspace.rs`):
  gpui 0.2.2's app delegate had no `applicationShouldTerminate:`, so Quit in the Dock or its menu,
  logging out, restarting and shutting down terminated Flux at once — unsaved documents were lost.
  - `src/platform.rs`: `Platform::on_quit_requested(callback)` and `Platform::cancel_quit()` (default:
    nothing). `src/app.rs`: `App::on_quit_requested(callback)` (the callback gets `&mut App`; when the
    app is borrowed — a nested run loop inside an update — it is asked again 50 ms later) and
    `App::cancel_quit()`.
  - `src/platform/mac/platform.rs`: the delegate's `applicationShouldTerminate:` returns "now" once
    the app has decided (`Platform::quit` sets `quit_approved`) or without a callback; otherwise it
    sets `quit_pending`, runs the callback as a separate main queue task and returns
    `NSTerminateLater`. AppKit then runs its run loop in `NSModalPanelRunLoopMode`, where the main
    dispatch queue (gpui's tasks, frames, timers) and window events keep working, so Flux's own
    dialogs ask their questions. `Platform::quit` answers a pending request with
    `replyToApplicationShouldTerminate:YES` (a `terminate:` sent during the wait would terminate at
    once, unasked); `Platform::cancel_quit` answers `NO`. Both replies go through the main queue,
    outside any borrow of the app (termination runs `applicationWillTerminate:` → `on_app_quit`).
  - The wait must not start inside a main queue task (the run loop doesn't serve the main queue
    nested in one of its tasks): the system's quit events come from the run loop, and gpui sends
    `terminate:` only after `Platform::quit`. Flux's scenario step `app-terminate` sends it from a run
    loop timer for the same reason.

- **Windows: the title bar and the end of the session** (part 10.9,
  `crates/flux-app/src/title_bar.rs`, `crates/flux-app/src/workspace.rs`).
  - The title bar's top edge: Flux's title bar is the window's caption (`WindowControlArea::Drag`)
    and reaches the window's top edge, where gpui itself answers `WM_NCHITTEST` with `HTTOP` (the
    custom frame has no top border). gpui 0.2.2 answered with the control area first, so the window
    couldn't be resized from its top edge. `src/platform/windows/events.rs` (`handle_hit_test_msg`):
    a drag area now gives way to the resize edges and is the answer only where none of them is; the
    window buttons (`Min`, `Max`, `Close`) still answer first.
  - The end of the session: signing out, restarting and shutting down, Windows ends a process once
    its windows have answered `WM_QUERYENDSESSION` and `WM_ENDSESSION` — with no `WM_CLOSE`, no
    `WM_DESTROY` and no end of gpui's run loop, after which the quit callback runs (`on_app_quit`):
    the state of Flux's windows wasn't saved. `src/platform/windows/events.rs`: `WM_ENDSESSION` with
    `wParam` true (the session does end) sends `WM_GPUI_END_SESSION` to the platform's window;
    `src/platform/windows/platform.rs` (`handle_end_session`) takes the quit callback and runs it —
    once for all the windows, and not again if the run loop ends after all. `WM_QUERYENDSESSION`
    keeps Windows' answer, yes: unlike macOS, the end of the session doesn't ask about unsaved
    documents (Windows waits for an answer only a few seconds, then offers to end the session
    anyway).

- **Motion** (2026-10-28, `crates/flux-app/src/motion.rs`): the author asked for lively, Liquid
  Glass-like animations; gpui 0.2.2 can animate a style (`with_animation`) but can't scale or move
  what an element paints, and its hover and press styles switch at once.
  - `src/style.rs`, `src/styled.rs`: `Style::transform_scale`, `transform_origin` (fractions of the
    element's size, the middle by default) and `transform_translate` — `Styled::transform_scale`,
    `transform_origin`, `transform_translate`. Paint only: layout and hitboxes keep the real
    bounds, so a popover that grows out of its anchor is clickable where it will be.
  - `src/window.rs`: `Window::with_element_scale` / `with_element_transform` keep a uniform
    transform (`p' = p · scale + offset`, nested ones composed) that every `paint_*` applies: the
    bounds of quads, shadows, paths, underlines, glyphs, SVGs, images and surfaces, their corner
    radii, borders, blur and thickness, and the content mask (masks pushed inside scale with the
    content; the mask in effect where the transform began clips in the window's space, as CSS
    clips a transformed element by its ancestors). Glyphs, SVGs and images keep their raster and
    are stretched by the sampler, so an animation doesn't rasterize a new size every frame; a
    rotated SVG's matrix is conjugated into the scaled space. `Interactivity::paint` applies an
    element's transform around its opacity (`with_style_paint_effects`).
  - `src/elements/div.rs`: `set_interaction_transitions` — the hover (`hover`, `group_hover`) and
    press (`active`, `group_active`) styles of elements with an id fade in and out: background
    (solid), border and text colors, opacity, scale and move are mixed between the style without
    and with them (`mix_styles`, straight-alpha colors: a transparent end fades the other's alpha
    only); everything else switches at once, as before. The fade lives in
    `InteractiveElementState::interaction_fades`, so an element without an id switches at once;
    a reversed fade starts from where it was; the release may overshoot (a spring: a pressed button
    springs back past its size). Unset (the default), nothing fades — stock behaviour.
  - `src/window.rs`: `Window::with_inert` — hitboxes inserted inside get an empty content mask
    (the bounds stay for whoever measures with them): a popover fading out after it closed is
    drawn but takes no hover, clicks, wheel or tooltips.
  - `src/elements/div.rs` (2026-10-29): `Interactivity::clear_interaction_styles` drops the hover
    and press styles set so far and the group the element names — an unavailable button neither
    lights up nor springs (`ui::available`).
  - Tooltips (2026-10-29): `set_tooltip_transitions(TooltipTransitions { handoff, exit, easing })`
    (`src/elements/div.rs`; `None`, the default — stock behaviour). A tooltip asked for while
    another one shows, or less than `handoff` after one went because the mouse left its element,
    shows without the 500 ms delay (`handle_tooltip_mouse_move`, `Window::tooltip_warm`); not
    after a click or the wheel hid one (`tooltip_dismissed`). The builder sees
    `Window::tooltip_handoff()` — Flux's tooltip then only fades in. `src/window.rs`: the tooltip
    drawn last frame is kept (`shown_tooltip`); once it is gone it fades out for `exit` where it
    was, inert (`prepaint_leaving_tooltip`, painted with its opacity); a new tooltip, a prompt or
    a drag drops it.

- **Scroll axis** (2026-10-29): a trackpad's swipe down always moves a little sideways too, and every
  scroll container under the pointer takes the event — a plugin board (a row scrolling sideways
  around lanes scrolling down, `crates/flux-app/src/plugin_view.rs`) slid left and right under
  every swipe down. Now a gesture keeps to its predominant axis, as native scroll views do.
  - `src/window.rs`: `ScrollGesture` (public, unit-tested in `plugin_view.rs` — the crate's own
    tests need the dev-dependencies left out), kept by the window and updated in `dispatch_event`
    before the listeners. A trackpad gesture's axis is decided once it has moved 4 px (until then
    each event goes its own way), sideways only when that movement is 1.2 times larger, and kept
    through the gesture and its momentum until `TouchPhase::Started` — or a 300 ms pause, for a
    device with precise deltas but no phases. A mouse wheel's step is a gesture by itself (macOS
    sends Shift+wheel sideways).
  - `src/elements/div.rs` (`paint_scroll_listener`): an element takes only the movement along the
    gesture's axis; with `allow_concurrent_scroll`, all of it. gpui's turning a scroll along the
    other axis into the element's own (a wheel scrolls a tab strip sideways) stays: an element
    that must not do it sets `restrict_scroll_to_axis` (Flux's board row and plugin trees).
    Elements with their own wheel listeners (the editor, the terminal) get the events unchanged.

- **Runtime shaders on Windows** (2026-10-30, part 10.9, `scripts/bundle-windows.sh`): gpui 0.2.2
  compiles its HLSL with `fxc.exe` in `build.rs` for a release build — only when the build script
  itself runs on Windows — and includes the bytecode from `OUT_DIR`; a debug build compiles the
  source files at runtime from the folder gpui was built in. Flux.exe built on a Mac (mingw-w64)
  had no shaders at all, and a CI runner without the Windows SDK's fxc (ARM64) couldn't build.
  Flux enables `runtime_shaders` (for Metal on macOS); now on Windows it means the same.
  - `src/platform/windows/directx_renderer.rs`, `shader_resources::runtime`: with `runtime_shaders`
    a shader is compiled by `D3DCompile` (d3dcompiler_47.dll, a part of Windows 10 and 11) from the
    HLSL embedded with `include_str!` — `shaders.hlsl` and `color_text_raster.hlsl` (the emoji
    pipeline of `direct_write.rs`), with `alpha_correction.hlsl` put in place of their `#include`
    (a source compiled from memory has no folder to include from) — in every profile, optimized
    as fxc's `/O3` in a release build. Each shader is compiled once per process and kept: every
    window's renderer and the text system share it. Without the feature nothing changes.
  - `build.rs`: with `runtime_shaders` the Windows build doesn't run fxc. The `windows-manifest`
    feature is untouched; Flux turns it off and embeds its own manifest
    (`crates/flux-app/build.rs`).
  - The first change outside `src/`: `flux.patch` now takes `build.rs` too (step 3 below).

## Moving to a newer gpui

1. Copy the new crate from `~/.cargo/registry/src/*/gpui-<version>/` (`src/`, `build.rs`,
   `resources/`, the licence, the README) and its `Cargo.toml` without `[[example]]`, `[[test]]`,
   `[[bench]]` and the dev-dependencies.
2. Apply `flux.patch` (`patch -p1 -d vendor/gpui < vendor/gpui/flux.patch`) and fix what doesn't
   apply by the descriptions above; grep the new `window.rs` for new `platform_window.` calls that
   take or give positions — they need the scale too.
3. Regenerate the patch from the crate's directory, `src/` and `build.rs`:
   `diff -ruN ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-<version>/src src > flux.patch`,
   then `diff -uN ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-<version>/build.rs build.rs >> flux.patch`
   (paths relative to `vendor/gpui`).
4. Check by hand: the mouse, scrolling, file drops and the input method (Japanese, Pinyin) at 125 %,
   the traffic lights at 80 % and 125 %; Quit in the Dock with an unsaved document (the question,
   Cancel keeps Flux, Don't Save quits); a swipe down over a plugin board in a narrow tab scrolls a
   lane and doesn't slide the board, a sideways swipe slides it; along a toolbar the next button's
   tooltip shows at once, a tooltip fades out when the mouse leaves, and after a click it waits.
   On Windows: signing out with a project open, then starting Flux, brings the project back with its
   tabs; the window resizes from its top edge and moves by its title bar.
