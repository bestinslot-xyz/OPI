//! The process's Prometheus metrics, every name prefixed `opi_`.
//!
//! [`global`] holds the one [`Metrics`] of the process. The source, the
//! runtime, the modules and the API record into it; opi-api's metrics
//! listener encodes it on each scrape with [`Metrics::encode`].
//!
//! Recording on a hot path is an atomic store or add: a module takes its
//! [`ModuleMetrics`] handle once and keeps it, and [`AtomicHistogram`] keeps
//! its buckets in atomics. Locks are taken only when a handle is created and
//! on a scrape.

use {
    prometheus_client::{
        collector::Collector,
        encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric, MetricEncoder, NoLabelSet},
        metrics::{MetricType, TypedMetric, counter::Counter, family::Family},
        registry::Registry,
    },
    std::{
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicI64, AtomicU8, AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
};

/// The content type of [`Metrics::encode`]'s text.
pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// Bucket bounds, in seconds, of the per-block histograms.
pub const BLOCK_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0,
];

/// Bucket bounds, in seconds, of the API request histogram.
pub const REQUEST_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The default of `READY_MAX_LAG_BLOCKS`.
pub const DEFAULT_READY_MAX_LAG_BLOCKS: u64 = 2;

/// The default of `READY_MAX_TIP_AGE_SECS`.
pub const DEFAULT_READY_MAX_TIP_AGE_SECS: u64 = 600;

/// The metrics of the process.
pub fn global() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}

/// Where the process is in its life, for readiness. The phase belongs to the
/// process, so a test that changes it runs in a test binary of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The process has not started its work yet.
    Starting,
    Running,
    /// Shutdown has begun.
    ShuttingDown,
}

impl Phase {
    fn from_u8(value: u8) -> Phase {
        match value {
            0 => Phase::Starting,
            1 => Phase::Running,
            _ => Phase::ShuttingDown,
        }
    }
}

/// A histogram of durations whose buckets, count and sum are atomics.
#[derive(Debug, Clone)]
pub struct AtomicHistogram {
    inner: Arc<HistogramInner>,
}

#[derive(Debug)]
struct HistogramInner {
    bounds: &'static [f64],
    /// One count per bound, then the `+Inf` bucket.
    counts: Box<[AtomicU64]>,
    sum_nanos: AtomicU64,
}

impl AtomicHistogram {
    /// A histogram with `bounds` (ascending, in seconds) and a `+Inf` bucket.
    pub fn new(bounds: &'static [f64]) -> AtomicHistogram {
        AtomicHistogram {
            inner: Arc::new(HistogramInner {
                bounds,
                counts: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
                sum_nanos: AtomicU64::new(0),
            }),
        }
    }

    pub fn observe(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        let bucket = self
            .inner
            .bounds
            .iter()
            .position(|bound| seconds <= *bound)
            .unwrap_or(self.inner.bounds.len());
        self.inner.counts[bucket].fetch_add(1, Ordering::Relaxed);
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.inner.sum_nanos.fetch_add(nanos, Ordering::Relaxed);
    }

    /// Observations so far.
    #[cfg(test)]
    fn count(&self) -> u64 {
        self.inner
            .counts
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .sum()
    }
}

impl TypedMetric for AtomicHistogram {
    const TYPE: MetricType = MetricType::Histogram;
}

impl EncodeMetric for AtomicHistogram {
    fn encode(&self, mut encoder: MetricEncoder) -> Result<(), std::fmt::Error> {
        let buckets: Vec<(f64, u64)> = self
            .inner
            .bounds
            .iter()
            .copied()
            .chain([f64::MAX])
            .zip(self.inner.counts.iter())
            .map(|(bound, count)| (bound, count.load(Ordering::Relaxed)))
            .collect();
        let count = buckets.iter().map(|(_, count)| count).sum();
        let sum = self.inner.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9;
        encoder.encode_histogram::<NoLabelSet>(sum, count, &buckets, None)
    }

    fn metric_type(&self) -> MetricType {
        Self::TYPE
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ModuleLabels {
    module: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RouteLabels {
    module: String,
    route: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RequestLabels {
    module: String,
    route: String,
    status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct EventLabels {
    event: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct FillLabels {
    module: String,
    fill: String,
}

type HistogramFamily<S> = Family<S, AtomicHistogram, fn() -> AtomicHistogram>;

fn block_histogram() -> AtomicHistogram {
    AtomicHistogram::new(BLOCK_BUCKETS)
}

fn request_histogram() -> AtomicHistogram {
    AtomicHistogram::new(REQUEST_BUCKETS)
}

/// A database pool's connections, as a pool probe reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    pub size: u32,
    pub idle: u32,
}

impl PoolStats {
    /// The stats of a pool with `size` open connections, `idle` of them idle
    /// (sqlx's `size()` and `num_idle()`).
    pub fn new(size: u32, idle: usize) -> PoolStats {
        PoolStats {
            size,
            idle: u32::try_from(idle).unwrap_or(u32::MAX),
        }
    }
}

type PoolProbe = Arc<dyn Fn() -> PoolStats + Send + Sync>;

/// An unknown height or tip.
const UNKNOWN: i64 = -1;

/// The time of no tip read, in [`State::tip_read_nanos`].
const NEVER: u64 = u64::MAX;

struct State {
    phase: AtomicU8,
    chain_tip: AtomicI64,
    /// When the chain tip was last read, in nanoseconds since `created`.
    tip_read_nanos: AtomicU64,
    created: Instant,
    modules: Mutex<Vec<(&'static str, Arc<ModuleState>)>>,
    fills: Mutex<Vec<(&'static str, &'static str, Arc<FillState>)>>,
    /// Pools read on each scrape, such as the API's.
    pools: Mutex<Vec<(&'static str, PoolProbe)>>,
    /// The open streams of the API's push feed, [`UNKNOWN`] before the
    /// first.
    stream_subscribers: AtomicI64,
}

impl Default for State {
    fn default() -> Self {
        State {
            phase: AtomicU8::new(0),
            chain_tip: AtomicI64::new(UNKNOWN),
            tip_read_nanos: AtomicU64::new(NEVER),
            created: Instant::now(),
            modules: Mutex::default(),
            fills: Mutex::default(),
            pools: Mutex::default(),
            stream_subscribers: AtomicI64::new(UNKNOWN),
        }
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").finish_non_exhaustive()
    }
}

impl State {
    fn chain_tip(&self) -> Option<ChainTip> {
        let height = known(self.chain_tip.load(Ordering::Relaxed))?;
        let read = self.tip_read_nanos.load(Ordering::Relaxed);
        if read == NEVER {
            return None;
        }
        let age = self
            .created
            .elapsed()
            .saturating_sub(Duration::from_nanos(read));
        Some(ChainTip { height, age })
    }
}

fn known(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

#[derive(Debug)]
struct FillState {
    /// How many guards of the fill are alive.
    running: AtomicI64,
    progress: AtomicI64,
    target: AtomicI64,
}

/// The chain tip the modules follow, and how long ago it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainTip {
    pub height: u64,
    pub age: Duration,
}

/// The bounds of [`indexer_readiness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyLimits {
    /// The most blocks a module may be behind the tip.
    pub max_lag_blocks: u64,
    /// The oldest a tip read may be.
    pub max_tip_age: Duration,
}

impl Default for ReadyLimits {
    fn default() -> Self {
        ReadyLimits {
            max_lag_blocks: DEFAULT_READY_MAX_LAG_BLOCKS,
            max_tip_age: Duration::from_secs(DEFAULT_READY_MAX_TIP_AGE_SECS),
        }
    }
}

/// The metrics of one process.
pub struct Metrics {
    registry: Registry,
    state: Arc<State>,
    block_seconds: HistogramFamily<ModuleLabels>,
    engine_seconds: HistogramFamily<ModuleLabels>,
    api_requests: Family<RequestLabels, Counter>,
    api_request_seconds: HistogramFamily<RouteLabels>,
    api_rate_limited: Family<ModuleLabels, Counter>,
    stream_messages: Family<EventLabels, Counter>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}

impl Metrics {
    /// An empty set of metrics in [`Phase::Starting`]; a process uses
    /// [`global`].
    pub fn new() -> Metrics {
        let state = Arc::new(State::default());
        let block_seconds = HistogramFamily::new_with_constructor(block_histogram as fn() -> _);
        let engine_seconds = HistogramFamily::new_with_constructor(block_histogram as fn() -> _);
        let api_requests = Family::default();
        let api_request_seconds =
            HistogramFamily::new_with_constructor(request_histogram as fn() -> _);
        let api_rate_limited = Family::default();

        let mut registry = Registry::with_prefix("opi");
        registry.register(
            "block_seconds",
            "Seconds a module takes to apply one block",
            block_seconds.clone(),
        );
        registry.register(
            "engine_seconds",
            "Seconds of one block spent in the BRC2.0 engine, for blocks that call it",
            engine_seconds.clone(),
        );
        registry.register(
            "api_requests",
            "API requests answered, by route pattern and status",
            api_requests.clone(),
        );
        registry.register(
            "api_request_seconds",
            "Seconds the API takes to answer a request, by route pattern",
            api_request_seconds.clone(),
        );
        registry.register(
            "api_rate_limited",
            "API requests the rate limiter rejected",
            api_rate_limited.clone(),
        );
        let stream_messages = Family::default();
        registry.register(
            "stream_messages",
            "Messages the API's push feed sent, by event",
            stream_messages.clone(),
        );
        registry.register_collector(Box::new(StateCollector(state.clone())));
        Metrics {
            registry,
            state,
            block_seconds,
            engine_seconds,
            api_requests,
            api_request_seconds,
            api_rate_limited,
            stream_messages,
        }
    }

    /// Counts a push feed subscriber until the guard drops.
    pub fn stream_subscriber(&self) -> StreamSubscriberGuard {
        let subscribers = &self.state.stream_subscribers;
        if subscribers
            .compare_exchange(UNKNOWN, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            subscribers.fetch_add(1, Ordering::SeqCst);
        }
        StreamSubscriberGuard {
            state: self.state.clone(),
        }
    }

    /// The push feed's open streams, `None` before the first.
    pub fn stream_subscribers(&self) -> Option<u64> {
        known(self.state.stream_subscribers.load(Ordering::SeqCst))
    }

    /// Counts a push feed message of `event` (`block`, `rollback`,
    /// `heartbeat` or `error`).
    pub fn stream_message(&self, event: &str) {
        self.stream_messages
            .get_or_create(&EventLabels {
                event: event.to_string(),
            })
            .inc();
    }

    /// The text exposition of every metric.
    pub fn encode(&self) -> String {
        let mut text = String::new();
        prometheus_client::encoding::text::encode(&mut text, &self.registry)
            .expect("writing to a String does not fail");
        text
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.state.phase.load(Ordering::SeqCst))
    }

    /// Moves the process to `phase`. Once shutdown has begun the phase
    /// stays [`Phase::ShuttingDown`].
    pub fn set_phase(&self, phase: Phase) {
        let value = match phase {
            Phase::Starting => 0,
            Phase::Running => 1,
            Phase::ShuttingDown => 2,
        };
        self.state.phase.fetch_max(value, Ordering::SeqCst);
    }

    /// The handle of module `name`, registering the module on first use. A
    /// registered module counts towards [`Metrics::indexer_readiness`].
    pub fn module(&self, name: &'static str) -> ModuleMetrics {
        let mut modules = self.state.modules.lock().expect("the metrics lock");
        let state = match modules.iter().find(|(module, _)| *module == name) {
            Some((_, state)) => state.clone(),
            None => {
                let state = Arc::new(ModuleState::default());
                modules.push((name, state.clone()));
                state
            }
        };
        let labels = ModuleLabels {
            module: name.to_string(),
        };
        ModuleMetrics {
            state,
            block_seconds: self.block_seconds.get_or_create(&labels).clone(),
        }
    }

    /// The histogram of `module`'s engine seconds per block.
    pub fn engine_seconds(&self, module: &str) -> AtomicHistogram {
        self.engine_seconds
            .get_or_create(&ModuleLabels {
                module: module.to_string(),
            })
            .clone()
    }

    /// Records a successful read of the chain tip the modules follow:
    /// bitcoind's best block, or the OPI network's best verified block in
    /// light mode.
    pub fn set_chain_tip(&self, height: u32) {
        let state = &self.state;
        let now = u64::try_from(state.created.elapsed().as_nanos()).unwrap_or(NEVER - 1);
        state.chain_tip.store(i64::from(height), Ordering::Relaxed);
        state.tip_read_nanos.store(now, Ordering::Relaxed);
    }

    /// The last chain tip read, `None` before the first.
    pub fn chain_tip(&self) -> Option<ChainTip> {
        self.state.chain_tip()
    }

    /// Marks fill `fill` of `module` running until the guard drops.
    pub fn fill(&self, module: &'static str, fill: &'static str) -> FillGuard {
        let state = {
            let mut fills = self.state.fills.lock().expect("the metrics lock");
            match fills.iter().find(|(m, f, _)| *m == module && *f == fill) {
                Some((_, _, state)) => state.clone(),
                None => {
                    let state = Arc::new(FillState {
                        running: AtomicI64::new(0),
                        progress: AtomicI64::new(UNKNOWN),
                        target: AtomicI64::new(UNKNOWN),
                    });
                    fills.push((module, fill, state.clone()));
                    state
                }
            }
        };
        state.running.fetch_add(1, Ordering::SeqCst);
        state.progress.store(UNKNOWN, Ordering::Relaxed);
        state.target.store(UNKNOWN, Ordering::Relaxed);
        FillGuard { state }
    }

    /// The fills running now, as `(module, fill)`.
    pub fn running_fills(&self) -> Vec<(&'static str, &'static str)> {
        self.state
            .fills
            .lock()
            .expect("the metrics lock")
            .iter()
            .filter(|(_, _, state)| state.running.load(Ordering::SeqCst) > 0)
            .map(|(module, fill, _)| (*module, *fill))
            .collect()
    }

    /// Reports `module`'s database pool through `probe` on each scrape,
    /// in place of an earlier probe of the same module.
    pub fn set_pool(
        &self,
        module: &'static str,
        probe: impl Fn() -> PoolStats + Send + Sync + 'static,
    ) {
        let mut pools = self.state.pools.lock().expect("the metrics lock");
        pools.retain(|(m, _)| *m != module);
        pools.push((module, Arc::new(probe)));
    }

    /// Counts an API request that `route` (a route pattern) answered with
    /// `status` after `elapsed`.
    pub fn api_request(&self, module: &str, route: &str, status: u16, elapsed: Duration) {
        self.api_requests
            .get_or_create(&RequestLabels {
                module: module.to_string(),
                route: route.to_string(),
                status,
            })
            .inc();
        self.api_request_seconds
            .get_or_create(&RouteLabels {
                module: module.to_string(),
                route: route.to_string(),
            })
            .observe(elapsed);
    }

    /// Counts an API request to `module` the rate limiter rejected.
    pub fn api_rate_limited(&self, module: &str) {
        self.api_rate_limited
            .get_or_create(&ModuleLabels {
                module: module.to_string(),
            })
            .inc();
    }

    /// Every registered module and its indexed height.
    pub fn module_heights(&self) -> Vec<(&'static str, Option<u64>)> {
        self.state
            .modules
            .lock()
            .expect("the metrics lock")
            .iter()
            .map(|(name, state)| (*name, known(state.height.load(Ordering::Relaxed))))
            .collect()
    }

    /// Whether `opi-indexer run` (or `brc20 light`) is ready to serve, with
    /// the reason it is not (see [`indexer_readiness`]).
    pub fn indexer_readiness(&self, limits: ReadyLimits) -> Result<(), String> {
        indexer_readiness(
            self.phase(),
            self.chain_tip(),
            &self.module_heights(),
            &self.running_fills(),
            limits,
        )
    }
}

/// Ready while the process runs, no fill runs, the chain tip was read within
/// `limits.max_tip_age`, at least one module is registered, and every module
/// has a height within `limits.max_lag_blocks` of the tip. A module above the
/// tip counts as caught up.
pub fn indexer_readiness(
    phase: Phase,
    tip: Option<ChainTip>,
    modules: &[(&str, Option<u64>)],
    running_fills: &[(&str, &str)],
    limits: ReadyLimits,
) -> Result<(), String> {
    match phase {
        Phase::Starting => return Err("starting".into()),
        Phase::ShuttingDown => return Err("shutting down".into()),
        Phase::Running => {}
    }
    if let Some((module, fill)) = running_fills.first() {
        return Err(format!("{module} is running its {fill} fill"));
    }
    let Some(ChainTip { height: tip, age }) = tip else {
        return Err("the chain tip is not known yet".into());
    };
    if age > limits.max_tip_age {
        return Err(format!(
            "the chain tip was last read {} s ago",
            age.as_secs()
        ));
    }
    if modules.is_empty() {
        return Err("no module is running".into());
    }
    let max_lag = limits.max_lag_blocks;
    for (module, height) in modules {
        let Some(height) = height else {
            return Err(format!("{module} has not reported a height yet"));
        };
        let lag = tip.saturating_sub(*height);
        if lag > max_lag {
            return Err(format!(
                "{module} is {lag} blocks behind the tip {tip}, more than {max_lag}"
            ));
        }
    }
    Ok(())
}

/// A module's metrics handle.
#[derive(Debug, Clone)]
pub struct ModuleMetrics {
    state: Arc<ModuleState>,
    block_seconds: AtomicHistogram,
}

/// A module's gauges, [`UNKNOWN`] until recorded.
#[derive(Debug)]
struct ModuleState {
    height: AtomicI64,
    pool_size: AtomicI64,
    pool_idle: AtomicI64,
}

impl Default for ModuleState {
    fn default() -> Self {
        ModuleState {
            height: AtomicI64::new(UNKNOWN),
            pool_size: AtomicI64::new(UNKNOWN),
            pool_idle: AtomicI64::new(UNKNOWN),
        }
    }
}

impl ModuleMetrics {
    /// Records the last block the module has applied; `None` before the
    /// first, or while the module is down.
    pub fn set_height(&self, height: Option<u32>) {
        let value = height.map_or(UNKNOWN, i64::from);
        self.state.height.store(value, Ordering::Relaxed);
    }

    /// Records the module's database pool connections.
    pub fn set_pool(&self, stats: PoolStats) {
        self.state
            .pool_size
            .store(i64::from(stats.size), Ordering::Relaxed);
        self.state
            .pool_idle
            .store(i64::from(stats.idle), Ordering::Relaxed);
    }

    /// Records the time one block took to apply.
    pub fn observe_block(&self, elapsed: Duration) {
        self.block_seconds.observe(elapsed);
    }
}

/// An open push feed stream, counted in `opi_stream_subscribers` while the
/// guard is alive.
#[derive(Debug)]
pub struct StreamSubscriberGuard {
    state: Arc<State>,
}

impl Drop for StreamSubscriberGuard {
    fn drop(&mut self) {
        self.state.stream_subscribers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A running fill. The fill runs while any of its guards is alive.
#[derive(Debug)]
pub struct FillGuard {
    state: Arc<FillState>,
}

impl FillGuard {
    /// Records how far the fill has come, in the unit its target uses.
    pub fn progress(&self, value: i64) {
        self.state.progress.store(value, Ordering::Relaxed);
    }

    /// Records where the fill ends.
    pub fn target(&self, value: i64) {
        self.state.target.store(value, Ordering::Relaxed);
    }
}

impl Drop for FillGuard {
    fn drop(&mut self) {
        self.state.running.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The recorded values of `field` of each labelled state, leaving out
/// [`UNKNOWN`] ones.
fn known_samples<L: Clone, T>(
    states: &[(L, Arc<T>)],
    field: fn(&T) -> &AtomicI64,
) -> Vec<(L, i64)> {
    states
        .iter()
        .filter_map(|(labels, state)| {
            let value = field(state).load(Ordering::Relaxed);
            (value != UNKNOWN).then(|| (labels.clone(), value))
        })
        .collect()
}

/// Encodes the gauges read from [`State`] on each scrape.
#[derive(Debug)]
struct StateCollector(Arc<State>);

impl StateCollector {
    fn gauge<S: EncodeLabelSet>(
        encoder: &mut DescriptorEncoder,
        name: &str,
        help: &str,
        samples: &[(S, i64)],
    ) -> Result<(), std::fmt::Error> {
        let mut metric = encoder.encode_descriptor(name, help, None, MetricType::Gauge)?;
        for (labels, value) in samples {
            metric.encode_family(labels)?.encode_gauge(value)?;
        }
        Ok(())
    }
}

impl Collector for StateCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let state = &self.0;
        let tip = state.chain_tip();
        if let Some(tip) = tip {
            let mut metric = encoder.encode_descriptor(
                "chain_tip_height",
                "Height of the chain tip the modules follow",
                None,
                MetricType::Gauge,
            )?;
            metric.encode_gauge(&(tip.height as i64))?;
            let mut metric = encoder.encode_descriptor(
                "chain_tip_age_seconds",
                "Seconds since the chain tip was last read",
                None,
                MetricType::Gauge,
            )?;
            metric.encode_gauge(&tip.age.as_secs_f64())?;
        }

        let module_states: Vec<(ModuleLabels, Arc<ModuleState>)> = state
            .modules
            .lock()
            .expect("the metrics lock")
            .iter()
            .map(|(name, state)| {
                (
                    ModuleLabels {
                        module: name.to_string(),
                    },
                    state.clone(),
                )
            })
            .collect();
        let modules = known_samples(&module_states, |state| &state.height);
        Self::gauge(
            &mut encoder,
            "indexed_height",
            "Last block a module has applied",
            &modules,
        )?;
        if let Some(tip) = tip {
            let lags: Vec<(ModuleLabels, i64)> = modules
                .iter()
                .map(|(labels, height)| {
                    (
                        labels.clone(),
                        tip.height.saturating_sub(*height as u64) as i64,
                    )
                })
                .collect();
            Self::gauge(
                &mut encoder,
                "lag_blocks",
                "Blocks a module is behind the chain tip",
                &lags,
            )?;
        }

        let fills: Vec<(FillLabels, Arc<FillState>)> = state
            .fills
            .lock()
            .expect("the metrics lock")
            .iter()
            .map(|(module, fill, state)| {
                (
                    FillLabels {
                        module: module.to_string(),
                        fill: fill.to_string(),
                    },
                    state.clone(),
                )
            })
            .collect();
        Self::gauge(
            &mut encoder,
            "fill_running",
            "How many runs of a start-up fill are in progress; 0 once it has finished",
            &known_samples(&fills, |state| &state.running),
        )?;
        Self::gauge(
            &mut encoder,
            "fill_progress",
            "How far a start-up fill has come: the last block filled, or for evm_addresses the events read",
            &known_samples(&fills, |state| &state.progress),
        )?;
        Self::gauge(
            &mut encoder,
            "fill_target",
            "The block a start-up fill ends at, when it is known",
            &known_samples(&fills, |state| &state.target),
        )?;

        let pools: Vec<(ModuleLabels, PoolStats)> = state
            .pools
            .lock()
            .expect("the metrics lock")
            .iter()
            .map(|(module, probe)| {
                (
                    ModuleLabels {
                        module: module.to_string(),
                    },
                    probe(),
                )
            })
            .collect();
        let mut sizes = known_samples(&module_states, |state| &state.pool_size);
        sizes.extend(
            pools
                .iter()
                .map(|(labels, stats)| (labels.clone(), i64::from(stats.size))),
        );
        let mut idle = known_samples(&module_states, |state| &state.pool_idle);
        idle.extend(
            pools
                .iter()
                .map(|(labels, stats)| (labels.clone(), i64::from(stats.idle))),
        );
        Self::gauge(
            &mut encoder,
            "db_pool_connections",
            "Open connections of a module's database pool",
            &sizes,
        )?;
        Self::gauge(
            &mut encoder,
            "db_pool_idle_connections",
            "Idle connections of a module's database pool",
            &idle,
        )?;
        let subscribers = state.stream_subscribers.load(Ordering::SeqCst);
        if subscribers != UNKNOWN {
            let mut metric = encoder.encode_descriptor(
                "stream_subscribers",
                "Open streams of the API's push feed",
                None,
                MetricType::Gauge,
            )?;
            metric.encode_gauge(&subscribers)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(height: u64) -> Option<ChainTip> {
        Some(ChainTip {
            height,
            age: Duration::from_secs(1),
        })
    }

    fn limits(max_lag_blocks: u64) -> ReadyLimits {
        ReadyLimits {
            max_lag_blocks,
            ..ReadyLimits::default()
        }
    }

    fn ready(
        tip: Option<ChainTip>,
        modules: &[(&str, Option<u64>)],
        fills: &[(&str, &str)],
    ) -> Result<(), String> {
        indexer_readiness(Phase::Running, tip, modules, fills, limits(2))
    }

    #[test]
    fn modules_within_the_lag_are_ready() {
        assert_eq!(
            ready(tip(100), &[("brc20", Some(100)), ("bitmap", Some(98))], &[]),
            Ok(())
        );
    }

    #[test]
    fn a_module_ahead_of_the_tip_is_ready() {
        assert_eq!(ready(tip(100), &[("brc20", Some(101))], &[]), Ok(()));
    }

    #[test]
    fn a_module_beyond_the_lag_is_not_ready() {
        let err = ready(tip(100), &[("brc20", Some(100)), ("sns", Some(97))], &[]).unwrap_err();
        assert!(err.contains("sns is 3 blocks behind"), "{err}");
    }

    #[test]
    fn a_module_without_a_height_is_not_ready() {
        let err = ready(tip(100), &[("runes", None)], &[]).unwrap_err();
        assert!(err.contains("runes has not reported"), "{err}");
    }

    #[test]
    fn an_unknown_tip_is_not_ready() {
        let err = ready(None, &[("brc20", Some(100))], &[]).unwrap_err();
        assert!(err.contains("tip"), "{err}");
    }

    #[test]
    fn a_stale_tip_is_not_ready() {
        let stale = |secs| {
            indexer_readiness(
                Phase::Running,
                Some(ChainTip {
                    height: 100,
                    age: Duration::from_secs(secs),
                }),
                &[("brc20", Some(100))],
                &[],
                ReadyLimits::default(),
            )
        };
        assert_eq!(stale(600), Ok(()));
        assert_eq!(
            stale(601),
            Err("the chain tip was last read 601 s ago".to_string())
        );
    }

    #[test]
    fn a_running_fill_is_not_ready() {
        let err = ready(
            tip(100),
            &[("brc20", Some(100))],
            &[("brc20", "prog_balances")],
        )
        .unwrap_err();
        assert!(err.contains("prog_balances"), "{err}");
    }

    #[test]
    fn no_module_is_not_ready() {
        assert!(ready(tip(100), &[], &[]).is_err());
    }

    #[test]
    fn starting_and_shutting_down_are_not_ready() {
        let modules = [("brc20", Some(100))];
        let check = |phase| indexer_readiness(phase, tip(100), &modules, &[], limits(2));
        assert_eq!(check(Phase::Starting), Err("starting".to_string()));
        assert_eq!(check(Phase::ShuttingDown), Err("shutting down".to_string()));
        assert_eq!(check(Phase::Running), Ok(()));
    }

    #[test]
    fn shutting_down_is_final() {
        let metrics = Metrics::new();
        assert_eq!(metrics.phase(), Phase::Starting);
        metrics.set_phase(Phase::Running);
        metrics.set_phase(Phase::ShuttingDown);
        metrics.set_phase(Phase::Running);
        assert_eq!(metrics.phase(), Phase::ShuttingDown);
    }

    #[test]
    fn the_lag_threshold_is_inclusive() {
        let check = |height, max_lag| {
            indexer_readiness(
                Phase::Running,
                tip(100),
                &[("brc20", Some(height))],
                &[],
                limits(max_lag),
            )
        };
        assert!(check(95, 5).is_ok());
        assert!(check(94, 5).is_err());
        assert!(check(99, 0).is_err());
    }

    #[test]
    fn readiness_follows_the_recorded_state() {
        let metrics = Metrics::new();
        let limits = limits(2);
        let brc20 = metrics.module("brc20");
        metrics.set_chain_tip(100);
        brc20.set_height(Some(99));
        assert_eq!(metrics.indexer_readiness(limits), Err("starting".into()));
        metrics.set_phase(Phase::Running);
        assert_eq!(metrics.indexer_readiness(limits), Ok(()));
        brc20.set_height(Some(90));
        assert!(metrics.indexer_readiness(limits).is_err());
        brc20.set_height(Some(99));
        let fill = metrics.fill("brc20", "block_time");
        let second = metrics.fill("brc20", "block_time");
        drop(fill);
        assert!(metrics.indexer_readiness(limits).is_err());
        drop(second);
        assert_eq!(metrics.indexer_readiness(limits), Ok(()));
        // A tip read long ago is stale.
        metrics.state.tip_read_nanos.store(0, Ordering::Relaxed);
        let old = ReadyLimits {
            max_tip_age: Duration::ZERO,
            ..limits
        };
        std::thread::sleep(Duration::from_millis(5));
        let err = metrics.indexer_readiness(old).unwrap_err();
        assert!(err.starts_with("the chain tip was last read"), "{err}");
        metrics.set_phase(Phase::ShuttingDown);
        assert_eq!(
            metrics.indexer_readiness(limits),
            Err("shutting down".into())
        );
    }

    #[test]
    fn the_exposition_lists_every_recorded_metric() {
        let metrics = Metrics::new();
        metrics.set_chain_tip(105);
        let module = metrics.module("bitmap");
        module.set_height(Some(100));
        module.observe_block(Duration::from_millis(30));
        metrics
            .engine_seconds("brc20")
            .observe(Duration::from_millis(7));
        let fill = metrics.fill("brc20", "prog_balances");
        fill.progress(50);
        fill.target(80);
        module.set_pool(PoolStats::new(2, 1));
        metrics.api_request(
            "brc20",
            "/v1/brc20/ticker_info/{ticker}",
            200,
            Duration::from_millis(3),
        );
        metrics.api_rate_limited("brc20");

        let text = metrics.encode();
        for line in [
            "opi_chain_tip_height 105",
            "opi_chain_tip_age_seconds ",
            r#"opi_indexed_height{module="bitmap"} 100"#,
            r#"opi_lag_blocks{module="bitmap"} 5"#,
            r#"opi_block_seconds_count{module="bitmap"} 1"#,
            r#"opi_block_seconds_bucket{le="0.05",module="bitmap"} 1"#,
            r#"opi_engine_seconds_count{module="brc20"} 1"#,
            r#"opi_fill_running{module="brc20",fill="prog_balances"} 1"#,
            r#"opi_fill_progress{module="brc20",fill="prog_balances"} 50"#,
            r#"opi_fill_target{module="brc20",fill="prog_balances"} 80"#,
            r#"opi_db_pool_connections{module="bitmap"} 2"#,
            r#"opi_db_pool_idle_connections{module="bitmap"} 1"#,
            r#"opi_api_requests_total{module="brc20",route="/v1/brc20/ticker_info/{ticker}",status="200"} 1"#,
            r#"opi_api_request_seconds_count{module="brc20",route="/v1/brc20/ticker_info/{ticker}"} 1"#,
            r#"opi_api_rate_limited_total{module="brc20"} 1"#,
        ] {
            assert!(text.contains(line), "{line} missing from\n{text}");
        }
        drop(fill);
        assert!(
            metrics
                .encode()
                .contains(r#"opi_fill_running{module="brc20",fill="prog_balances"} 0"#)
        );
    }

    #[test]
    fn stream_metrics_count_subscribers_and_messages() {
        let metrics = Metrics::new();
        assert!(!metrics.encode().contains("opi_stream_subscribers"));
        let first = metrics.stream_subscriber();
        let second = metrics.stream_subscriber();
        metrics.stream_message("block");
        metrics.stream_message("block");
        metrics.stream_message("heartbeat");
        let text = metrics.encode();
        for line in [
            "opi_stream_subscribers 2",
            r#"opi_stream_messages_total{event="block"} 2"#,
            r#"opi_stream_messages_total{event="heartbeat"} 1"#,
        ] {
            assert!(text.contains(line), "{line} missing from\n{text}");
        }
        drop(first);
        drop(second);
        assert_eq!(metrics.stream_subscribers(), Some(0));
        assert!(metrics.encode().contains("opi_stream_subscribers 0"));
    }

    #[test]
    fn a_new_pool_probe_replaces_the_module_s_old_one() {
        let metrics = Metrics::new();
        metrics.set_pool("sns", || PoolStats::new(1, 1));
        metrics.set_pool("sns", || PoolStats::new(4, 0));
        let text = metrics.encode();
        assert!(
            text.contains(r#"opi_db_pool_connections{module="sns"} 4"#),
            "{text}"
        );
        assert!(
            !text.contains(r#"opi_db_pool_connections{module="sns"} 1"#),
            "{text}"
        );
    }

    #[test]
    fn histogram_buckets_and_sum_follow_the_observations() {
        let histogram = AtomicHistogram::new(&[0.1, 1.0]);
        histogram.observe(Duration::from_millis(50));
        histogram.observe(Duration::from_millis(500));
        histogram.observe(Duration::from_secs(5));
        assert_eq!(histogram.count(), 3);
        let counts: Vec<u64> = histogram
            .inner
            .counts
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .collect();
        assert_eq!(counts, [1, 1, 1]);
        assert_eq!(
            histogram.inner.sum_nanos.load(Ordering::Relaxed),
            5_550_000_000
        );
    }
}
