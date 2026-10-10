//! The operations that are running right now, one per VM: what spins in the
//! list and what disables the menu (spec 3.5, design 5.3).

use std::collections::HashMap;

use hypervctl_core::VmId;

/// The operations in flight, keyed by VM (design 5.3).
#[derive(Debug, Default)]
pub struct OpsState {
    in_flight: HashMap<VmId, String>,
}

impl OpsState {
    /// Marks `vm` as busy with the operation named `label`.
    pub fn start(&mut self, vm: VmId, label: String) {
        self.in_flight.insert(vm, label);
    }

    /// Marks the operation of `vm` as finished.
    pub fn finish(&mut self, vm: VmId) {
        self.in_flight.remove(&vm);
    }

    /// Whether `vm` has an operation in flight (design 5.7).
    pub fn in_flight(&self, vm: VmId) -> bool {
        self.in_flight.contains_key(&vm)
    }

    /// The label of the operation of `vm`, for the spinner row (spec 3.5).
    pub fn label(&self, vm: VmId) -> Option<&str> {
        self.in_flight.get(&vm).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }
}
