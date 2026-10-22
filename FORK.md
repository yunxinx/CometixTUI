# CometixTUI fork

This fork extends [Haleclipse/CometixTUI](https://github.com/Haleclipse/CometixTUI) at `ec7c06155482815a4b4a99be5ad4c62675a632a1` with controlled-input support:

- Keyboard and paste events are dispatched individually. Component state is processed before the next input event, so typing `hi`, Enter, and `x` can submit `hi` and retain `x` as a fresh draft. Input bypasses frame throttling and resize settling, including when a resize arrived earlier in the same throttle window; non-input updates still coalesce.
- `TextInputProps::max_height` caps the visible rows of an auto-growing multiline input. The viewport follows the cursor and resets its scroll offset when content shrinks.
- Keybinding handlers outside a pending chord's contexts leave it for the matching context to resolve.

It also changes how the inline renderer reacts to terminal resizes:

- A resize is painted only after the size has stayed unchanged for 75ms, so a window drag produces one reset instead of one per intermediate size.
- Only a width change resets the inline canvas and purges native scrollback; a height-only change keeps scrollback, and rows pushed above the viewport follow the offscreen rule. When a shorter viewport clips rows below the input cursor, the renderer restores those rows from the previous canvas before diffing, preserving both the cursor baseline and native history.
- `SystemContext::set_scrollback_replay` registers a `ScrollbackReplay`. After every automatic purge (width change or offscreen reset) the renderer writes the canvases it returns for the new width above the live canvas, so applications can keep history older than their retained tree in scrollback.

The regression tests live in `packages/iocraft/src/components/text_input.rs`, `packages/iocraft/src/components/keybinding_provider.rs`, `packages/iocraft/src/render/tests/render_loop.rs` (`test_resize_burst_paints_once_after_settling`, `test_resize_commits_submit_before_next_input`) and `packages/iocraft/src/terminal/tests/mod.rs` (`test_inline_width_resize_replays_scrollback_between_purge_and_canvas`, `test_inline_height_only_resize_keeps_scrollback`, `test_inline_height_shrink_restores_rows_below_cursor`). Consumers should pin a full commit SHA. Upstream MIT/Apache-2.0 licensing is unchanged.
