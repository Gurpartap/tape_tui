//! Component registry and identifiers.

use crate::core::component::Component;

/// Stable identifier for a component owned by a single `TuiRuntime` instance.
///
/// Semantics:
/// - IDs are unique within a runtime instance.
/// - IDs are never reused for the lifetime of the runtime instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ComponentId(u64);

impl ComponentId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Default)]
pub struct ComponentRegistry {
    entries: Vec<Option<Box<dyn Component>>>,
    next_id: u64,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_boxed(&mut self, component: Box<dyn Component>) -> ComponentId {
        let id = ComponentId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("component id overflowed u64");
        let idx = id.raw().try_into().expect("component id overflowed usize");
        if self.entries.len() <= idx {
            self.entries.resize_with(idx + 1, || None);
        }
        self.entries[idx] = Some(component);
        id
    }

    pub fn get_mut(&mut self, id: ComponentId) -> Option<&mut Box<dyn Component>> {
        let idx: usize = id.raw().try_into().ok()?;
        self.entries.get_mut(idx).and_then(|entry| entry.as_mut())
    }
}
