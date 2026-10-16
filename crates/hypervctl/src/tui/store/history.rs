//! The per-VM sample history of the Monitor tab (spec 3.9, design 5.12).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use hypervctl_core::{VmId, VmSummary};

/// How many samples one VM keeps: 300 × the default 2 s refresh is ten
/// minutes (spec 3.9).
pub const RING_CAPACITY: usize = 300;

/// One list result as the Monitor tab saw it (design 5.12).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: Instant,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
}

/// The samples of one VM, oldest first, capped at [`RING_CAPACITY`].
#[derive(Debug, Clone, Default)]
pub struct Ring {
    samples: VecDeque<Sample>,
}

impl Ring {
    /// Appends `sample`, dropping the oldest one once the ring is full
    /// (design 5.12).
    pub fn push(&mut self, sample: Sample) {
        if self.samples.len() == RING_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// The samples from the oldest to the newest.
    pub fn iter(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    /// The newest sample, which marks the right edge of the graph (design
    /// 5.12).
    pub fn newest(&self) -> Option<&Sample> {
        self.samples.back()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// The history of every VM the list has shown; it lives only as long as the
/// TUI does (spec 3.9).
#[derive(Debug, Default)]
pub struct History {
    rings: HashMap<VmId, Ring>,
}

impl History {
    /// Records one list result for every VM in it, and drops the rings of the
    /// VMs that left the list (spec 3.9, design 5.12).
    pub fn record(&mut self, at: Instant, items: &[VmSummary]) {
        let present: HashSet<VmId> = items.iter().map(|vm| vm.id).collect();
        self.rings.retain(|id, _| present.contains(id));
        for vm in items {
            self.rings.entry(vm.id).or_default().push(Sample {
                at,
                cpu_percent: vm.cpu_usage_percent,
                memory_bytes: vm.memory_assigned_bytes,
            });
        }
    }

    /// The samples of `vm`, when the list has ever shown it.
    pub fn ring(&self, vm: VmId) -> Option<&Ring> {
        self.rings.get(&vm)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hypervctl_core::VmState;
    use uuid::Uuid;

    use super::*;

    fn vm(index: u128, cpu: Option<f64>) -> VmSummary {
        VmSummary {
            id: VmId::new(Uuid::from_u128(index << 96)),
            name: format!("vm-{index}"),
            state: VmState::Running,
            cpu_usage_percent: cpu,
            memory_assigned_bytes: Some(1024),
            uptime: None,
            generation: None,
        }
    }

    #[test]
    fn the_ring_keeps_the_last_three_hundred_samples() {
        // Anomaly 31: the ring stops at its capacity and evicts the oldest.
        let mut ring = Ring::default();
        let base = Instant::now();
        for index in 0..(RING_CAPACITY + 1) {
            ring.push(Sample {
                at: base + Duration::from_secs(index as u64),
                cpu_percent: Some(index as f64),
                memory_bytes: None,
            });
        }
        assert_eq!(ring.iter().count(), RING_CAPACITY);
        let first = ring
            .iter()
            .next()
            .unwrap_or_else(|| unreachable!("the ring is full"));
        assert_eq!(first.cpu_percent, Some(1.0), "sample 0 was evicted");
        assert_eq!(
            ring.newest().and_then(|sample| sample.cpu_percent),
            Some(300.0)
        );
    }

    #[test]
    fn recording_drops_the_rings_of_vms_that_left_the_list() {
        // Anomaly 31: a VM that disappears from the list takes its samples
        // with it, while the others keep theirs (spec 3.9).
        let mut history = History::default();
        let base = Instant::now();
        let both = vec![vm(1, Some(10.0)), vm(2, None)];
        history.record(base, &both);
        history.record(base + Duration::from_secs(2), &both);
        let survivor = both[0].id;
        let gone = both[1].id;
        history.record(base + Duration::from_secs(4), &[both[0].clone()]);
        assert!(history.ring(gone).is_none());
        assert_eq!(
            history.ring(survivor).map(|ring| ring.iter().count()),
            Some(3),
            "the survivor keeps every sample"
        );
    }
}
