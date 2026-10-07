//! The scheduler: one cycle thread on absolute deadlines, stepping every instance due at a tick.
//!
//! Per tick: the external inputs and tunables of every due instance are fetched; if any input is
//! stale the tick is skipped (status `Faulted`, `stale` counted, heartbeat held, outputs
//! untouched) and published, so connectors show the fault at once; else the due instances step
//! in declared order, each after fetching its wired inputs, their outputs are stored and the
//! cycle is published, which advances the heartbeat. A model error terminates
//! every instance and ends the run with that error (fail-stop). Missed deadlines are counted as
//! overruns; the tick count stays contiguous, so model time is `start_time + tick · tick_s`.
//! Nothing on the tick path allocates.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::info;

use crate::clock::{CycleClock, Deadline, Mono};
use crate::image::{CycleImage, Direction, ImageError, SignalId};
use crate::model::{ModelAdapter, ModelError, ModelInstance, StepIo};
use crate::plan::{Plan, SystemSignals};
use crate::project::RealtimeConfig;
use crate::sys::{LinuxRealtime, MonotonicClock, RealtimeSyscalls, affinity_width, errno_name};
use crate::value::Buffer;

/// The engine's state, published as `<prefix>.status` (`i32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    /// Before the first tick.
    Init = 0,
    /// Ticks step and publish.
    Running = 1,
    /// An input was stale at the last tick; recovers by itself.
    Faulted = 2,
    /// The run ended, on request or after a model error.
    Stopped = 3,
}

/// What a run did, returned by [`EngineHandle::join`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunSummary {
    /// Ticks processed, skipped ones included.
    pub cycles: u64,
    /// Ticks whose outputs were published.
    pub published: u64,
    /// Deadlines missed.
    pub overruns: u64,
    /// Ticks skipped for a stale input.
    pub stale: u64,
    /// Longest tick, from its deadline to its publish.
    pub max_cycle: Duration,
    /// Mean tick duration.
    pub mean_cycle: Duration,
}

/// The engine could not start or stopped on a failure.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Plan and instances do not fit together.
    #[error("init: {0}")]
    Init(String),
    /// The real-time settings were refused.
    #[error("realtime: {0}")]
    Realtime(String),
    /// A process image access failed.
    #[error("image: {0}")]
    Image(#[from] ImageError),
    /// A model failed; the run is over.
    #[error("instance `{instance}`: {source}")]
    Model {
        /// Instance id.
        instance: String,
        /// The failure.
        source: ModelError,
    },
    /// The cycle thread unwound.
    #[error("the cycle thread panicked")]
    Panicked,
}

/// Loaded adapters by model id.
pub type ModelAdapters = BTreeMap<String, Arc<dyn ModelAdapter>>;

/// Create every instance of `plan` from `adapters`, in plan order.
///
/// # Errors
/// A model has no adapter, or its adapter refuses the instance.
pub fn instantiate(
    plan: &Plan,
    adapters: &ModelAdapters,
) -> Result<Vec<Box<dyn ModelInstance>>, EngineError> {
    plan.instances
        .iter()
        .map(|inst| {
            let adapter = adapters.get(&inst.model).ok_or_else(|| {
                EngineError::Init(format!("no adapter loaded for model `{}`", inst.model))
            })?;
            adapter
                .instantiate(&inst.spec)
                .map_err(|source| EngineError::Model {
                    instance: inst.id.clone(),
                    source,
                })
        })
        .collect()
}

/// Starts the cycle thread.
#[derive(Debug)]
pub struct Engine;

impl Engine {
    /// Start a run on the monotonic clock, with `realtime` applied to the cycle thread.
    ///
    /// Returns once every instance is initialised, or with the error that stopped it.
    ///
    /// # Errors
    /// Real-time settings refused, plan and instances mismatched, or an instance failed to init.
    pub fn start(
        plan: Plan,
        instances: Vec<Box<dyn ModelInstance>>,
        image: CycleImage,
        realtime: Option<RealtimeConfig>,
    ) -> Result<EngineHandle, EngineError> {
        Self::start_with(
            plan,
            instances,
            image,
            realtime,
            MonotonicClock,
            LinuxRealtime,
        )
    }

    /// [`Self::start`] with an injected clock and syscall seam.
    ///
    /// # Errors
    /// As [`Self::start`].
    pub fn start_with<C, S>(
        plan: Plan,
        instances: Vec<Box<dyn ModelInstance>>,
        image: CycleImage,
        realtime: Option<RealtimeConfig>,
        clock: C,
        sys: S,
    ) -> Result<EngineHandle, EngineError>
    where
        C: CycleClock + Send + 'static,
        S: RealtimeSyscalls + Send + 'static,
    {
        let cycle = Cycle::new(&plan, instances, image)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (finished_tx, finished_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop_flag = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("taktwerk-cycle".to_owned())
            .spawn(move || {
                let result = run(
                    cycle, plan.tick, realtime, &clock, &sys, &stop_flag, &ready_tx,
                );
                finished_tx.send_replace(true);
                result
            })
            .map_err(|e| EngineError::Init(format!("cannot start the cycle thread: {e}")))?;
        let handle = EngineHandle {
            stop,
            finished: finished_rx,
            join: Some(join),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => Err(handle.join().err().unwrap_or(EngineError::Panicked)),
        }
    }
}

/// A running engine.
#[derive(Debug)]
pub struct EngineHandle {
    stop: Arc<AtomicBool>,
    finished: watch::Receiver<bool>,
    join: Option<JoinHandle<Result<RunSummary, EngineError>>>,
}

impl EngineHandle {
    /// Ask the cycle thread to stop after the current tick.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Flips to `true` when the cycle thread has ended, on request or on a failure.
    #[must_use]
    pub fn finished(&self) -> watch::Receiver<bool> {
        self.finished.clone()
    }

    /// Whether the cycle thread has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        *self.finished.borrow()
    }

    /// Wait for the cycle thread and return what the run did, or the failure that ended it.
    ///
    /// # Errors
    /// The run ended on a model error, or the thread panicked.
    pub fn join(mut self) -> Result<RunSummary, EngineError> {
        match self.join.take() {
            Some(join) => join.join().unwrap_or(Err(EngineError::Panicked)),
            None => Err(EngineError::Panicked),
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The cycle thread's body.
#[allow(
    clippy::too_many_arguments,
    reason = "one private entry point, called once"
)]
fn run(
    mut cycle: Cycle,
    tick: Duration,
    realtime: Option<RealtimeConfig>,
    clock: &dyn CycleClock,
    sys: &dyn RealtimeSyscalls,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<(), EngineError>>,
) -> Result<RunSummary, EngineError> {
    let base_instant = Instant::now();
    let base_mono = clock.now();
    let instant_of = |now: Mono| {
        base_instant
            .checked_add(now.saturating_since(base_mono))
            .unwrap_or(base_instant)
    };
    let prepared = realtime
        .as_ref()
        .map_or(Ok(()), |cfg| apply_realtime(cfg, sys))
        .and_then(|()| cycle.init(instant_of(clock.now())));
    if let Err(e) = prepared {
        cycle.terminate(Status::Stopped);
        let _ = ready.send(Err(e));
        return Err(EngineError::Init("the engine did not start".into()));
    }
    let _ = ready.send(Ok(()));
    info!(
        instances = cycle.slots.len(),
        tick_ms = tick.as_secs_f64() * 1e3,
        "cycle thread started"
    );

    let mut grid = Deadline::start(clock.now(), tick);
    let mut sum = Duration::ZERO;
    let mut max = Duration::ZERO;
    let stopped = || stop.load(Ordering::Acquire);
    loop {
        if clock.sleep_until(grid.next(), &stopped) {
            break;
        }
        let t0 = grid.next();
        cycle.tick(instant_of(clock.now()))?;
        let took = clock.now().saturating_since(t0);
        sum = sum.saturating_add(took);
        max = max.max(took);
        let missed = grid.advance(clock.now());
        cycle.overruns = cycle.overruns.saturating_add(u64::from(missed));
    }
    cycle.terminate(Status::Stopped);
    let _ = cycle.store_system(instant_of(clock.now()));
    let cycles = cycle.tick;
    let summary = RunSummary {
        cycles,
        published: cycle.heartbeat,
        overruns: cycle.overruns,
        stale: cycle.stale,
        max_cycle: max,
        mean_cycle: if cycles == 0 {
            Duration::ZERO
        } else {
            sum / u32::try_from(cycles).unwrap_or(u32::MAX)
        },
    };
    info!(?summary, "cycle thread stopped");
    Ok(summary)
}

/// Lock memory, pin and set the class of the calling thread as `cfg` asks.
fn apply_realtime(cfg: &RealtimeConfig, sys: &dyn RealtimeSyscalls) -> Result<(), EngineError> {
    if cfg.lock_memory {
        sys.lock_memory().map_err(|code| {
            EngineError::Realtime(format!(
                "mlockall(MCL_CURRENT | MCL_FUTURE) failed: {}; raise RLIMIT_MEMLOCK or drop \
                 lock_memory",
                errno_name(code)
            ))
        })?;
    }
    if let Some(cpu) = cfg.cpu {
        if cpu >= affinity_width() {
            return Err(EngineError::Realtime(format!(
                "cpu = {cpu} is past the affinity mask ({} CPUs)",
                affinity_width()
            )));
        }
        sys.set_affinity(cpu).map_err(|code| {
            EngineError::Realtime(format!(
                "cannot pin the cycle thread to CPU {cpu}: sched_setaffinity failed: {}",
                errno_name(code)
            ))
        })?;
    }
    sys.set_scheduler(cfg.policy, cfg.priority)
        .map_err(|code| {
            EngineError::Realtime(format!(
                "cannot put the cycle thread on {:?} at priority {}: pthread_setschedparam failed: \
             {}; a real-time class needs CAP_SYS_NICE or an RLIMIT_RTPRIO of at least {}",
                cfg.policy,
                cfg.priority,
                errno_name(code),
                cfg.priority
            ))
        })?;
    info!(policy = ?cfg.policy, priority = cfg.priority, cpu = ?cfg.cpu, lock_memory = cfg.lock_memory, "real-time settings applied to the cycle thread");
    Ok(())
}

/// One instance on the cycle thread, with its buffers.
struct Slot {
    id: String,
    every: u64,
    instance: Box<dyn ModelInstance>,
    io: StepIo,
    /// `io.inputs` index → external input signal.
    external: Vec<(usize, SignalId)>,
    /// `io.inputs` index → output signal of another instance.
    wired: Vec<(usize, SignalId)>,
    /// `io.outputs` index → signal.
    outputs: Vec<SignalId>,
    /// `io.tunables` index → signal and the write counter last delivered.
    tunables: Vec<(SignalId, u64)>,
}

/// The per-tick state, allocated once.
struct Cycle {
    image: CycleImage,
    slots: Vec<Slot>,
    system: SystemSignals,
    tick: u64,
    heartbeat: u64,
    overruns: u64,
    stale: u64,
    status: Status,
    /// Slots whose `init` was called; only these are terminated.
    initialised: usize,
    terminated: bool,
    start_time: f64,
    tick_s: f64,
    /// Scratch buffers for the system signals.
    u64_buf: Buffer,
    i32_buf: Buffer,
}

/// What one tick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tick {
    /// Stepped and published.
    Published,
    /// Skipped for a stale input.
    Skipped,
}

impl Cycle {
    /// Allocate every buffer `plan` needs around `instances`.
    fn new(
        plan: &Plan,
        instances: Vec<Box<dyn ModelInstance>>,
        image: CycleImage,
    ) -> Result<Self, EngineError> {
        if instances.len() != plan.instances.len() {
            return Err(EngineError::Init(format!(
                "the plan has {} instances, {} were given",
                plan.instances.len(),
                instances.len()
            )));
        }
        let layout = image.layout();
        let buffer_of = |id: SignalId| {
            layout
                .spec(id)
                .map(|s| Buffer::zeroed(s.ty, s.len()))
                .ok_or(EngineError::Image(ImageError::Unknown(id)))
        };
        let mut slots = Vec::with_capacity(instances.len());
        for (inst, instance) in plan.instances.iter().zip(instances) {
            let mut io = StepIo::default();
            let mut external = Vec::new();
            let mut wired = Vec::new();
            for (k, port) in inst.inputs.iter().enumerate() {
                io.inputs.push(buffer_of(port.signal)?);
                match layout.spec(port.signal).map(|s| s.direction) {
                    Some(Direction::Output) => wired.push((k, port.signal)),
                    _ => external.push((k, port.signal)),
                }
            }
            for port in &inst.outputs {
                io.outputs.push(buffer_of(port.signal)?);
            }
            for port in &inst.tunables {
                let mut buf = buffer_of(port.signal)?;
                if let Some(start) = inst.spec.params.get(&port.name) {
                    buf.copy_from(start).map_err(|_| {
                        EngineError::Init(format!(
                            "instance `{}`: start value of `{}` does not match its signal",
                            inst.id, port.name
                        ))
                    })?;
                }
                io.tunables.push(buf);
            }
            slots.push(Slot {
                id: inst.id.clone(),
                every: u64::from(inst.every.max(1)),
                instance,
                io,
                external,
                wired,
                outputs: inst.outputs.iter().map(|p| p.signal).collect(),
                tunables: inst.tunables.iter().map(|p| (p.signal, 0)).collect(),
            });
        }
        Ok(Self {
            image,
            slots,
            system: plan.system,
            tick: 0,
            heartbeat: 0,
            overruns: 0,
            stale: 0,
            status: Status::Init,
            initialised: 0,
            terminated: false,
            start_time: plan.start_time,
            tick_s: plan.tick.as_secs_f64(),
            u64_buf: Buffer::U64(vec![0]),
            i32_buf: Buffer::I32(vec![0]),
        })
    }

    /// Initialise every instance with whatever inputs are present. May allocate.
    fn init(&mut self, now: Instant) -> Result<(), EngineError> {
        for i in 0..self.slots.len() {
            let slot = &mut self.slots[i];
            for &(k, id) in slot.external.iter().chain(&slot.wired) {
                match self.image.fetch(id, &mut slot.io.inputs[k], now) {
                    Ok(_) | Err(ImageError::Stale(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            for (k, (id, seq)) in slot.tunables.iter_mut().enumerate() {
                // A tunable nobody wrote yet keeps its start value.
                let mut written = slot.io.tunables[k].clone();
                let s = self.image.fetch(*id, &mut written, now)?;
                if s != 0 {
                    slot.io.tunables[k] = written;
                    *seq = s;
                }
            }
            let time = self.start_time;
            self.initialised = i + 1;
            slot.instance
                .init(time, &mut slot.io)
                .map_err(|source| EngineError::Model {
                    instance: slot.id.clone(),
                    source,
                })?;
        }
        self.status = Status::Running;
        self.store_system(now)?;
        Ok(())
    }

    /// Run one tick at `now`. No allocation.
    fn tick(&mut self, now: Instant) -> Result<Tick, EngineError> {
        let tick = self.tick;
        let time = self.start_time + tick as f64 * self.tick_s;
        let mut stale = false;
        for slot in self.slots.iter_mut().filter(|s| tick % s.every == 0) {
            for &(k, id) in &slot.external {
                match self.image.fetch(id, &mut slot.io.inputs[k], now) {
                    Ok(_) => {}
                    Err(ImageError::Stale(_)) => stale = true,
                    Err(e) => return Err(e.into()),
                }
            }
            for (k, (id, seq)) in slot.tunables.iter_mut().enumerate() {
                let s = self.image.fetch(*id, &mut slot.io.tunables[k], now)?;
                if s != *seq {
                    *seq = s;
                    slot.io.tunables_changed = true;
                }
            }
        }
        if stale {
            self.stale = self.stale.saturating_add(1);
            self.status = Status::Faulted;
            self.tick = tick.saturating_add(1);
            self.store_system(now)?;
            // Connectors see the held heartbeat and the fault at once; outputs are untouched.
            self.image.publish(tick);
            return Ok(Tick::Skipped);
        }
        for i in 0..self.slots.len() {
            let slot = &mut self.slots[i];
            if tick % slot.every != 0 {
                continue;
            }
            for &(k, id) in &slot.wired {
                self.image.fetch(id, &mut slot.io.inputs[k], now)?;
            }
            if let Err(source) = slot.instance.step(time, &mut slot.io) {
                let instance = slot.id.clone();
                self.terminate(Status::Stopped);
                let _ = self.store_system(now);
                return Err(EngineError::Model { instance, source });
            }
            slot.io.tunables_changed = false;
            for (k, &id) in slot.outputs.iter().enumerate() {
                self.image.store(id, &slot.io.outputs[k], now)?;
            }
        }
        self.heartbeat = self.heartbeat.saturating_add(1);
        self.status = Status::Running;
        self.tick = tick.saturating_add(1);
        self.store_system(now)?;
        self.image.publish(tick);
        Ok(Tick::Published)
    }

    /// Store heartbeat, status and counters. No allocation.
    fn store_system(&mut self, now: Instant) -> Result<(), EngineError> {
        let sys = self.system;
        for (id, value) in [
            (sys.heartbeat, self.heartbeat),
            (sys.cycle, self.tick),
            (sys.overruns, self.overruns),
            (sys.stale, self.stale),
        ] {
            if let Buffer::U64(b) = &mut self.u64_buf {
                b[0] = value;
            }
            self.image.store(id, &self.u64_buf, now)?;
        }
        if let Buffer::I32(b) = &mut self.i32_buf {
            b[0] = self.status as i32;
        }
        self.image.store(sys.status, &self.i32_buf, now)?;
        Ok(())
    }

    /// Terminate every instance once and set `status`.
    fn terminate(&mut self, status: Status) {
        self.status = status;
        if self.terminated {
            return;
        }
        self.terminated = true;
        for slot in &mut self.slots[..self.initialised] {
            slot.instance.terminate();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::alloc_count;
    use crate::image::{ImageHandle, ImageLayout, SignalSpec, image};
    use crate::model::{Causality, InstanceSpec, ModelInterface, Variable};
    use crate::plan::{InstancePlan, Port};
    use crate::project::SchedPolicy;
    use crate::value::{Layout, ScalarType};

    /// A log every fake instance writes to: `(instance, event, time)`. Ids are shared so a
    /// log entry costs no allocation once the log has capacity.
    type Log = Arc<Mutex<Vec<(Arc<str>, &'static str, f64)>>>;

    /// Copies input 0 to output 0 (plus a gain from tunable 0 when present) and logs calls.
    struct Fake {
        id: Arc<str>,
        log: Log,
        fail_at_step: Option<u64>,
        steps: u64,
    }

    impl ModelInstance for Fake {
        fn init(&mut self, start_time: f64, _io: &mut StepIo) -> Result<(), ModelError> {
            self.log
                .lock()
                .unwrap()
                .push((self.id.clone(), "init", start_time));
            Ok(())
        }

        fn step(&mut self, time: f64, io: &mut StepIo) -> Result<(), ModelError> {
            self.steps += 1;
            if self.fail_at_step == Some(self.steps) {
                return Err(ModelError::Call {
                    call: "step",
                    code: 7,
                    detail: "boom".into(),
                });
            }
            if io.tunables_changed {
                self.log
                    .lock()
                    .unwrap()
                    .push((self.id.clone(), "changed", time));
            }
            let gain = match io.tunables.first() {
                Some(Buffer::F64(t)) => t[0],
                _ => 1.0,
            };
            if let (Some(Buffer::F64(x)), Some(Buffer::F64(y))) =
                (io.inputs.first(), io.outputs.first_mut())
            {
                for (yi, xi) in y.iter_mut().zip(x) {
                    *yi = xi * gain;
                }
            }
            self.log
                .lock()
                .unwrap()
                .push((self.id.clone(), "step", time));
            Ok(())
        }

        fn terminate(&mut self) {
            self.log
                .lock()
                .unwrap()
                .push((self.id.clone(), "terminate", 0.0));
        }
    }

    fn spec(name: &str, direction: Direction, max_age: Option<Duration>) -> SignalSpec {
        SignalSpec {
            name: name.into(),
            ty: ScalarType::F64,
            shape: vec![1],
            layout: Layout::RowMajor,
            direction,
            max_age,
        }
    }

    fn sys_specs() -> Vec<SignalSpec> {
        let s = |n: &str, ty| SignalSpec {
            name: format!("tw.{n}"),
            ty,
            shape: vec![],
            layout: Layout::RowMajor,
            direction: Direction::System,
            max_age: None,
        };
        vec![
            s("heartbeat", ScalarType::U64),
            s("status", ScalarType::I32),
            s("cycle", ScalarType::U64),
            s("overruns", ScalarType::U64),
            s("stale", ScalarType::U64),
        ]
    }

    fn system() -> SystemSignals {
        SystemSignals {
            heartbeat: SignalId(0),
            status: SignalId(1),
            cycle: SignalId(2),
            overruns: SignalId(3),
            stale: SignalId(4),
        }
    }

    fn inst(
        id: &str,
        every: u32,
        input: &str,
        output: &str,
        tunable: Option<&str>,
        layout: &ImageLayout,
    ) -> InstancePlan {
        let port = |n: &str, s: &str| Port {
            name: n.into(),
            signal: layout.id(s).unwrap(),
        };
        InstancePlan {
            id: id.into(),
            model: "m".into(),
            every,
            spec: InstanceSpec {
                id: id.into(),
                dims: Default::default(),
                params: Default::default(),
                step_size: 0.01 * f64::from(every),
            },
            inputs: vec![port("x", input)],
            outputs: vec![port("y", output)],
            tunables: tunable.map(|t| vec![port("k", t)]).unwrap_or_default(),
        }
    }

    /// `a` (every 1): ext → a.y; `b` (every 2): a.y → b.y, with tunable `b.k`.
    fn two_instance_plan(max_age: Option<Duration>) -> Plan {
        let mut specs = sys_specs();
        specs.push(spec("ext", Direction::Input, max_age));
        specs.push(spec("a.y", Direction::Output, None));
        specs.push(spec("b.y", Direction::Output, None));
        specs.push(spec("b.k", Direction::Tunable, None));
        let layout = ImageLayout::new(specs).unwrap();
        let instances = vec![
            inst("a", 1, "ext", "a.y", None, &layout),
            inst("b", 2, "a.y", "b.y", Some("b.k"), &layout),
        ];
        Plan {
            tick: Duration::from_millis(10),
            start_time: 100.0,
            layout,
            instances,
            system: system(),
        }
    }

    struct Rig {
        cycle: Cycle,
        handle: ImageHandle,
        log: Log,
        now: Instant,
    }

    impl Rig {
        fn new(plan: Plan, fail_b_at: Option<u64>) -> Self {
            let log: Log = Default::default();
            let (cycle_image, handle) = image(plan.layout.clone());
            let fakes: Vec<Box<dyn ModelInstance>> = plan
                .instances
                .iter()
                .map(|i| {
                    Box::new(Fake {
                        id: Arc::from(i.id.as_str()),
                        log: Arc::clone(&log),
                        fail_at_step: if i.id == "b" { fail_b_at } else { None },
                        steps: 0,
                    }) as Box<dyn ModelInstance>
                })
                .collect();
            let cycle = Cycle::new(&plan, fakes, cycle_image).unwrap();
            Self {
                cycle,
                handle,
                log,
                now: Instant::now(),
            }
        }

        fn id(&self, name: &str) -> SignalId {
            self.handle.layout().id(name).unwrap()
        }

        fn write(&self, name: &str, v: f64) {
            self.handle
                .write_stamped(self.id(name), &Buffer::F64(vec![v]), self.now)
                .unwrap();
        }

        fn f64(&self, name: &str) -> f64 {
            let mut b = Buffer::F64(vec![0.0]);
            self.handle.read(self.id(name), &mut b).unwrap();
            match b {
                Buffer::F64(v) => v[0],
                _ => unreachable!(),
            }
        }

        fn u64(&self, name: &str) -> u64 {
            let mut b = Buffer::U64(vec![0]);
            self.handle.read(self.id(name), &mut b).unwrap();
            match b {
                Buffer::U64(v) => v[0],
                _ => unreachable!(),
            }
        }

        fn status(&self) -> i32 {
            let mut b = Buffer::I32(vec![0]);
            self.handle.read(self.id("tw.status"), &mut b).unwrap();
            match b {
                Buffer::I32(v) => v[0],
                _ => unreachable!(),
            }
        }

        fn tick(&mut self) -> Result<Tick, EngineError> {
            self.now += Duration::from_millis(10);
            self.cycle.tick(self.now)
        }

        fn events(&self) -> Vec<(Arc<str>, &'static str, f64)> {
            self.log.lock().unwrap().clone()
        }
    }

    #[test]
    fn instances_step_in_order_at_their_period_and_data_flows_through_the_image() {
        let mut rig = Rig::new(two_instance_plan(None), None);
        rig.cycle.init(rig.now).unwrap();
        assert_eq!(rig.status(), Status::Running as i32);
        rig.write("ext", 2.0);
        rig.write("b.k", 10.0);
        for _ in 0..4 {
            assert_eq!(rig.tick().unwrap(), Tick::Published);
        }
        let steps: Vec<_> = rig.events().into_iter().filter(|e| e.1 == "step").collect();
        let expected = [
            ("a", 100.0),
            ("b", 100.0),
            ("a", 100.01),
            ("a", 100.02),
            ("b", 100.02),
            ("a", 100.03),
        ];
        assert_eq!(steps.len(), expected.len());
        for ((id, _, t), (eid, et)) in steps.iter().zip(expected) {
            assert_eq!(&**id, eid);
            assert!((t - et).abs() < 1e-9, "{id} at {t}, expected {et}");
        }
        assert_eq!(rig.f64("a.y"), 2.0);
        // `b` runs after `a` in the same tick and sees this tick's `a.y`, scaled by its tunable.
        assert_eq!(rig.f64("b.y"), 20.0);
        assert_eq!(rig.u64("tw.heartbeat"), 4);
        assert_eq!(rig.u64("tw.cycle"), 4);
        assert_eq!(*rig.handle.published().borrow(), 3);
    }

    #[test]
    fn a_stale_input_skips_the_tick_holds_the_heartbeat_and_recovers() {
        let mut rig = Rig::new(two_instance_plan(Some(Duration::from_millis(15))), None);
        rig.cycle.init(rig.now).unwrap();
        rig.write("ext", 1.0);
        assert_eq!(rig.tick().unwrap(), Tick::Published);
        assert_eq!(rig.status(), Status::Running as i32);
        // 20 ms after the write: stale.
        assert_eq!(rig.tick().unwrap(), Tick::Skipped);
        assert_eq!(rig.status(), Status::Faulted as i32);
        assert_eq!(rig.u64("tw.heartbeat"), 1);
        assert_eq!(rig.u64("tw.stale"), 1);
        assert_eq!(rig.u64("tw.cycle"), 2);
        // The skipped tick is published with the held heartbeat, so connectors see the fault.
        assert_eq!(*rig.handle.published().borrow(), 1);
        let steps_before = rig.events().iter().filter(|e| e.1 == "step").count();
        // A fresh write recovers without intervention.
        rig.write("ext", 3.0);
        assert_eq!(rig.tick().unwrap(), Tick::Published);
        assert_eq!(rig.status(), Status::Running as i32);
        assert_eq!(rig.u64("tw.heartbeat"), 2);
        assert_eq!(rig.f64("a.y"), 3.0);
        assert!(rig.events().iter().filter(|e| e.1 == "step").count() > steps_before);
    }

    #[test]
    fn a_model_error_terminates_everything_and_fails_the_run() {
        let mut rig = Rig::new(two_instance_plan(None), Some(1));
        rig.cycle.init(rig.now).unwrap();
        let err = rig.tick().unwrap_err();
        assert!(
            matches!(err, EngineError::Model { ref instance, .. } if instance == "b"),
            "{err}"
        );
        assert_eq!(rig.status(), Status::Stopped as i32);
        let terminated: Vec<_> = rig
            .events()
            .into_iter()
            .filter(|e| e.1 == "terminate")
            .map(|e| e.0)
            .collect();
        assert_eq!(
            terminated.iter().map(|s| &**s).collect::<Vec<_>>(),
            ["a", "b"]
        );
        // A second terminate is a no-op.
        rig.cycle.terminate(Status::Stopped);
        assert_eq!(
            rig.events().iter().filter(|e| e.1 == "terminate").count(),
            2
        );
    }

    #[test]
    fn a_tunable_write_is_flagged_once_and_survives_a_skipped_tick() {
        let mut plan = two_instance_plan(Some(Duration::from_millis(15)));
        plan.instances[1].every = 1;
        let mut rig = Rig::new(plan, None);
        rig.cycle.init(rig.now).unwrap();
        rig.write("ext", 1.0);
        rig.tick().unwrap(); // tick 0: nothing changed
        rig.write("b.k", 5.0);
        rig.tick().unwrap(); // tick 1: stale, skipped; the change is noted
        assert_eq!(rig.u64("tw.stale"), 1);
        assert!(rig.cycle.slots[1].io.tunables_changed);
        rig.write("ext", 1.0);
        rig.tick().unwrap(); // tick 2: b steps with the flag set
        rig.tick().unwrap(); // tick 3: flag cleared
        assert_eq!(rig.f64("b.y"), 5.0);
        assert!(!rig.cycle.slots[1].io.tunables_changed);
        assert_eq!(rig.cycle.slots[1].tunables[0].1, 1);
        let changed: Vec<_> = rig
            .events()
            .into_iter()
            .filter(|e| e.1 == "changed")
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(&*changed[0].0, "b");
        assert!((changed[0].2 - 100.02).abs() < 1e-9);
    }

    #[test]
    fn a_steady_state_tick_does_not_allocate() {
        let mut rig = Rig::new(two_instance_plan(Some(Duration::from_secs(1))), None);
        rig.cycle.init(rig.now).unwrap();
        rig.write("ext", 1.0);
        rig.write("b.k", 2.0);
        for _ in 0..4 {
            rig.tick().unwrap();
        }
        // The fakes log every step; keep that from growing the log under measurement.
        rig.log.lock().unwrap().reserve(1_000);
        let before = alloc_count::allocations();
        for _ in 0..50 {
            rig.now += Duration::from_millis(10);
            let now = rig.now;
            let _ = rig.cycle.tick(now).unwrap();
        }
        // Skipped ticks too.
        rig.now += Duration::from_secs(2);
        let now = rig.now;
        assert_eq!(rig.cycle.tick(now).unwrap(), Tick::Skipped);
        let after = alloc_count::allocations();
        assert_eq!(after - before, 0, "the tick path allocated");
        assert_eq!(rig.u64("tw.stale"), 1);
    }

    /// Advances to every deadline at once and asks to stop after `ticks` sleeps.
    struct FakeClock {
        now: Mutex<u64>,
        sleeps: Mutex<u64>,
        ticks: u64,
        /// Extra time each tick "takes", so overruns can be provoked.
        work: u64,
    }

    impl CycleClock for FakeClock {
        fn now(&self) -> Mono {
            let mut now = self.now.lock().unwrap();
            *now += self.work;
            Mono::from_nanos(*now)
        }

        fn sleep_until(&self, deadline: Mono, stop: &dyn Fn() -> bool) -> bool {
            let mut sleeps = self.sleeps.lock().unwrap();
            if *sleeps >= self.ticks || stop() {
                return true;
            }
            *sleeps += 1;
            let mut now = self.now.lock().unwrap();
            *now = (*now).max(deadline.as_nanos());
            false
        }
    }

    #[derive(Default)]
    struct FakeSys {
        calls: Mutex<Vec<String>>,
        refuse_class: Option<i32>,
    }

    impl RealtimeSyscalls for FakeSys {
        fn set_scheduler(&self, policy: SchedPolicy, priority: u8) -> Result<(), i32> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("sched {policy:?} {priority}"));
            self.refuse_class.map_or(Ok(()), Err)
        }
        fn set_affinity(&self, cpu: usize) -> Result<(), i32> {
            self.calls.lock().unwrap().push(format!("cpu {cpu}"));
            Ok(())
        }
        fn lock_memory(&self) -> Result<(), i32> {
            self.calls.lock().unwrap().push("mlockall".into());
            Ok(())
        }
    }

    fn fakes(plan: &Plan, log: &Log) -> Vec<Box<dyn ModelInstance>> {
        plan.instances
            .iter()
            .map(|i| {
                Box::new(Fake {
                    id: Arc::from(i.id.as_str()),
                    log: Arc::clone(log),
                    fail_at_step: None,
                    steps: 0,
                }) as Box<dyn ModelInstance>
            })
            .collect()
    }

    #[test]
    fn the_thread_runs_on_the_grid_and_reports_a_summary() {
        let plan = two_instance_plan(None);
        let (cycle_image, handle) = image(plan.layout.clone());
        let log: Log = Default::default();
        let clock = FakeClock {
            now: Mutex::new(1_000),
            sleeps: Mutex::new(0),
            ticks: 10,
            work: 0,
        };
        let engine = Engine::start_with(
            plan.clone(),
            fakes(&plan, &log),
            cycle_image,
            None,
            clock,
            FakeSys::default(),
        )
        .unwrap();
        let summary = engine.join().unwrap();
        assert_eq!(summary.cycles, 10);
        assert_eq!(summary.published, 10);
        assert_eq!(summary.overruns, 0);
        assert_eq!(summary.stale, 0);
        let mut b = Buffer::I32(vec![0]);
        handle
            .read(handle.layout().id("tw.status").unwrap(), &mut b)
            .unwrap();
        assert_eq!(b, Buffer::I32(vec![Status::Stopped as i32]));
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|e| e.1 == "terminate")
                .count(),
            2
        );
    }

    #[test]
    fn overruns_are_counted_and_the_tick_count_stays_contiguous() {
        let plan = two_instance_plan(None);
        let (cycle_image, _handle) = image(plan.layout.clone());
        let log: Log = Default::default();
        // Every `now()` costs 12 ms; a tick reads the clock several times, so each overruns.
        let clock = FakeClock {
            now: Mutex::new(0),
            sleeps: Mutex::new(0),
            ticks: 5,
            work: 12_000_000,
        };
        let engine = Engine::start_with(
            plan.clone(),
            fakes(&plan, &log),
            cycle_image,
            None,
            clock,
            FakeSys::default(),
        )
        .unwrap();
        let summary = engine.join().unwrap();
        assert_eq!(summary.cycles, 5);
        assert!(summary.overruns >= 5, "{summary:?}");
        assert!(summary.max_cycle >= Duration::from_millis(12));
        let times: Vec<f64> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|e| &*e.0 == "a" && e.1 == "step")
            .map(|e| e.2)
            .collect();
        assert_eq!(times.len(), 5);
        for (i, t) in times.iter().enumerate() {
            assert!((t - (100.0 + i as f64 * 0.01)).abs() < 1e-9, "{times:?}");
        }
    }

    impl RealtimeSyscalls for Arc<FakeSys> {
        fn set_scheduler(&self, p: SchedPolicy, prio: u8) -> Result<(), i32> {
            FakeSys::set_scheduler(self, p, prio)
        }
        fn set_affinity(&self, cpu: usize) -> Result<(), i32> {
            FakeSys::set_affinity(self, cpu)
        }
        fn lock_memory(&self) -> Result<(), i32> {
            FakeSys::lock_memory(self)
        }
    }

    fn fake_clock(ticks: u64) -> FakeClock {
        FakeClock {
            now: Mutex::new(0),
            sleeps: Mutex::new(0),
            ticks,
            work: 0,
        }
    }

    #[test]
    fn realtime_settings_apply_on_the_cycle_thread_and_a_refusal_fails_the_start() {
        let plan = two_instance_plan(None);
        let cfg = RealtimeConfig {
            policy: SchedPolicy::Fifo,
            priority: 50,
            cpu: Some(0),
            lock_memory: true,
        };
        let log: Log = Default::default();
        let (cycle_image, _h) = image(plan.layout.clone());
        let sys = Arc::new(FakeSys::default());
        let engine = Engine::start_with(
            plan.clone(),
            fakes(&plan, &log),
            cycle_image,
            Some(cfg.clone()),
            fake_clock(1),
            Arc::clone(&sys),
        )
        .unwrap();
        engine.join().unwrap();
        assert_eq!(
            *sys.calls.lock().unwrap(),
            ["mlockall", "cpu 0", "sched Fifo 50"]
        );

        let log: Log = Default::default();
        let (cycle_image, _h) = image(plan.layout.clone());
        let refusing = Arc::new(FakeSys {
            refuse_class: Some(libc::EPERM),
            ..Default::default()
        });
        let err = Engine::start_with(
            plan.clone(),
            fakes(&plan, &log),
            cycle_image,
            Some(cfg),
            fake_clock(1),
            refusing,
        )
        .unwrap_err();
        assert!(
            matches!(err, EngineError::Realtime(ref m) if m.contains("errno 1")),
            "{err}"
        );
        // Nothing was initialised, so nothing is terminated.
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn stop_ends_the_run() {
        let plan = two_instance_plan(None);
        let (cycle_image, _h) = image(plan.layout.clone());
        let log: Log = Default::default();
        let clock = FakeClock {
            now: Mutex::new(0),
            sleeps: Mutex::new(0),
            ticks: u64::MAX,
            work: 0,
        };
        let engine = Engine::start_with(
            plan.clone(),
            fakes(&plan, &log),
            cycle_image,
            None,
            clock,
            FakeSys::default(),
        )
        .unwrap();
        let mut finished = engine.finished();
        engine.stop();
        let summary = engine.join().unwrap();
        assert!(*finished.borrow_and_update());
        assert_eq!(summary.published, summary.cycles);
    }

    struct FakeAdapter(ModelInterface);

    impl ModelAdapter for FakeAdapter {
        fn interface(&self) -> &ModelInterface {
            &self.0
        }
        fn instantiate(&self, spec: &InstanceSpec) -> Result<Box<dyn ModelInstance>, ModelError> {
            if spec.id == "bad" {
                return Err(ModelError::Instantiate("refused".into()));
            }
            Ok(Box::new(Fake {
                id: Arc::from(spec.id.as_str()),
                log: Default::default(),
                fail_at_step: None,
                steps: 0,
            }))
        }
    }

    #[test]
    fn instantiate_uses_the_adapter_of_each_model() {
        let plan = two_instance_plan(None);
        let interface = ModelInterface {
            name: "m".into(),
            dimensions: vec![],
            variables: vec![Variable {
                name: "x".into(),
                causality: Causality::Input,
                ty: ScalarType::F64,
                shape: vec![],
                layout: Layout::RowMajor,
                unit: None,
                description: None,
            }],
            instances: Default::default(),
        };
        let mut adapters = ModelAdapters::new();
        assert!(matches!(
            instantiate(&plan, &adapters).err(),
            Some(EngineError::Init(_))
        ));
        adapters.insert("m".into(), Arc::new(FakeAdapter(interface)));
        assert_eq!(instantiate(&plan, &adapters).unwrap().len(), 2);
        let mut bad = plan.clone();
        bad.instances[0].spec.id = "bad".into();
        assert!(matches!(
            instantiate(&bad, &adapters).err(),
            Some(EngineError::Model { .. })
        ));
    }
}
