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

## Moving to a newer gpui

1. Copy the new crate from `~/.cargo/registry/src/*/gpui-<version>/` (`src/`, `build.rs`,
   `resources/`, the licence, the README) and its `Cargo.toml` without `[[example]]`, `[[test]]`,
   `[[bench]]` and the dev-dependencies.
2. Apply `flux.patch` (`patch -p1 -d vendor/gpui < vendor/gpui/flux.patch`) and fix what doesn't
   apply by the descriptions above; grep the new `window.rs` for new `platform_window.` calls that
   take or give positions — they need the scale too.
3. Regenerate the patch from the crate's directory:
   `diff -ruN ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-<version>/src src > flux.patch`
   (paths relative to `vendor/gpui`).
4. Check by hand: the mouse, scrolling, file drops and the input method (Japanese, Pinyin) at 125 %,
   the traffic lights at 80 % and 125 %; Quit in the Dock with an unsaved document (the question,
   Cancel keeps Flux, Don't Save quits).
