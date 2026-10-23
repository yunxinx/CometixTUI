# CometixTUI fork

This fork extends [Haleclipse/CometixTUI](https://github.com/Haleclipse/CometixTUI) at `ec7c06155482815a4b4a99be5ad4c62675a632a1` with controlled-input support:

- Keyboard and paste events are dispatched individually. Component state is processed before the next input event, so typing `hi`, Enter, and `x` can submit `hi` and retain `x` as a fresh draft. Input bypasses frame throttling and resize settling, including when a resize arrived earlier in the same throttle window; non-input updates still coalesce.
- `TextInputProps::max_height` caps the visible rows of an auto-growing multiline input. The viewport follows the cursor and resets its scroll offset when content shrinks.
- Keybinding handlers outside a pending chord's contexts leave it for the matching context to resolve.

It also changes how the inline renderer reacts to terminal resizes:

- A resize is painted only after the size has stayed unchanged for 75ms, so a window drag produces one reset instead of one per intermediate size.
- Only a width change resets the inline canvas and purges native scrollback; a height-only change keeps scrollback, and rows pushed above the viewport follow the offscreen rule. When a shorter viewport clips rows below the input cursor, the renderer restores those rows from the previous canvas before diffing, preserving both the cursor baseline and native history.
- `SystemContext::set_scrollback_replay` registers a `ScrollbackReplay`. After every automatic purge (width change or offscreen reset) the renderer writes the canvases it returns for the new width above the live canvas, so applications can keep history older than their retained tree in scrollback.

The renderer never sets the cursor shape: a shown cursor keeps the shape configured in the user's terminal, and exiting has no shape to restore.

The regression tests live in `packages/iocraft/src/components/text_input.rs`, `packages/iocraft/src/components/keybinding_provider.rs`, `packages/iocraft/src/render/tests/render_loop.rs` (`test_resize_burst_paints_once_after_settling`, `test_resize_commits_submit_before_next_input`) and `packages/iocraft/src/terminal/tests/mod.rs` (`test_inline_width_resize_replays_scrollback_between_purge_and_canvas`, `test_inline_height_only_resize_keeps_scrollback`, `test_inline_height_shrink_restores_rows_below_cursor`, `test_cursor_show_and_exit_keep_the_user_cursor_shape`). The [iocraft upstream sync](#iocraft-upstream-sync) below records the subsequent upstream ports and consumer compatibility changes. Consumers should pin a full commit SHA. Upstream MIT/Apache-2.0 licensing is unchanged.

## iocraft upstream sync

This fork has ported [ccbrown/iocraft](https://github.com/ccbrown/iocraft) changes up to `2158a53` on top of the base fork, which last merged `3e26e2b` (v0.9.1). Applicable changes were applied directly or re-implemented in this fork's structure; a closing `-s ours` merge records the sync point. The 33 upstream commits are accounted for below: 18 Ported, 12 Already covered, and 3 Not taken. Code and comment cleanup was incorporated with the corresponding ports.

### Ported

- Required props are checked at compile time with `#[iocraft(required)]`; optional props use each field type's default (`iocraft#228`). Covered by `required_prop` in `packages/iocraft-macros/tests/element.rs` and `props_diagnostics` in `packages/iocraft-macros/tests/ui.rs`, which runs `ui/complete_required_props.rs`, `ui/missing_required_prop.rs`, and `ui/private_builder_fields.rs`. `Newline` still reserves one row by default, covered by `test_newline_default_count_is_one` in `packages/iocraft/src/components/newline.rs`.
- Hooks lifetime errors point to the component's Hooks parameter (`iocraft#240`). Covered by `props_diagnostics` in `packages/iocraft-macros/tests/ui.rs`, using `ui/explicit_hooks_lifetime.rs`.
- Upstream Hooks lifetime diagnostics tests are retained (`iocraft#244`), covered by `props_diagnostics` in `packages/iocraft-macros/tests/ui.rs` and its `ui/explicit_hooks_lifetime.rs` fixture. The same commit's VS16 padding and paste cases are already covered by saturating padding arithmetic and existing paste normalization; temporary probes were removed.
- Covariance helpers omit type and const generic defaults (`iocraft#260`). Covered by `StructWithLifetimeAndDefaultedType` and `StructWithLifetimeAndDefaultedConst` in `packages/iocraft-macros/tests/props.rs`.
- `Self` in props field types and bounds refers to the props struct, while nested items retain their own `Self` (`iocraft#285`). Covered by `RecursiveProps`, `GenericRecursiveProps`, `AssociatedSelfProps`, `ConstSelfProps`, `SelfBoundProps`, and `NestedItemSelfProps` in `packages/iocraft-macros/tests/props.rs`.
- Cloning a `Handler` and converting it into a `HandlerMut` accept payloads without `Clone`, `Send`, or `Sync` (`iocraft#297`). Covered by `test_non_clone_non_send_payload_can_clone_and_convert` in `packages/iocraft/src/handler.rs`.
- Macros resolve renamed Cargo dependencies, including this fork's 14 additional layout fields (`iocraft#301`). Covered by `props_diagnostics` in `packages/iocraft-macros/tests/ui.rs`, using `ui/renamed_dependency.rs`.
- The optional `serde` feature forwards to `crossterm/serde` for the re-exported color and event types, using the same serialization format as iocraft upstream (`iocraft#235`). Covered by `serde_matches_crossterm_format` in `packages/iocraft/src/style.rs`.
- Retaining an async handler no longer keeps its component's tasks alive, and invocations after unmount have no effect (`iocraft#256`). Covered by `test_retained_handler_does_not_outlive_component_tasks` and `test_async_handler_wakes_an_already_waiting_component` in `packages/iocraft/src/hooks/use_async_handler.rs`.
- Async handlers can enqueue another invocation from their factory or future without locking the polling task (`iocraft#263`). Covered by `test_reentrant_async_handlers_complete_pending_parents` in `packages/iocraft/src/hooks/use_async_handler.rs`.
- State changes release their write borrow before waking consumers, allowing consumers to read the new value immediately (`iocraft#294`). Covered by `test_woken_consumer_can_read_changed_state` in `packages/iocraft/src/hooks/use_state.rs`.
- State and ref setters drop replaced values after releasing the write borrow, so destructors can read the replacement (`iocraft#299`). Covered by `test_replaced_value_drop_can_read_replacement` in both `packages/iocraft/src/hooks/use_state.rs` and `packages/iocraft/src/hooks/use_ref.rs`.
- Output formatting, wakers, writers, and acknowledgement callbacks can enqueue output without deadlocking (`iocraft#279`), re-implemented in this fork's `use_output`; draining remains in `post_component_effects`, and messages queued while writing wait for the next drain. Covered by `test_formatting_and_waking_can_enqueue_output` and, with `unstable-output-streams`, `test_backend_output_is_flushed_on_the_next_update` in `packages/iocraft/src/hooks/use_output.rs`.
- Synchronized frame-ending write errors propagate to the render loop, with update errors taking precedence and panic cleanup still ending the update (`iocraft#281`). Covered by `test_end_of_synchronized_update_reports_errors` in `packages/iocraft/src/terminal/tests/mod.rs`.
- Leading Unicode whitespace no longer panics during `Text` or `MixedText` alignment (`iocraft#270`). Covered by `test_alignment_preserves_unicode_indentation` in `packages/iocraft/src/components/text.rs`.
- Wide glyphs crossing an internal clip boundary leave underlying cells unchanged; `SpacerHead` is only written at the canvas's right edge, preventing right-hand siblings from shifting left in terminal output (this fork's variant of `iocraft#241`). Covered by `test_wide_text_clipping_preserves_right_sibling` in `packages/iocraft/src/components/text.rs`, including an `avt` assertion on the sibling's terminal column.
- Windows `TextInput` accepts non-control, non-ASCII-alphanumeric AltGr characters reported as Ctrl+Alt, while retaining Ctrl+Alt letter/digit shortcuts (`iocraft#233`). Covered by the platform-specific `test_is_altgr_char` tests in `packages/iocraft/src/components/text_input.rs`.
- Local mouse hooks ignore events outside the component's visible area, including ancestor clipping; coordinates remain relative to the original, unclipped origin (`iocraft#276`). Covered for both plain and propagated hooks by `test_local_mouse_respects_ancestor_clip_without_moving_origin` in `packages/iocraft/src/hooks/use_terminal_events.rs`.

### Fork fixes made during the sync

- The chalk tmux truecolor guard uses an equivalent boolean expression accepted by Rust 1.94.0 clippy.
- Intra-doc links that pointed at private or unresolved items now resolve, so `cargo doc` passes with `-D warnings` on Rust 1.94.0.
- The FastScrollBox drain regression exits when draining completes and fails after five seconds if it does not. Covered by `test_fast_scroll_box_defaults_to_opt_in_drain` in `packages/iocraft/src/components/scroll_box.rs`; production scroll behavior is unchanged.
- View click and hover targets use the same visible-area calculation as local mouse hooks, retaining unclipped local coordinates and hit testing for visible overflow. Covered by `test_view_click_respects_ancestor_clip_without_moving_origin` in `packages/iocraft/src/components/view.rs`. Ancestor clip changes invalidate Memo retained blits and CachedSubtree snapshots so descendant event regions are refreshed. Zero-height sibling suppression traverses children under an empty clip to keep hidden descendants out of hit testing.

### Breaking changes for consumers

- `element!` initializes optional props using each field type's `Default`; a handwritten `impl Default` on the props struct is no longer used. Fields whose types do not implement `Default` must be marked `#[iocraft(required)]` and supplied by the caller.
- `NewlineProps::count` is now `Option<i32>`. Omitting it still reserves one row, and `element!(Newline(count: 2))` remains valid.
- `UIRouterProps::app` is required, and `UIRouterProps` no longer implements `Default`.

### Already covered

- Zero-width glyphs are not emitted and measure as zero, keeping layout and output aligned without a blank base (`iocraft#242`).
- Button accepts any number of children and lays them out inside a View (`iocraft#236`); no multiple-child panic is needed, and the children documentation reflects this contract.
- Wide-cell relationships are cleared before partial overwrites or erasure, preventing stale wide heads and tails (`iocraft#246`).
- Text extraction skips wide-character tail cells (`iocraft#249`); starting inside a tail retains this fork's selection behavior without an extra leading space.
- Bold and dim are independent attributes, and intensity transitions emit SGR 22 before enabling the next attributes (`iocraft#259`).
- Drawing and forced wrapping preserve grapheme clusters, including joined emoji (`iocraft#264`); a double-width cluster that cannot fit the canvas width is clipped whole, and zero-width suffixes retain this fork's omission behavior.
- Text wrapping preserves leading blank lines (`iocraft#271`).
- Text reuses its measure function for style-only changes; content, wrap mode, and structured-mode changes reinstall it, while `MeasureMemo` caches layout probes (`iocraft#287`). Empty `Text` retains this fork's 0×0 layout rather than reserving a line.
- Single-line paste normalizes LF, CR, and CRLF to spaces, while multiline input preserves line breaks (`iocraft#243`).
- Requested TextInput cursor offsets are clamped to the value length and rounded down to UTF-8 character boundaries (`iocraft#252`); the handle documentation now states this behavior.
- TextInput auto-grow derives layout from current props and text rows, so disabling and re-enabling it restores the height (`iocraft#266`).
- Button uses View's primary-button release-click handling, so right and middle buttons do not activate it (`iocraft#295`); its documentation now specifies the primary button.

### Not taken

- The upstream release commit (`iocraft#230`) is omitted; this fork does not follow upstream versions or CHANGELOG entries.
- The upstream v0.9.3 release commit (`iocraft#245`) is omitted for the same reason.
- The agent guides, contributing guide, and pull request template from `iocraft#306` are not imported; this fork keeps its own contributor workflow. Its code cleanup was incorporated with the corresponding ports.
