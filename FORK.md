# CometixTUI fork

This fork extends [Haleclipse/CometixTUI](https://github.com/Haleclipse/CometixTUI) at `ec7c06155482815a4b4a99be5ad4c62675a632a1` with controlled-input support:

- Keyboard and paste events are dispatched individually. Component state is processed before the next input event, so typing `hi`, Enter, and `x` can submit `hi` and retain `x` as a fresh draft. Input bypasses frame throttling; non-input updates still coalesce.
- `TextInputProps::max_height` caps the visible rows of an auto-growing multiline input. The viewport follows the cursor and resets its scroll offset when content shrinks.
- Keybinding handlers outside a pending chord's contexts leave it for the matching context to resolve.

The regression tests live in `packages/iocraft/src/components/text_input.rs` and `packages/iocraft/src/components/keybinding_provider.rs`. Consumers should pin a full commit SHA. Upstream MIT/Apache-2.0 licensing is unchanged.
