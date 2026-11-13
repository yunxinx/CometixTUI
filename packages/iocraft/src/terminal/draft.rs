use crate::Canvas;
use std::any::Any;
use std::sync::Arc;

/// Application-owned source coverage, returned unchanged after a local commit.
#[derive(Clone)]
pub struct SourceCoverage(Arc<dyn Any + Send + Sync>, usize, super::OutputAllocations);

impl SourceCoverage {
    /// Wraps a shared immutable source identity. The terminal never interprets its contents.
    pub fn new<T: Any + Send + Sync>(value: Arc<T>) -> Self {
        Self(value, size_of::<T>(), Default::default())
    }

    /// Declares additional owned storage behind the identity, excluding already budgeted shared sources.
    pub fn with_owned_bytes(mut self, bytes: usize) -> Self {
        self.1 = self
            .1
            .checked_add(bytes)
            .expect("source coverage fits addressable memory");
        self
    }

    pub(crate) fn reserve(&mut self, memory: &Arc<super::OutputMemory>) -> std::io::Result<()> {
        let mut allocations = super::OutputAllocations::default();
        allocations.shared(
            memory,
            (3, Arc::as_ptr(&self.0) as *const () as usize),
            self.1,
        )?;
        self.2 = allocations;
        Ok(())
    }

    /// Reads the identity using the application's original type.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl std::fmt::Debug for SourceCoverage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SourceCoverage")
            .finish_non_exhaustive()
    }
}

/// A half-open screen rectangle and its immutable application-owned hit target.
#[derive(Clone, Debug)]
pub struct HitRegion {
    /// Left screen column.
    pub column: u16,
    /// Top screen row.
    pub row: u16,
    /// Number of screen columns.
    pub width: u16,
    /// Number of screen rows.
    pub height: u16,
    /// Identity to interpret the hit without consulting newer geometry.
    pub target: SourceCoverage,
}

impl HitRegion {
    fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.column
            && u32::from(column) < u32::from(self.column) + u32::from(self.width)
            && row >= self.row
            && u32::from(row) < u32::from(self.row) + u32::from(self.height)
    }
}

/// The finite input projection belonging to a completely written and flushed frame.
#[derive(Clone)]
pub struct CommittedFrame {
    /// Local write receipt; it is not a terminal acknowledgement.
    pub receipt: CommitReceipt,
    pub(crate) hits: Arc<[HitRegion]>,
    pub(crate) _allocations: super::OutputAllocations,
    pub(crate) views: Option<Arc<crate::components::ViewHitProjection>>,
}

impl std::fmt::Debug for CommittedFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommittedFrame")
            .field("receipt", &self.receipt)
            .field("hits", &self.hits)
            .finish_non_exhaustive()
    }
}

impl CommittedFrame {
    pub(crate) fn matching_targets<T: Any>(&self) -> impl Iterator<Item = &T> {
        self.hits
            .iter()
            .rev()
            .filter_map(|region| region.target.get())
    }

    /// Returns the topmost target of a type, including when a drag leaves its rectangle.
    pub fn target<T: Any>(&self) -> Option<&T> {
        self.hits
            .iter()
            .rev()
            .find_map(|region| region.target.get())
    }

    /// Returns the topmost target of the requested type at a screen position.
    pub fn hit<T: Any>(&self, column: u16, row: u16) -> Option<&T> {
        self.hits
            .iter()
            .rev()
            .filter(|region| region.contains(column, row))
            .find_map(|region| region.target.get())
    }
}

/// Confirmation that a complete operation was written and flushed locally.
/// This is not an acknowledgement from the terminal emulator.
#[derive(Clone, Debug)]
pub struct CommitReceipt {
    /// Monotonic sequence within one terminal session.
    pub sequence: u64,
    /// Coverage supplied by the application for this operation.
    pub coverage: Option<SourceCoverage>,
}

/// The result of submitting a replaceable display frame.
pub enum FrameCommit {
    /// The frame and its cursor were completely written and flushed.
    Committed(CommitReceipt),
    /// A newer frame replaced this frame before output started.
    Superseded,
}

/// A finite display target with cursor and immutable image attachments.
pub struct FrameDraft {
    pub(crate) canvas: Canvas,
    pub(super) graphics: super::graphics::GraphicsPlan,
    pub(crate) allocations: super::OutputAllocations,
    pub(crate) coverage: Option<SourceCoverage>,
    pub(crate) alternate: bool,
    pub(crate) paint: bool,
    pub(crate) hits: Vec<HitRegion>,
    pub(crate) views: Option<Arc<crate::components::ViewHitProjection>>,
}

impl FrameDraft {
    /// Takes ownership of a prepared main-screen frame.
    pub fn new(canvas: Canvas) -> Self {
        Self {
            canvas,
            graphics: Default::default(),
            allocations: Default::default(),
            coverage: None,
            alternate: false,
            paint: true,
            hits: Vec::new(),
            views: None,
        }
    }

    /// Supplies finite hit descriptions in paint order, back to front.
    pub fn with_hits(mut self, hits: Vec<HitRegion>) -> Self {
        self.hits = hits;
        self
    }

    /// Targets the alternate screen. Screen changes are ordered with this frame.
    pub fn alternate_screen(mut self) -> Self {
        self.alternate = true;
        self
    }

    /// Associates the frame with application-owned source coverage.
    pub fn with_coverage(mut self, coverage: SourceCoverage) -> Self {
        self.coverage = Some(coverage);
        self
    }
}

/// A finite set of history rows with immutable image attachments.
pub struct HistoryDraft {
    pub(crate) canvas: Canvas,
    pub(super) graphics: super::graphics::GraphicsPlan,
    pub(crate) coverage: Option<SourceCoverage>,
}

impl HistoryDraft {
    /// Takes ownership of prepared rows. Wire image IDs are assigned during output.
    pub fn new(canvas: Canvas) -> Self {
        Self {
            canvas,
            graphics: Default::default(),
            coverage: None,
        }
    }

    /// Associates the rows with application-owned source coverage.
    pub fn with_coverage(mut self, coverage: SourceCoverage) -> Self {
        self.coverage = Some(coverage);
        self
    }
}
