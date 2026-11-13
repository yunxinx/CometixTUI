use super::*;
use crate::{CanvasImage, CanvasImageCell, CanvasTextStyle, ImageScope};
use std::collections::BTreeMap;
use std::hash::BuildHasher;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PlacementKey {
    source: usize,
    columns: u16,
    rows: u16,
}

impl PlacementKey {
    fn new(image: &CanvasImage) -> Self {
        Self {
            source: Arc::as_ptr(&image.source) as *const () as usize,
            columns: image.columns,
            rows: image.rows,
        }
    }
}

struct Presentation {
    _image: CanvasImage,
    id: u32,
    wanted: bool,
    committed: bool,
    _allocation: Arc<Allocation>,
}

struct ScreenImages {
    next_id: u32,
    first_id: u32,
    retired: Option<(u32, u32)>,
    hot: BTreeMap<PlacementKey, Presentation>,
    scope: Option<ImageScope>,
    pending_id: Option<u32>,
}

impl ScreenImages {
    fn new(first_id: u32) -> Self {
        Self {
            next_id: first_id,
            first_id,
            retired: None,
            hot: BTreeMap::new(),
            scope: None,
            pending_id: None,
        }
    }

    fn retire_scope(&mut self) {
        if self.scope.as_ref().is_some_and(|scope| !scope.is_active()) {
            if self.next_id != self.first_id {
                self.retired = Some((
                    self.retired.map_or(self.first_id, |(cursor, _)| cursor),
                    self.next_id,
                ));
            }
            self.first_id = self.next_id;
            self.scope = None;
            self.hot.clear();
        }
    }

    fn drain_retired(&mut self, output: &mut dyn Write) -> io::Result<()> {
        if let Some((mut cursor, end)) = self.retired {
            for _ in 0..16 {
                if cursor == end {
                    break;
                }
                delete(output, cursor)?;
                cursor = cursor
                    .checked_add(2)
                    .expect("retired range ends within allocated IDs");
            }
            self.retired = (cursor != end).then_some((cursor, end));
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct GraphicsPlan {
    allocations: OutputAllocations,
    presentations: Vec<Arc<Allocation>>,
}

impl GraphicsPlan {
    pub(super) fn reserve(canvas: &Canvas, memory: &Arc<OutputMemory>) -> io::Result<Self> {
        let allocations = canvas.reserve_output(memory, true)?;
        let mut sources = std::collections::BTreeSet::new();
        let mut presentations = Vec::new();
        for (_, _, cell) in canvas.image_cells() {
            if sources.insert(PlacementKey::new(&cell.image)) {
                presentations.push(memory.reserve(512)?);
            }
        }
        Ok(Self {
            allocations,
            presentations,
        })
    }
}

pub(super) struct Graphics {
    screens: [ScreenImages; 2],
    memory: Arc<OutputMemory>,
}

impl Graphics {
    pub(super) fn new(memory: Arc<OutputMemory>) -> Self {
        let seed = std::collections::hash_map::RandomState::new().hash_one(()) as u32;
        let start = (seed & 0x7fff_fffe) | 0x0100_0000;
        Self {
            screens: [ScreenImages::new(start), ScreenImages::new(start + 1)],
            memory,
        }
    }

    pub(super) fn prepare(
        &mut self,
        canvas: &Canvas,
        alternate: bool,
        output: &mut dyn Write,
        mut plan: GraphicsPlan,
    ) -> io::Result<Canvas> {
        let screen = &mut self.screens[usize::from(alternate)];
        let mut scope = canvas.image_scope().cloned();
        for (_, _, cell) in canvas.image_cells() {
            if cell.image.source.scope().is_active() {
                if scope
                    .as_ref()
                    .is_some_and(|scope| scope != cell.image.source.scope())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a frame has one image scope",
                    ));
                }
                scope = Some(cell.image.source.scope().clone());
            }
        }
        if let (Some(current), Some(next)) = (&screen.scope, &scope)
            && current.is_active()
            && current != next
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retire the previous image scope before replacing it",
            ));
        }
        screen.retire_scope();
        if scope.is_some() {
            screen.scope = scope;
        }
        for presentation in screen.hot.values_mut() {
            presentation.wanted = false;
        }
        for (_, _, cell) in canvas.image_cells() {
            if !cell.image.source.scope().is_active() {
                continue;
            }
            let image = &cell.image;
            let key = PlacementKey::new(image);
            if let Some(presentation) = screen.hot.get_mut(&key) {
                presentation.wanted = true;
                continue;
            }
            let allocation = plan
                .presentations
                .pop()
                .expect("admission reserves every possible presentation");
            let id = screen.next_id;
            screen.next_id = id.checked_add(2).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "terminal image ID space exhausted",
                )
            })?;
            screen.pending_id = Some(id);
            let mut chunks = image
                .source
                .encoded_png()
                .as_bytes()
                .chunks(4096)
                .peekable();
            let mut first = true;
            while let Some(chunk) = chunks.next() {
                let more = usize::from(chunks.peek().is_some());
                if first {
                    write!(output, "\x1b_Ga=t,f=100,t=d,i={id},q=2,m={more};")?;
                } else {
                    write!(output, "\x1b_Gm={more};")?;
                }
                output.write_all(chunk)?;
                output.write_all(b"\x1b\\")?;
                first = false;
            }
            write!(
                output,
                "\x1b_Ga=p,U=1,i={id},p={},c={},r={},q=2\x1b\\",
                placement_id(image),
                image.columns,
                image.rows
            )?;
            screen.hot.insert(
                key,
                Presentation {
                    _image: image.clone(),
                    id,
                    wanted: true,
                    committed: false,
                    _allocation: allocation,
                },
            );
            screen.pending_id = None;
        }
        let mut resolved = canvas.clone();
        resolved.output_allocations = plan.allocations;
        resolved.resolve_images(
            |cell| match screen.hot.get(&PlacementKey::new(&cell.image)) {
                Some(presentation) => placeholder(cell, presentation.id),
                None => (" ".into(), CanvasTextStyle::default()),
            },
        );
        Ok(resolved)
    }

    pub(super) fn append_archival(
        &mut self,
        canvas: &Canvas,
        output: &mut dyn Write,
        plan: Option<GraphicsPlan>,
    ) -> io::Result<()> {
        let plan = match plan {
            Some(plan) => plan,
            None => GraphicsPlan::reserve(canvas, &self.memory)?,
        };
        // The live frame's bindings must survive a history batch inserted above it.
        // Archived bindings are released locally after flush; their IDs stay in the scope range.
        let live = std::mem::take(&mut self.screens[0].hot);
        let result = (|| {
            let resolved = self.prepare(canvas, false, output, plan)?;
            resolved.write_ansi(&mut *output)?;
            output.flush()
        })();
        if result.is_ok() {
            self.screens[0].hot = live;
        } else {
            self.screens[0].hot.extend(live);
        }
        result
    }

    pub(super) fn commit(&mut self, alternate: bool, output: &mut dyn Write) -> io::Result<()> {
        let screen = &mut self.screens[usize::from(alternate)];
        for presentation in screen.hot.values_mut() {
            if presentation.wanted {
                presentation.committed = true;
            } else if alternate {
                delete(output, presentation.id)?;
            }
        }
        screen.hot.retain(|_, presentation| presentation.wanted);
        screen.drain_retired(output)
    }

    pub(super) fn retirement_pending(&self, _alternate: bool) -> bool {
        self.screens.iter().any(|screen| {
            screen.retired.is_some()
                || (screen
                    .scope
                    .as_ref()
                    .is_some_and(|scope| !scope.is_active())
                    && (screen.next_id != screen.first_id || !screen.hot.is_empty()))
        })
    }

    pub(super) fn drain_retired(
        &mut self,
        _alternate: bool,
        output: &mut dyn Write,
    ) -> io::Result<()> {
        for screen in &mut self.screens {
            screen.retire_scope();
            screen.drain_retired(output)?;
        }
        Ok(())
    }

    pub(super) fn invalidate(&mut self) {
        for screen in &mut self.screens {
            screen.hot.clear();
            screen.pending_id = None;
        }
    }

    pub(super) fn cleanup(&mut self, alternate: bool, output: &mut dyn Write) -> io::Result<()> {
        let screen = &mut self.screens[usize::from(alternate)];
        let mut result = Ok(());
        if let Some(id) = screen.pending_id.take() {
            result = result.and(delete(output, id));
        }
        for presentation in screen.hot.values() {
            if alternate || !presentation.committed {
                result = result.and(delete(output, presentation.id));
            }
        }
        result
    }

    pub(super) fn leave_alternate(&mut self, output: &mut dyn Write) -> io::Result<()> {
        let screen = &mut self.screens[1];
        for presentation in screen.hot.values() {
            delete(output, presentation.id)?;
        }
        screen.hot.clear();
        Ok(())
    }
}

fn delete(output: &mut dyn Write, id: u32) -> io::Result<()> {
    write!(output, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}
fn placement_id(image: &CanvasImage) -> u32 {
    (u32::from(image.columns) << 9) | u32::from(image.rows)
}
fn color(id: u32) -> crate::Color {
    crate::Color::Rgb {
        r: (id >> 16) as u8,
        g: (id >> 8) as u8,
        b: id as u8,
    }
}
fn placeholder(cell: &CanvasImageCell, id: u32) -> (String, CanvasTextStyle) {
    let mut text = String::with_capacity(16);
    text.push('\u{10eeee}');
    text.push(DIACRITICS[usize::from(cell.row)]);
    text.push(DIACRITICS[usize::from(cell.column)]);
    text.push(DIACRITICS[(id >> 24) as usize]);
    let style = CanvasTextStyle {
        color: Some(color(id)),
        underline_color: Some(color(placement_id(&cell.image))),
        ..CanvasTextStyle::default()
    };
    (text, style)
}

// Kitty's wire-format index table: https://sw.kovidgoyal.net/kitty/graphics-protocol/
// gen/rowcolumn-diacritics.txt; indices are protocol values, not interchangeable accents.
const DIACRITICS: [char; 256] = [
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059c}', '\u{059d}', '\u{059e}', '\u{059f}', '\u{05a0}', '\u{05a1}',
    '\u{05a8}', '\u{05a9}', '\u{05ab}', '\u{05ac}', '\u{05af}', '\u{05c4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065a}', '\u{065b}', '\u{065d}', '\u{065e}', '\u{06d6}', '\u{06d7}', '\u{06d8}',
    '\u{06d9}', '\u{06da}', '\u{06db}', '\u{06dc}', '\u{06df}', '\u{06e0}', '\u{06e1}', '\u{06e2}',
    '\u{06e4}', '\u{06e7}', '\u{06e8}', '\u{06eb}', '\u{06ec}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073a}', '\u{073d}', '\u{073f}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074a}', '\u{07eb}', '\u{07ec}', '\u{07ed}', '\u{07ee}',
    '\u{07ef}', '\u{07f0}', '\u{07f1}', '\u{07f3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081b}', '\u{081c}', '\u{081d}', '\u{081e}', '\u{081f}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082a}', '\u{082b}', '\u{082c}',
    '\u{082d}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0f82}', '\u{0f83}', '\u{0f86}', '\u{0f87}',
    '\u{135d}', '\u{135e}', '\u{135f}', '\u{17dd}', '\u{193a}', '\u{1a17}', '\u{1a75}', '\u{1a76}',
    '\u{1a77}', '\u{1a78}', '\u{1a79}', '\u{1a7a}', '\u{1a7b}', '\u{1a7c}', '\u{1b6b}', '\u{1b6d}',
    '\u{1b6e}', '\u{1b6f}', '\u{1b70}', '\u{1b71}', '\u{1b72}', '\u{1b73}', '\u{1cd0}', '\u{1cd1}',
    '\u{1cd2}', '\u{1cda}', '\u{1cdb}', '\u{1ce0}', '\u{1dc0}', '\u{1dc1}', '\u{1dc3}', '\u{1dc4}',
    '\u{1dc5}', '\u{1dc6}', '\u{1dc7}', '\u{1dc8}', '\u{1dc9}', '\u{1dcb}', '\u{1dcc}', '\u{1dd1}',
    '\u{1dd2}', '\u{1dd3}', '\u{1dd4}', '\u{1dd5}', '\u{1dd6}', '\u{1dd7}', '\u{1dd8}', '\u{1dd9}',
    '\u{1dda}', '\u{1ddb}', '\u{1ddc}', '\u{1ddd}', '\u{1dde}', '\u{1ddf}', '\u{1de0}', '\u{1de1}',
    '\u{1de2}', '\u{1de3}', '\u{1de4}', '\u{1de5}', '\u{1de6}', '\u{1dfe}', '\u{20d0}', '\u{20d1}',
    '\u{20d4}', '\u{20d5}', '\u{20d6}', '\u{20d7}', '\u{20db}', '\u{20dc}', '\u{20e1}', '\u{20e7}',
    '\u{20e9}', '\u{20f0}', '\u{2cef}', '\u{2cf0}', '\u{2cf1}', '\u{2de0}', '\u{2de1}', '\u{2de2}',
    '\u{2de3}', '\u{2de4}', '\u{2de5}', '\u{2de6}', '\u{2de7}', '\u{2de8}', '\u{2de9}', '\u{2dea}',
    '\u{2deb}', '\u{2dec}', '\u{2ded}', '\u{2dee}', '\u{2def}', '\u{2df0}', '\u{2df1}', '\u{2df2}',
    '\u{2df3}', '\u{2df4}', '\u{2df5}', '\u{2df6}', '\u{2df7}', '\u{2df8}', '\u{2df9}', '\u{2dfa}',
    '\u{2dfb}', '\u{2dfc}', '\u{2dfd}', '\u{2dfe}', '\u{2dff}', '\u{a66f}', '\u{a67c}', '\u{a67d}',
    '\u{a6f0}', '\u{a6f1}', '\u{a8e0}', '\u{a8e1}', '\u{a8e2}', '\u{a8e3}', '\u{a8e4}', '\u{a8e5}',
];
