use super::*;

/// Represents a writeable region of a [`Canvas`]. All coordinates provided to functions of this
/// type are relative to the subview origin and clipped to the subview and canvas bounds.
pub struct CanvasSubviewMut<'a> {
    pub(super) x: isize,
    pub(super) y: isize,
    pub(super) clip_x: isize,
    pub(super) clip_y: isize,
    pub(super) clip_width: usize,
    pub(super) clip_height: usize,
    pub(super) canvas: &'a mut Canvas,
}

// Absolute, half-open bounds. Keep arithmetic widened until intersection is complete.
struct ClipBounds {
    left: i128,
    top: i128,
    right: i128,
    bottom: i128,
}

impl CanvasSubviewMut<'_> {
    // A clip can extend beyond isize even though its intersection fits on the canvas.
    fn clip_bounds(&self) -> ClipBounds {
        let width = self.canvas.width() as i128;
        let height = self.canvas.height() as i128;
        let left = (self.clip_x as i128).clamp(0, width);
        let top = (self.clip_y as i128).clamp(0, height);
        ClipBounds {
            left,
            top,
            right: (self.clip_x as i128 + self.clip_width as i128).clamp(left, width),
            bottom: (self.clip_y as i128 + self.clip_height as i128).clamp(top, height),
        }
    }

    // Translate a relative rectangle and intersect it before narrowing to canvas coordinates.
    fn clipped_rect(&self, x: isize, y: isize, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let clip = self.clip_bounds();
        let left = self.x as i128 + x as i128;
        let top = self.y as i128 + y as i128;
        let start_x = left.clamp(clip.left, clip.right);
        let start_y = top.clamp(clip.top, clip.bottom);
        let end_x = (left + w as i128).clamp(start_x, clip.right);
        let end_y = (top + h as i128).clamp(start_y, clip.bottom);
        (
            start_x as usize,
            start_y as usize,
            (end_x - start_x) as usize,
            (end_y - start_y) as usize,
        )
    }

    fn clipped_point(&self, x: isize, y: isize) -> Option<(usize, usize)> {
        let (x, y, w, h) = self.clipped_rect(x, y, 1, 1);
        (w > 0 && h > 0).then_some((x, y))
    }

    /// Returns a reference to a cell at the given **relative** subview position.
    ///
    /// Returns `None` if the resulting absolute position is out of bounds or
    /// outside the clip region.
    pub fn cell(&self, x: isize, y: isize) -> Option<&CanvasCell> {
        let (x, y) = self.clipped_point(x, y)?;
        self.canvas.cell(x, y)
    }

    /// Extracts plain text from a rectangular region using **relative** subview
    /// coordinates. The region is clipped to both the clip bounds and the canvas bounds.
    pub fn get_text(&self, x: isize, y: isize, w: usize, h: usize) -> String {
        let (x, y, w, h) = self.clipped_rect(x, y, w, h);
        self.canvas.get_text(x, y, w, h)
    }

    /// Fills the region with the given color.
    pub fn set_background_color(&mut self, x: isize, y: isize, w: usize, h: usize, color: Color) {
        let (x, y, w, h) = self.clipped_rect(x, y, w, h);
        if w == 0 || h == 0 {
            return;
        }
        self.canvas.set_background_color(x, y, w, h, color);
    }

    /// Removes text from the region without touching overlay/damage metadata.
    pub fn clear_text(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let (x, y, w, h) = self.clipped_rect(x, y, w, h);
        if w == 0 || h == 0 {
            return;
        }
        self.canvas.clear_text(x, y, w, h);
    }

    /// Clears cells/styles/hyperlinks/overlays in this subview and marks the
    /// affected rectangle damaged.
    ///
    /// This is the component-local counterpart to [`Canvas::clear_region`],
    /// mirroring CC Ink's `screen.clearRegion(...)` operation. Coordinates are
    /// relative to the subview and clipped to its clip rect before wide-glyph
    /// boundary repair and damage calculation are delegated to the root canvas.
    pub fn clear_region(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let (x, y, w, h) = self.clipped_rect(x, y, w, h);
        if w == 0 || h == 0 {
            return;
        }
        self.canvas.clear_region(x, y, w, h);
    }

    /// Marks a rectangular region as excluded from fullscreen text selection.
    /// Coordinates are relative to the subview and clipped to the subview's clip bounds.
    pub fn mark_no_select_region(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let (x, y, w, h) = self.clipped_rect(x, y, w, h);
        if w == 0 || h == 0 {
            return;
        }
        self.canvas.mark_no_select_region(x, y, w, h);
    }

    /// Marks a relative row as a soft-wrap continuation of the previous row.
    /// `prev_content_end` is relative to this subview and is translated to an
    /// absolute canvas column and clamped to the canvas width before being stored.
    pub fn mark_soft_wrap_continuation(&mut self, y: isize, prev_content_end: usize) {
        let (_, y, _, h) = self.clipped_rect(0, y, 0, 1);
        if h == 0 {
            return;
        }
        let abs_prev_content_end = (self.x as i128 + prev_content_end as i128)
            .clamp(0, self.canvas.width() as i128) as usize;
        self.canvas
            .mark_soft_wrap_continuation(y, abs_prev_content_end);
    }

    /// Copies a rectangular region from `src` into this subview.
    ///
    /// This is useful for custom retained-screen components that produce an
    /// offscreen [`Canvas`] with metadata such as selection overlays,
    /// `noSelect`, and `softWrap`, then blit it into the component's allocated
    /// layout box. Coordinates are clipped to both the subview clip rect and the
    /// source canvas. Copied cells are marked damaged so terminal diff writers
    /// repaint post-render overlays even when the underlying text is unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_from(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
    ) {
        self.blit_region_from_impl(src, dst_x, dst_y, src_x, src_y, width, height, true);
    }

    /// Copies a rectangular region from `src` without marking terminal-output damage.
    ///
    /// This is the clean-blit counterpart to [`Self::blit_region_from`]. Use it
    /// only when the restored cells are known to match the previous terminal
    /// frame; otherwise the terminal writer may skip a repaint that is required
    /// to repair stale physical output.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_from_clean(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
    ) {
        self.blit_region_from_impl(src, dst_x, dst_y, src_x, src_y, width, height, false);
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_region_from_impl(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
        mark_damage: bool,
    ) {
        if width == 0 || height == 0 || src_x >= src.width() || src_y >= src.height() {
            return;
        }

        let (dst_left, dst_top, copy_width, copy_height) =
            self.clipped_rect(dst_x, dst_y, width, height);
        if copy_width == 0 || copy_height == 0 {
            return;
        }
        // Clipping moves the destination right/down; move the source by the same amount.
        let shifted_source = |src: usize, dst: usize, origin: i128| {
            usize::try_from(src as i128 + (dst as i128 - origin)).unwrap_or(usize::MAX)
        };
        let src_left = shifted_source(src_x, dst_left, self.x as i128 + dst_x as i128);
        let src_top = shifted_source(src_y, dst_top, self.y as i128 + dst_y as i128);
        let copy_width = copy_width.min(src.width().saturating_sub(src_left));
        let copy_height = copy_height.min(src.height().saturating_sub(src_top));
        if copy_width == 0 || copy_height == 0 {
            return;
        }
        let clip_right = self.clip_bounds().right as usize;

        // Subview blits copy from arbitrary sources (e.g. ScrollBox moving
        // viewport content), so the destination rows genuinely change and are
        // always marked written — independent of the damage flag, which only
        // controls repaint wake-up.
        self.canvas
            .mark_rows_written_range(dst_top, dst_top.saturating_add(copy_height));
        let mut damage_width = copy_width;

        for row_offset in 0..copy_height {
            let src_row = src_top + row_offset;
            let dst_row = dst_top + row_offset;
            let src_right = src_left + copy_width;
            let dst_right = dst_left + copy_width;

            self.canvas.cells_row_mut(dst_row)[dst_left..dst_right]
                .clone_from_slice(&src.cells[src_row][src_left..src_right]);
            self.canvas.overlays_row_mut(dst_row)[dst_left..dst_right]
                .clone_from_slice(&src.overlays[src_row][src_left..src_right]);
            self.canvas.no_select_row_mut(dst_row)[dst_left..dst_right]
                .clone_from_slice(&src.no_select[src_row][src_left..src_right]);

            let src_soft_wrap = src.soft_wrap[src_row];
            self.canvas.soft_wrap[dst_row] = if src_soft_wrap > 0 {
                let translated = if src_soft_wrap <= src_left {
                    dst_left
                } else {
                    dst_left + src_soft_wrap.saturating_sub(src_left)
                };
                translated.min(self.canvas.width())
            } else {
                0
            };

            if src_right < src.width()
                && dst_right < self.canvas.width()
                && dst_right < clip_right
                && src.cells[src_row][src_right - 1].cell_width == CellWidth::Wide
            {
                self.canvas.cells_row_mut(dst_row)[dst_right] = CanvasCell {
                    cell_width: CellWidth::WidthTail,
                    ..Default::default()
                };
                self.canvas.overlays_row_mut(dst_row)[dst_right] = None;
                damage_width = damage_width.max(copy_width + 1);
            }
        }

        if mark_damage {
            self.canvas.mark_damage(DamageRegion {
                x: dst_left,
                y: dst_top,
                width: damage_width,
                height: copy_height,
            });
        }
    }

    /// Declares the physical cursor position at the given **relative** subview position.
    /// Out-of-bounds or outside-clip positions are silently ignored.
    /// See [`Canvas::declare_cursor`].
    pub fn declare_cursor(&mut self, x: isize, y: isize, visible: bool) {
        let Some((x, y)) = self.clipped_point(x, y) else {
            return;
        };
        self.canvas.declare_cursor(x, y, visible);
    }

    /// Sets a style overlay on a cell at the given **relative** subview position.
    /// Out-of-bounds or outside-clip positions are silently ignored.
    pub fn set_overlay(&mut self, x: isize, y: isize, overlay: StyleOverlay) {
        let Some((x, y)) = self.clipped_point(x, y) else {
            return;
        };
        self.canvas.set_overlay(x, y, overlay);
    }

    pub(crate) fn row_is_visible(&self, y: isize) -> bool {
        let clip = self.clip_bounds();
        let row = self.y as i128 + y as i128;
        clip.left < clip.right && row >= clip.top && row < clip.bottom
    }

    pub(crate) fn set_text_row_with_styles(
        &mut self,
        y: isize,
        text: &str,
        style: CanvasTextStyle,
        style_at: impl FnMut(usize) -> Option<StyleOverlay>,
    ) {
        if !self.row_is_visible(y) {
            return;
        }
        let clip = self.clip_bounds();
        let row = (self.y as i128 + y as i128) as usize;
        self.canvas.set_text_row_str_clipped(
            self.x,
            row,
            clip.left as isize,
            (clip.right - 1) as isize,
            text,
            style,
            None,
            style_at,
        );
    }

    /// Writes text to the region.
    pub fn set_text(&mut self, x: isize, y: isize, text: &str, style: CanvasTextStyle) {
        self.set_text_with_link(x, y, text, style, None);
    }

    /// Writes text to the region, optionally wrapping it in an OSC 8 hyperlink.
    pub fn set_text_with_link(
        &mut self,
        x: isize,
        y: isize,
        text: &str,
        style: CanvasTextStyle,
        hyperlink: Option<&str>,
    ) {
        let clip = self.clip_bounds();
        // Text starting left of the clip may enter it; text starting right of it cannot.
        let Ok(x) = isize::try_from(self.x as i128 + x as i128) else {
            return;
        };
        if clip.left == clip.right || clip.top == clip.bottom || x as i128 >= clip.right {
            return;
        }
        let min_x = clip.left as isize;
        let max_x = (clip.right - 1) as isize;
        let start_y = self.y as i128 + y as i128;
        for (offset, line) in text.lines().enumerate() {
            let y = start_y + offset as i128;
            if y >= clip.bottom {
                break;
            }
            if y >= clip.top {
                self.canvas.set_text_row_str_clipped(
                    x,
                    y as usize,
                    min_x,
                    max_x,
                    line,
                    style,
                    hyperlink,
                    |_| None,
                );
            }
        }
    }
}
