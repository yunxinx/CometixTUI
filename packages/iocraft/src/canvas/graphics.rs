use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The lifetime of images belonging to one application view or session.
#[derive(Clone, Debug, Default)]
pub struct ImageScope(Arc<AtomicBool>);

impl ImageScope {
    /// Retires this scope. Existing image sources can no longer request presentation.
    pub fn retire(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether images in this scope may still be presented.
    pub fn is_active(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }
}

impl PartialEq for ImageScope {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Immutable image data whose allocation remains charged while any canvas references it.
pub trait CanvasImageSource: std::fmt::Debug + Send + Sync + 'static {
    /// The PNG encoded as standard base64, without line breaks.
    fn encoded_png(&self) -> &str;
    /// The domain lifetime of this image, independent of terminal wire IDs.
    fn scope(&self) -> &ImageScope;
}

/// An image placement before the output owner assigns terminal IDs.
#[derive(Clone, Debug)]
pub struct CanvasImage {
    pub(crate) source: Arc<dyn CanvasImageSource>,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
}

impl CanvasImage {
    /// The shared immutable source retained by this placement.
    pub fn source(&self) -> &Arc<dyn CanvasImageSource> {
        &self.source
    }
    /// Width of the complete placement in terminal columns.
    pub fn columns(&self) -> u16 {
        self.columns
    }

    /// Height of the complete placement in terminal rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Creates a placement in the Unicode-placeholder coordinate range.
    pub fn new(source: Arc<dyn CanvasImageSource>, columns: u16, rows: u16) -> io::Result<Self> {
        if !(1..=256).contains(&columns) || !(1..=256).contains(&rows) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "image placement exceeds placeholder coordinates",
            ));
        }
        Ok(Self {
            source,
            columns,
            rows,
        })
    }
}

impl PartialEq for CanvasImage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
            && self.columns == other.columns
            && self.rows == other.rows
    }
}

/// A clipped image cell retaining coordinates in its complete placement.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasImageCell {
    /// The complete placement and its immutable source.
    pub image: CanvasImage,
    /// Column in the complete placement, before clipping.
    pub column: u16,
    /// Row in the complete placement, before clipping.
    pub row: u16,
}

impl Canvas {
    /// Sets the image lifetime for this frame, including frames with no remaining images.
    pub fn set_image_scope(&mut self, scope: Option<ImageScope>) {
        self.image_scope = scope;
    }

    pub(crate) fn image_scope(&self) -> Option<&ImageScope> {
        self.image_scope.as_ref()
    }
    /// Visits visible image cells with their canvas coordinates and original placement coordinates.
    pub fn image_cells(&self) -> impl Iterator<Item = (usize, usize, &CanvasImageCell)> {
        self.cells.iter().enumerate().flat_map(|(row, cells)| {
            cells.iter().enumerate().filter_map(move |(column, cell)| {
                cell.image.as_ref().map(|image| (column, row, image))
            })
        })
    }

    pub(crate) fn resolve_images(
        &mut self,
        mut resolve: impl FnMut(&CanvasImageCell) -> (String, CanvasTextStyle),
    ) {
        for row in 0..self.height() {
            if !self.cells[row].iter().any(|cell| cell.image.is_some()) {
                continue;
            }
            for cell in self.cells_row_mut(row) {
                if let Some(image) = &cell.image {
                    let (value, style) = resolve(image);
                    cell.character = Some(Character {
                        value,
                        style,
                        hyperlink: cell
                            .character
                            .as_mut()
                            .and_then(|character| character.hyperlink.take()),
                    });
                }
            }
        }
    }
}
