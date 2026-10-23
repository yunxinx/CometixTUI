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

## iocraft upstream sync

This fork has ported [ccbrown/iocraft](https://github.com/ccbrown/iocraft) changes up to `2158a53` on top of the base fork, which last merged `3e26e2b` (v0.9.1). Each change was re-implemented in this fork's structure; a closing `-s ours` merge records the sync point.

### Ported

- Required props are checked at compile time with `#[iocraft(required)]`; optional props use each field type's default (`iocraft#228`). Covered by `required_prop` in `packages/iocraft-macros/tests/element.rs` and the `complete_required_props`, `missing_required_prop`, and `private_builder_fields` fixtures in `packages/iocraft-macros/tests/ui/`. `Newline` still reserves one row by default, covered by `test_newline_default_count_is_one` in `packages/iocraft/src/components/newline.rs`.
- Hooks lifetime errors point to the component's Hooks parameter (`iocraft#240`, macro tests from `iocraft#244`). Covered by the `explicit_hooks_lifetime` fixture in `packages/iocraft-macros/tests/ui/`.
- Covariance helpers omit type and const generic defaults (`iocraft#260`). Covered by `StructWithLifetimeAndDefaultedType` and `StructWithLifetimeAndDefaultedConst` in `packages/iocraft-macros/tests/props.rs`.
- `Self` in props field types and bounds refers to the props struct, while nested items retain their own `Self` (`iocraft#285`). Covered by `RecursiveProps`, `GenericRecursiveProps`, `AssociatedSelfProps`, `ConstSelfProps`, `SelfBoundProps`, and `NestedItemSelfProps` in `packages/iocraft-macros/tests/props.rs`.
- Cloning a `Handler` and converting it into a `HandlerMut` accept payloads without `Clone`, `Send`, or `Sync` (`iocraft#297`). Covered by `test_non_clone_non_send_payload_can_clone_and_convert` in `packages/iocraft/src/handler.rs`.
- Macros resolve renamed Cargo dependencies, including this fork's 14 additional layout fields (`iocraft#301`). Covered by the `renamed_dependency` fixture in `packages/iocraft-macros/tests/ui/`.
- The optional `serde` feature forwards to `crossterm/serde` for the re-exported color and event types, using the same serialization format as iocraft upstream (`iocraft#235`). Covered by `serde_matches_crossterm_format` in `packages/iocraft/src/style.rs`.

### Fork fixes made during the sync

### Breaking changes for consumers

- `element!` initializes optional props using each field type's `Default`; a handwritten `impl Default` on the props struct is no longer used. Fields whose types do not implement `Default` must be marked `#[iocraft(required)]` and supplied by the caller.
- `NewlineProps::count` is now `Option<i32>`. Omitting it still reserves one row, and `element!(Newline(count: 2))` remains valid.
- `UIRouterProps::app` is required, and `UIRouterProps` no longer implements `Default`.

### Already covered

### Not taken
