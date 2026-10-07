//! The process image: the engine-owned value of every signal.
//!
//! Connectors write inputs and tunables and read outputs through an [`ImageHandle`] from any
//! thread. The cycle thread reads inputs into its own buffers at the tick, checks their age, and
//! stores outputs back through the [`CycleImage`]; [`CycleImage::publish`] then tells connectors a
//! cycle's outputs are complete. Each signal has its own lock, held only for one copy.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::value::{Buffer, Layout, ScalarType};

/// Index of a signal in its [`ImageLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalId(pub u32);

impl SignalId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Who writes a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Written by a connector, read by the cycle.
    Input,
    /// Written by the cycle (a model output), read by connectors and other models.
    Output,
    /// Written by a connector while running, delivered to a model as a tunable parameter.
    Tunable,
    /// Written by the engine itself: heartbeat, status, counters.
    System,
}

/// One signal of the image, with its shape bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalSpec {
    /// Unique name; connectors map external addresses to it.
    pub name: String,
    /// Element type.
    #[serde(rename = "type")]
    pub ty: ScalarType,
    /// Bound shape; empty for a scalar.
    pub shape: Vec<usize>,
    /// Storage order of a multi-dimensional value.
    pub layout: Layout,
    /// Who writes it.
    pub direction: Direction,
    /// Oldest an input may be at the tick before the cycle faults; `None` never goes stale.
    pub max_age: Option<Duration>,
}

impl SignalSpec {
    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    /// Whether the signal holds no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The fixed set of signals of one engine, built at init.
#[derive(Debug, Clone, Default)]
pub struct ImageLayout {
    specs: Vec<SignalSpec>,
    by_name: BTreeMap<String, SignalId>,
}

impl ImageLayout {
    /// Index `specs` by name.
    ///
    /// # Errors
    /// [`ImageError::Duplicate`] when two signals share a name.
    pub fn new(specs: Vec<SignalSpec>) -> Result<Self, ImageError> {
        let mut by_name = BTreeMap::new();
        for (i, spec) in specs.iter().enumerate() {
            let id = SignalId(u32::try_from(i).map_err(|_| ImageError::TooMany)?);
            if by_name.insert(spec.name.clone(), id).is_some() {
                return Err(ImageError::Duplicate(spec.name.clone()));
            }
        }
        Ok(Self { specs, by_name })
    }

    /// The signal called `name`.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<SignalId> {
        self.by_name.get(name).copied()
    }

    /// The spec of `id`.
    #[must_use]
    pub fn spec(&self, id: SignalId) -> Option<&SignalSpec> {
        self.specs.get(id.index())
    }

    /// All signals in id order.
    pub fn iter(&self) -> impl Iterator<Item = (SignalId, &SignalSpec)> {
        self.specs
            .iter()
            .enumerate()
            .map(|(i, s)| (SignalId(i as u32), s))
    }

    /// Number of signals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether there are no signals.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

#[derive(Debug)]
struct Slot {
    value: Buffer,
    /// When the value was last written; `None` until the first write.
    stamp: Option<Instant>,
    /// Bumped on every write.
    seq: u64,
}

#[derive(Debug)]
struct Shared {
    layout: ImageLayout,
    slots: Vec<Mutex<Slot>>,
}

impl Shared {
    fn slot(&self, id: SignalId) -> Result<MutexGuard<'_, Slot>, ImageError> {
        let slot = self.slots.get(id.index()).ok_or(ImageError::Unknown(id))?;
        // A writer that panicked mid-copy leaves a value of the right type and length; keep going.
        Ok(slot.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn direction(&self, id: SignalId) -> Result<Direction, ImageError> {
        self.layout
            .spec(id)
            .map(|s| s.direction)
            .ok_or(ImageError::Unknown(id))
    }
}

/// Create the image for `layout`: the cycle thread's side and a cloneable connector handle.
#[must_use]
pub fn image(layout: ImageLayout) -> (CycleImage, ImageHandle) {
    let slots = layout
        .specs
        .iter()
        .map(|s| {
            Mutex::new(Slot {
                value: Buffer::zeroed(s.ty, s.len()),
                stamp: None,
                seq: 0,
            })
        })
        .collect();
    let shared = Arc::new(Shared { layout, slots });
    let (tx, rx) = watch::channel(0);
    (
        CycleImage {
            shared: Arc::clone(&shared),
            published: tx,
        },
        ImageHandle {
            shared,
            published: rx,
        },
    )
}

/// The connector side of the image. Cheap to clone; usable from any thread.
#[derive(Debug, Clone)]
pub struct ImageHandle {
    shared: Arc<Shared>,
    published: watch::Receiver<u64>,
}

impl ImageHandle {
    /// The signals of this image.
    #[must_use]
    pub fn layout(&self) -> &ImageLayout {
        &self.shared.layout
    }

    /// Write an input or tunable and stamp it now.
    ///
    /// # Errors
    /// The signal is unknown, is written by the engine, or `value` does not match its type and
    /// length.
    pub fn write(&self, id: SignalId, value: &Buffer) -> Result<(), ImageError> {
        self.write_stamped(id, value, Instant::now())
    }

    /// Write an input or tunable with the time the value was sampled.
    ///
    /// # Errors
    /// As [`Self::write`].
    pub fn write_stamped(
        &self,
        id: SignalId,
        value: &Buffer,
        stamp: Instant,
    ) -> Result<(), ImageError> {
        match self.shared.direction(id)? {
            Direction::Input | Direction::Tunable => {}
            Direction::Output | Direction::System => return Err(ImageError::NotWritable(id)),
        }
        let mut slot = self.shared.slot(id)?;
        slot.value
            .copy_from(value)
            .map_err(|_| ImageError::Mismatch(id))?;
        slot.stamp = Some(stamp);
        slot.seq = slot.seq.wrapping_add(1);
        Ok(())
    }

    /// Copy the current value of any signal into `out`; returns when it was written.
    ///
    /// # Errors
    /// The signal is unknown or `out` does not match its type and length.
    pub fn read(&self, id: SignalId, out: &mut Buffer) -> Result<Option<Instant>, ImageError> {
        let slot = self.shared.slot(id)?;
        out.copy_from(&slot.value)
            .map_err(|_| ImageError::Mismatch(id))?;
        Ok(slot.stamp)
    }

    /// A receiver that changes to the cycle number each time a cycle's outputs are published.
    #[must_use]
    pub fn published(&self) -> watch::Receiver<u64> {
        self.published.clone()
    }
}

/// The cycle thread's side of the image. One per engine; not cloneable.
#[derive(Debug)]
pub struct CycleImage {
    shared: Arc<Shared>,
    published: watch::Sender<u64>,
}

impl CycleImage {
    /// The signals of this image.
    #[must_use]
    pub fn layout(&self) -> &ImageLayout {
        &self.shared.layout
    }

    /// Copy signal `id` into `out`, refusing it when older than its `max_age` at `now`.
    ///
    /// Returns the signal's write counter, so a caller can tell a tunable changed. No allocation.
    ///
    /// # Errors
    /// [`ImageError::Stale`] (or never written while a `max_age` is set), or a mismatch.
    pub fn fetch(&self, id: SignalId, out: &mut Buffer, now: Instant) -> Result<u64, ImageError> {
        let max_age = self.shared.layout.spec(id).and_then(|s| s.max_age);
        let slot = self.shared.slot(id)?;
        if let Some(max_age) = max_age {
            match slot.stamp {
                Some(stamp) if now.saturating_duration_since(stamp) <= max_age => {}
                _ => return Err(ImageError::Stale(id)),
            }
        }
        out.copy_from(&slot.value)
            .map_err(|_| ImageError::Mismatch(id))?;
        Ok(slot.seq)
    }

    /// Store an output or system signal from `value`. No allocation.
    ///
    /// # Errors
    /// The signal is unknown, is an input, or `value` does not match it.
    pub fn store(&self, id: SignalId, value: &Buffer, now: Instant) -> Result<(), ImageError> {
        match self.shared.direction(id)? {
            Direction::Output | Direction::System => {}
            Direction::Input | Direction::Tunable => return Err(ImageError::NotWritable(id)),
        }
        let mut slot = self.shared.slot(id)?;
        slot.value
            .copy_from(value)
            .map_err(|_| ImageError::Mismatch(id))?;
        slot.stamp = Some(now);
        slot.seq = slot.seq.wrapping_add(1);
        Ok(())
    }

    /// Announce that cycle `cycle`'s outputs are all stored.
    pub fn publish(&self, cycle: u64) {
        self.published.send_replace(cycle);
    }
}

/// A process image access failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    /// Two signals share a name.
    #[error("duplicate signal `{0}`")]
    Duplicate(String),
    /// More signals than a `u32` counts.
    #[error("too many signals")]
    TooMany,
    /// No such signal.
    #[error("unknown signal {0:?}")]
    Unknown(SignalId),
    /// The caller may not write this signal.
    #[error("signal {0:?} is not writable from here")]
    NotWritable(SignalId),
    /// Type or length differ from the signal's.
    #[error("signal {0:?}: type or length mismatch")]
    Mismatch(SignalId),
    /// Older than its `max_age`, or never written.
    #[error("signal {0:?} is stale")]
    Stale(SignalId),
}
