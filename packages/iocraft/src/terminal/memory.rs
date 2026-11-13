use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, Weak};

/// A capacity envelope admitted by the application's resource budget.
/// The retained lease covers output allocations and their unused completion reserve.
pub struct OutputCapacity {
    bytes: usize,
    _lease: Box<dyn Send + Sync>,
}

impl OutputCapacity {
    /// Transfers a reservation for `bytes` to the terminal output lifetime.
    pub fn new(bytes: usize, lease: impl Send + Sync + 'static) -> Self {
        Self {
            bytes,
            _lease: Box::new(lease),
        }
    }
}

impl Default for OutputCapacity {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024, ())
    }
}

struct State {
    used: usize,
    allocations: BTreeMap<(u8, usize), Weak<Allocation>>,
}

pub(crate) struct OutputMemory {
    capacity: OutputCapacity,
    state: Mutex<State>,
}

impl OutputMemory {
    pub(crate) fn new(capacity: OutputCapacity) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            state: Mutex::new(State {
                used: 0,
                allocations: BTreeMap::new(),
            }),
        })
    }

    pub(crate) fn reserve(self: &Arc<Self>, bytes: usize) -> io::Result<Arc<Allocation>> {
        self.allocate(None, bytes)
    }

    pub(crate) fn shared(
        self: &Arc<Self>,
        key: (u8, usize),
        bytes: usize,
    ) -> io::Result<Arc<Allocation>> {
        self.allocate(Some(key), bytes)
    }

    fn allocate(
        self: &Arc<Self>,
        key: Option<(u8, usize)>,
        bytes: usize,
    ) -> io::Result<Arc<Allocation>> {
        let mut state = self.state.lock().expect("output memory is not poisoned");
        if let Some(existing) = key
            .and_then(|key| state.allocations.get(&key))
            .and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        let bytes = bytes.checked_add(256).ok_or_else(capacity_error)?;
        let used = state
            .used
            .checked_add(bytes)
            .filter(|used| *used <= self.capacity.bytes)
            .ok_or_else(capacity_error)?;
        state.used = used;
        let allocation = Arc::new(Allocation {
            memory: self.clone(),
            bytes,
            key,
        });
        if let Some(key) = key {
            state.allocations.insert(key, Arc::downgrade(&allocation));
        }
        Ok(allocation)
    }
}

fn capacity_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        "terminal output capacity exhausted",
    )
}

pub(crate) struct Allocation {
    memory: Arc<OutputMemory>,
    bytes: usize,
    key: Option<(u8, usize)>,
}

impl std::fmt::Debug for Allocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputAllocation")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        let mut state = self
            .memory
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.used -= self.bytes;
        if let Some(key) = self.key
            && state
                .allocations
                .get(&key)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
        {
            state.allocations.remove(&key);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OutputAllocations(pub(crate) Vec<Arc<Allocation>>);

impl OutputAllocations {
    pub(crate) fn reserve(&mut self, memory: &Arc<OutputMemory>, bytes: usize) -> io::Result<()> {
        self.0.push(memory.reserve(bytes)?);
        Ok(())
    }

    pub(crate) fn shared(
        &mut self,
        memory: &Arc<OutputMemory>,
        key: (u8, usize),
        bytes: usize,
    ) -> io::Result<()> {
        self.0.push(memory.shared(key, bytes)?);
        Ok(())
    }
}
