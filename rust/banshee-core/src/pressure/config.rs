// pressure/config — what Banshee watches, where the lines are, and what each
// dimension is called.
//
// Ten dimensions, not the seven the plan sketched. `memory` split into three
// because ADR-0007's addendum requires exactly that: swap-percent has a moving
// denominator (macOS grows swap files on demand, so the percentage can FALL while
// the machine worsens), and the honest signals are absolute swap bytes, the
// swapin/swapout rate, and free memory. `banshee-0g4` added a fourth memory
// signal — the kernel's own pressure level — the same way, rather than splicing
// an arbiter into the availability replay. One value per dimension keeps the
// hysteresis replay simple and makes the UI able to say WHICH memory signal
// tripped.

use serde::{Deserialize, Serialize};

use super::band::BandSpec;

/// One thing Banshee watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dimension {
    /// MEASURED CPU utilization — the fraction of core-time actually spent
    /// running (`Δ(total − idle) / Δtotal` of the per-core tick counters), a
    /// value in 0.0..=1.0. **Not load-per-core**: the run queue counts runnable
    /// threads, so a churning security agent read 3.26× per core while the cores
    /// sat half idle and Banshee cried "CPU saturation" at a machine doing nothing.
    /// Utilization (`banshee-87l.19`) cannot make that mistake — idle cores read
    /// idle. Load-per-core survives as the labeled secondary in the detail string:
    /// high demand over idle cores is scheduling contention, not saturation.
    Cpu,
    /// The kernel's thermal pressure level (0 nominal … 4 sleeping), read via
    /// `notify_get_state`. The kernel's OWN verdict about heat, like
    /// `KernelPressure` is about memory: a throttled machine runs slower for the
    /// same load figure, so a load red without this beside it is a story with a
    /// missing chapter (`banshee-m5y`: 7.2× and 2.2× per core banded the same;
    /// throttling is one reason the same number means different things).
    Thermal,
    /// Available RAM in bytes (`available_bytes`, the `banshee-cot` fix). Falling
    /// is worse. Advisory since the memory second pass (`banshee-yk8`): the metric is right but its band is
    /// decoration — macOS holds availability near a target, so the value barely
    /// moves and the band never fires while the machine goes from calm to Wailing.
    /// Shown as an informational reading; no longer banded into the level.
    Memory,
    /// Absolute swap bytes in use. **Not a percentage** — see ADR-0007.
    Swap,
    /// `kern.memorystatus_vm_pressure_level` — the kernel's OWN memory verdict
    /// (1 normal, 2 warn, 4 critical). The arbiter the `banshee-cot` incident
    /// named, banded in its own right because it genuinely comes apart from
    /// `available_bytes`: the `banshee-10w` parity run measured WARN while
    /// availability was comfortably green (swap was carrying the pressure).
    KernelPressure,
    /// Swapins + swapouts per second. The honest thrash signal, and the one
    /// `perf-scan` structurally cannot compute from a single invocation.
    Thrash,
    /// Kernel-reported reclaimability, `kern.memorystatus_level`, as a percentage
    /// (100 = plenty free, falling toward 0 as the kernel runs out of room to
    /// reclaim). Distinct from `KernelPressure`'s enum verdict: the two genuinely
    /// come apart — 39% reclaimable while the pressure level read WARN
    /// (2026-09-09). Advisory — a number the kernel keeps near a target, shown but
    /// never banded into the level (`banshee-yk8`).
    KernelFree,
    /// Compressed-memory pool occupancy in bytes (`pages_compressor × page_size`).
    /// The signal behind the "only a reboot resets the compressor" finding (`banshee-xlw`):
    /// a saturated compressor pool is memory the machine cannot
    /// return without a restart. Advisory — it backs a finding, it does not drive
    /// the level.
    Compressor,
    /// Kernel jetsam kills under sustained pressure
    /// (`kern.memorystatus.kill_on_sustained_pressure_count`), measured as the
    /// count of NEW kills across the window. NOT advisory and NOT subject to the
    /// sustain grace: a kill is a confirmed past event, not a threshold spike that
    /// might recover, so any kill rings the top bell at once (`banshee-yk8`).
    Jetsam,
    /// Interactive agent CLI sessions older than the staleness threshold.
    Agents,
    /// Agent helper processes orphaned at `ppid == 1`.
    Orphans,
    /// Managed monitoring/security agents, as a share of one core.
    Corporate,
    /// Free bytes on the data volume. Falling is worse.
    Disk,
    /// Days since boot. Advisory — see `is_advisory`.
    Uptime,
}

/// Every dimension, in a stable order. Used for deterministic tie-breaking, so
/// two evaluations of identical data always name the same dominant source.
pub const ALL_DIMENSIONS: [Dimension; 14] = [
    Dimension::Cpu,
    Dimension::Thermal,
    Dimension::Memory,
    Dimension::Swap,
    Dimension::KernelPressure,
    Dimension::Thrash,
    Dimension::KernelFree,
    Dimension::Compressor,
    Dimension::Jetsam,
    Dimension::Agents,
    Dimension::Orphans,
    Dimension::Corporate,
    Dimension::Disk,
    Dimension::Uptime,
];

/// What a dimension's pressure comes FROM, for the menu bar suffix glyph.
///
/// Several dimensions share a source: swap, thrash and free memory are all the
/// same story to a person deciding what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Cpu,
    Memory,
    Disk,
    /// Agent-session sprawl: stale sessions and the helpers they leak.
    Sprawl,
    /// Centrally-managed agents the user cannot remove.
    Corporate,
    /// Heat: the kernel is throttling the machine to cool it.
    Thermal,
}

impl Source {
    /// The suffix glyph, appended to the level's face at Restless and above.
    ///
    /// Cobwebs for sprawl: things left too long. Deliberately no ghost anywhere —
    /// 👻 is already another of the author's menu-bar apps' icon, and two apps
    /// sharing a glyph defeats the point of having one.
    pub fn glyph(self) -> &'static str {
        match self {
            Source::Cpu => "🔥",
            Source::Memory => "🧠",
            Source::Disk => "💽",
            Source::Sprawl => "🕸️",
            Source::Corporate => "🏢",
            Source::Thermal => "🌡️",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Cpu => "CPU",
            Source::Memory => "memory",
            Source::Disk => "disk",
            Source::Sprawl => "agent sprawl",
            Source::Corporate => "managed agents",
            Source::Thermal => "thermal",
        }
    }
}

/// How a dimension's value should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unit {
    /// A bare dimensionless ratio, e.g. CPU utilization as a fraction 0.0..=1.0.
    /// The human `detail` string renders it as a percentage; the unit only says
    /// "this is a ratio, not bytes or a count".
    Ratio,
    Bytes,
    PerSecond,
    Count,
    Percent,
    Days,
}

impl Dimension {
    pub fn key(self) -> &'static str {
        match self {
            Dimension::Cpu => "cpu",
            Dimension::Thermal => "thermal",
            Dimension::Memory => "memory",
            Dimension::Swap => "swap",
            Dimension::KernelPressure => "kernelPressure",
            Dimension::Thrash => "thrash",
            Dimension::KernelFree => "kernelFree",
            Dimension::Compressor => "compressor",
            Dimension::Jetsam => "jetsam",
            Dimension::Agents => "agents",
            Dimension::Orphans => "orphans",
            Dimension::Corporate => "corporate",
            Dimension::Disk => "disk",
            Dimension::Uptime => "uptime",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dimension::Cpu => "CPU utilization",
            Dimension::Thermal => "Thermal pressure",
            Dimension::Memory => "Available memory",
            Dimension::Swap => "Swap in use",
            Dimension::KernelPressure => "Kernel memory pressure",
            Dimension::Thrash => "Swap thrash",
            Dimension::KernelFree => "Kernel reclaimable memory",
            Dimension::Compressor => "Compressed memory",
            Dimension::Jetsam => "Memory kills",
            Dimension::Agents => "Stale agent sessions",
            Dimension::Orphans => "Orphaned helpers",
            Dimension::Corporate => "Managed agents",
            Dimension::Disk => "Disk headroom",
            Dimension::Uptime => "Uptime",
        }
    }

    pub fn source(self) -> Source {
        match self {
            Dimension::Cpu => Source::Cpu,
            // Its own source, not CPU's: "the machine is hot and throttling" is a
            // different action from "the machine is saturated" (close the lid
            // less, move off the duvet, wait — versus reap the load).
            Dimension::Thermal => Source::Thermal,
            // One story to a person deciding what to do: free memory, swap
            // occupancy, thrash and the kernel's own verdict are all "this
            // machine is out of RAM".
            Dimension::Memory
            | Dimension::Swap
            | Dimension::KernelPressure
            | Dimension::Thrash
            | Dimension::KernelFree
            | Dimension::Compressor
            | Dimension::Jetsam
            | Dimension::Uptime => Source::Memory,
            Dimension::Agents | Dimension::Orphans => Source::Sprawl,
            Dimension::Corporate => Source::Corporate,
            Dimension::Disk => Source::Disk,
        }
    }

    pub fn unit(self) -> Unit {
        match self {
            Dimension::Cpu => Unit::Ratio,
            Dimension::Memory | Dimension::Swap | Dimension::Disk | Dimension::Compressor => {
                Unit::Bytes
            }
            Dimension::Thrash => Unit::PerSecond,
            // The kernel levels are enums (1/2/4; 0–4); Count is the least-wrong
            // rendering hint, and the detail string carries the words.
            Dimension::KernelPressure | Dimension::Thermal => Unit::Count,
            // Jetsam is a count of kills in the window; agents/orphans are counts.
            Dimension::Agents | Dimension::Orphans | Dimension::Jetsam => Unit::Count,
            // KernelFree is the kernel's reclaimability percentage; corporate is a
            // share of one core.
            Dimension::Corporate | Dimension::KernelFree => Unit::Percent,
            Dimension::Uptime => Unit::Days,
        }
    }

    /// Advisory dimensions produce findings but never drive the level or the
    /// dominant source.
    ///
    /// Four are advisory, all for the same reason: the reading is worth SEEING but
    /// banding it into the level would ring a false bell.
    /// - `Uptime`: `perf-scan` calls it "material only alongside high swap"; a
    ///   30-day uptime on its own is not a problem.
    /// - `Memory`: availability is the right metric (`banshee-cot`) but macOS holds
    ///   it near a target, so its band is decoration — it never fires while the
    ///   machine actually degrades (`banshee-yk8`).
    /// - `KernelFree`: the kernel keeps `memorystatus_level` near a target too; a
    ///   number to show beside the kernel's verdict, not a second alarm.
    /// - `Compressor`: occupancy backs the reboot finding (`banshee-xlw`); it is a
    ///   diagnosis, not a threshold the level should escalate on.
    ///
    /// `Jetsam` is deliberately NOT here — a kill is a fact, not a stable reading.
    pub fn is_advisory(self) -> bool {
        matches!(
            self,
            Dimension::Uptime | Dimension::Memory | Dimension::KernelFree | Dimension::Compressor
        )
    }
}

/// Thresholds, hysteresis and timing for the whole model.
///
/// **All configuration.** ADR-0005: thresholds, margins and grace periods are
/// config, not constants — but the code that applies them exists exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PressureConfig {
    pub cpu: BandSpec,
    pub thermal: BandSpec,
    pub memory: BandSpec,
    pub swap: BandSpec,
    pub kernel_pressure: BandSpec,
    pub thrash: BandSpec,
    pub agents: BandSpec,
    pub orphans: BandSpec,
    pub corporate: BandSpec,
    pub disk: BandSpec,
    pub uptime: BandSpec,
    pub kernel_free: BandSpec,
    pub compressor: BandSpec,
    pub jetsam: BandSpec,

    /// How long a dimension must stay Red before the level escalates from
    /// Restless to Wailing. A brief red spike during a build is not a crisis.
    pub red_grace_secs: u64,
    /// How far past its red line one SUSTAINED dimension must be to ring the top
    /// bell (Shrieking) on its own, in severity units — red-band-widths past the
    /// yellow threshold, so it is comparable across dimensions (`banshee-m5y`).
    ///
    /// Before this, magnitude stopped mattering the instant a dimension went red:
    /// a swap reading tens of GB into the red escalated no further than one a hair
    /// past the line — both Wailing. The top level was reachable only by TWO
    /// sustained reds, so a single dimension in genuine runaway (swap 28 GB+,
    /// thrash orders of magnitude over) read the same as a marginal one. 3.0 is two
    /// band-widths past red: swap ~28 GB, well above the yellow/red 4/12 GB line
    /// and below the 2026-08-04 incident's 35.6 GB.
    ///
    /// The SUSTAINED requirement is load-bearing: severity tracks the current
    /// hysteresis-replayed value, so a transient spike is gone before it holds past
    /// grace, and a recovering dimension de-escalates out of this on its own. That
    /// is what keeps this from re-introducing the false-Shrieking-on-a-spike shape (`banshee-s6s`).
    /// Bounded dimensions cannot reach it at all — CPU utilization
    /// tops out at severity 1.5 (`banshee-87l.19`), so a machine merely pegged at
    /// 100% busy stays Wailing, which is correct: a build maxing the cores is not a
    /// two-dimension crisis.
    pub catastrophic_severity: f64,
    /// How many trailing samples the model reads. At the 15s cheap-tier cadence
    /// 40 samples is ten minutes — enough for a trend, short enough to react.
    pub window_samples: usize,
    /// Minimum samples before the model will report a level at all. Below this it
    /// reports `Checking`.
    pub min_samples: usize,
    /// How long a dimension must have been red — CONTINUOUSLY observed
    /// (`held_secs`) — before an alert episode opens (ADR-0009). Netdata's
    /// `delay: up`, Prometheus's `for`. Defaults to the red grace period, so an
    /// episode opens as the level reaches Wailing; set it higher to be told later,
    /// lower to record spikes the glyph does not escalate on.
    pub episode_up_secs: u64,
    /// How long a dimension must stay BELOW red before its episode closes and the
    /// one recovery notice goes out — Prometheus's `keep_firing_for`, Netdata's
    /// `delay: down`. A red that returns inside this window REOPENS the same
    /// episode, so a flap is one incident.
    pub episode_down_secs: u64,
    /// How often a still-firing episode re-notifies. The point-event model's
    /// per-dimension cooldown, kept at the same value so notification volume is
    /// unchanged; re-fires swallowed inside it are COUNTED (`suppressed`).
    pub episode_repeat_secs: u64,
    /// A `Corporate` reading derived from a process younger than this is not
    /// banded. Measured: a log shipper restarted 109s earlier reported 14.6% off
    /// 16s of CPU, because the ratio's denominator was tiny.
    pub corporate_min_life_secs: u64,
    /// How many consumers a finding names in its who-line. Two or three
    /// is the useful range: the top of the list is the action, and a fourth name
    /// is where the reader stops reading. Also the CPU rule's minimum-lifetime
    /// guard reuses `corporate_min_life_secs` — a per-program cumulative-CPU
    /// ratio is the same arithmetic as the managed-agent one and lies the same
    /// way over a short life.
    pub who_limit: usize,
    /// The disk projection at which time-to-full alone forces the band RED and
    /// the unit of the time severity term (`banshee-3sn`): severity is 1.0 with
    /// this long to full, 2.0 at half of it, 3.0 (catastrophic) at a third. One
    /// value on purpose, so the forced red and severity 1.0 cannot disagree
    /// about where "red by time" begins.
    pub disk_projection_red_secs: f64,
    /// The disk projection at which time-to-full forces at least YELLOW. The
    /// 2026-09-15 descent spent 40 minutes at "full in ~1 h" inside the yellow
    /// byte band — this is the line that would have coloured it.
    pub disk_projection_yellow_secs: f64,
    /// Projection thresholds that RE-NOTIFY an open disk episode when crossed,
    /// bypassing `episode_repeat_secs` (largest first). A projection collapsing
    /// from 5.8 hours to 22 minutes inside one episode is new news, the same way
    /// a level rise is; the 2026-09-15 episode said it once at minute two and
    /// then went silent.
    pub disk_projection_notify_secs: Vec<u64>,
    /// Load-per-core above which the CPU detail names SCHEDULING CONTENTION when
    /// measured utilization is still below its yellow line (`banshee-87l.19`). A
    /// run queue this deep over cores that are not busy is threads queueing for a
    /// lock or a churning background/security agent, not compute the user asked
    /// for — so the detail says so, in different words and with no fire glyph,
    /// instead of the old false "CPU saturation". `perf-scan`'s ">3× per core is
    /// SATURATED" heuristic, kept only as the demand threshold it always should
    /// have been: a description of the run queue, cross-checked against whether the
    /// cores are actually doing work.
    pub cpu_contention_demand_per_core: f64,

    /// Expected spacing of the sample tier, in seconds.
    ///
    /// The model needs this to tell a MISSED TICK from a GAP. Without it,
    /// `held_secs` counted straight across a hole in the series and the first red
    /// reading after any restart or laptop wake looked "sustained" — escalating to
    /// Wailing or Shrieking on the second sample (found by running
    /// the installed daemon).
    ///
    /// `SamplerConfig::from_env` overwrites this with the interval the sampler
    /// actually resolved, so a cadence override cannot leave the two disagreeing.
    pub sample_interval_secs: u64,
    /// Expected spacing of the census tier, in seconds. Its dimensions
    /// (`agents`, `orphans`, `corporate`) are legitimately ~20× further apart than
    /// the sample tier's, which is exactly why one gap threshold cannot serve both.
    pub census_interval_secs: u64,
    /// How many times its tier's cadence an interval may reach before it counts as
    /// a GAP that breaks a run.
    ///
    /// 3.0 tolerates a missed tick or two — which happens routinely, since the
    /// sampler uses `MissedTickBehavior::Delay` — while refusing to claim
    /// continuity across a hole. At the 15s cadence that is a 45s ceiling; at the
    /// census tier's 300s, 900s.
    pub gap_tolerance: f64,
    /// How far back the thrash dimension looks for its peak, in seconds.
    ///
    /// Thrash is a SPIKE TRAIN, not a level: one afternoon measured 3, 24, 257,
    /// 1641, 1706, 2329, 4145 and 6394 ops/sec, swinging ~70× within minutes. Banding
    /// on the newest interval meant the verdict read GREEN at 24 ops/sec six minutes
    /// after an alert fired RED at 1706 — hysteresis could not save it, because
    /// de-escalation needs only `confirm_samples` (2, i.e. ~30s) and the reported
    /// `value` was the newest sample. So a storm inside the window was discarded
    /// while the machine still felt terrible.
    ///
    /// 300s at the 15s cadence is 20 intervals. The statistic is an upper
    /// percentile rather than the max: see `windowed_peak`.
    pub thrash_peak_window_secs: u64,

    /// Memory the headroom read budgets per concurrent worker (ADR-0010):
    /// `memSlots = floor((availableBytes − reserve) / this)`. 2 GiB: a coding
    /// agent worker measures 0.4–0.9 GB before it runs anything and then spawns a
    /// build that uses every core; Gentoo's documented compile budget is 1.5–2 GB
    /// per job. Decided 2026-09-08 on published prior art (1 GiB was the low end of
    /// every published figure). Env: `BANSHEE_HEADROOM_SLOT_BYTES`.
    pub headroom_memory_per_slot_bytes: u64,
    /// The share of TOTAL RAM the headroom read never lets agents fill — the
    /// OS's and the human's. Subtracted from available memory before dividing by
    /// the per-worker budget. 0.25: Bazel keeps a third of the host for itself,
    /// GKE/AKS/EKS reserve 18–25% of a laptop-sized node before scheduling
    /// anything, Gentoo leaves room "so that the system can run the basics".
    /// Apple protects the interactive user with QoS, not idle cores, which is why
    /// this is a memory reserve and there is no core reserve.
    pub headroom_memory_reserve_fraction: f64,
    /// `retryAfterSecs` while the verdict is still `Checking`: how long an agent
    /// should wait before asking again for a verdict that does not exist yet.
    pub headroom_retry_checking_secs: u64,
    /// `retryAfterSecs` when a red on memory, thermal or disk (or a resource
    /// zero) forces a wait BELOW Wailing — a fresh red inside its grace period.
    pub headroom_retry_red_secs: u64,
    /// `retryAfterSecs` at Wailing: one sustained red.
    pub headroom_retry_wailing_secs: u64,
    /// `retryAfterSecs` at Shrieking: two sustained reds — the machine is not
    /// coming back in a minute.
    pub headroom_retry_shrieking_secs: u64,

    /// The `deltas` read's lookback when the caller names none: "what
    /// changed vs an hour ago" is the question a person asks after taking an
    /// action and waiting to see whether it helped.
    pub deltas_default_lookback_secs: u64,
    /// The longest lookback the `deltas` read accepts. Bounded by the raw
    /// retention (ADR-0006): the read compares raw samples and full censuses,
    /// and past 24 hours there are only rollups, which have no census behind
    /// them to name a consumer. Above this is a 400, not a silent clamp.
    pub deltas_max_lookback_secs: u64,
    /// The smallest byte change worth a clause in the `deltas` sentence. Below
    /// it a consumer or a swap/memory move is in the table but not the words —
    /// "Slack grew 40 MB" is noise, and a sentence that names noise stops being
    /// read. 100 MB: a tenth of the smallest change anyone acts on.
    pub deltas_min_consumer_bytes: u64,
}

impl Default for PressureConfig {
    fn default() -> Self {
        Self {
            // MEASURED utilization, a fraction 0.0..=1.0 (`banshee-87l.19`). 85%
            // busy sustained is yellow — the machine is working hard but keeping
            // up; 95% is red, genuine saturation where cores are pegged. The 0.05
            // margin means utilization must fall back below 80% to leave yellow.
            // This is what makes the false red impossible: idle cores read idle no
            // matter how long the run queue is. Load-per-core, which used to band
            // here at rising(1.0, 3.0, …), is now the labeled secondary in the
            // detail — a high run queue over idle cores is scheduling contention,
            // reported calmly, not a compute alarm.
            cpu: BandSpec::rising(0.85, 0.95, 0.05, 2),

            // The kernel's thermal enum on its own scale: MODERATE (1, Apple's
            // "fair": fans audible, corrective action begun) is yellow; HEAVY (2,
            // Apple's "serious": fans at maximum, performance impacted) and above
            // is red. Hot turns its menu bar orange the moment the level leaves
            // nominal; MacThrottle notifies at heavy. The 0.5 margin means the
            // level must actually drop back to nominal to leave yellow.
            thermal: BandSpec::rising(1.0, 2.0, 0.5, 2),

            // AVAILABLE RAM: free + speculative + purgeable + file cache
            // (`SysReading::available_bytes`), NOT the free list — banding on
            // `pages_free` was the `banshee-cot` false-positive bug (red at
            // 84 MB "free" while the kernel's pressure level said NORMAL and
            // 9+ GB was reclaimable). These thresholds were always CHOSEN as
            // if the value meant availability; now the value does. Measured
            // healthy on this machine 2026-09-01: ~9.3 GB available. During
            // the 2026-08-31 swap crisis, availability genuinely collapsed —
            // the reds that mattered stay red under the new source.
            memory: BandSpec::falling(2e9, 512e6, 256e6, 2),

            // The kernel's own verdict, on the kernel's own scale: WARN (2) is
            // yellow, CRITICAL (4) is red. NOT tuning knobs in the usual sense —
            // the values are the sysctl's contract — but config like everything
            // else, so what the daemon is using is visible. The 0.5 margin
            // means a level must actually LEAVE 2 (i.e. read 1) to de-escalate,
            // and the confirm count smooths a single-sample flap on top of the
            // kernel's own hysteresis.
            kernel_pressure: BandSpec::rising(2.0, 4.0, 0.5, 2),

            // ABSOLUTE swap bytes, never a percentage (ADR-0007). Measured
            // 7.08 GB then 10.66 GB the same day under load; the 2026-08-04
            // incident sat at 35.6 GB.
            swap: BandSpec::rising(4e9, 12e9, 1e9, 2),

            // swapins + swapouts per second. Measured ~9,300/s combined during a
            // release build — deeply red. Sustained triple digits is already bad.
            thrash: BandSpec::rising(100.0, 1000.0, 50.0, 2),

            // Interactive sessions past the staleness threshold. 2026-08-04 had
            // 17 sessions, several six days old.
            agents: BandSpec::rising(2.0, 6.0, 1.0, 1),

            // Orphaned helpers. 2026-08-04 measured ~45 of them; a healthy
            // machine measured 2.
            orphans: BandSpec::rising(10.0, 40.0, 5.0, 1),

            // Managed agents as a share of ONE core, deduplicated. Measured 49.2%
            // (2026-08-31) against roughly 32% inferred for 2026-08-04, so the
            // thresholds sit above today's normal in order to catch GROWTH rather
            // than to complain permanently about a baseline the user cannot change.
            corporate: BandSpec::rising(60.0, 100.0, 5.0, 2),

            // Free bytes on the data volume. Measured 65–71 GB free; the
            // 2026-07-16 crisis bottomed out at 1.1 GB. Raised from 50/15 after
            // the 2026-09-15 crisis (`banshee-3sn`): red at 15 GB was LESS than
            // the 24 GB of swapfiles the volume was already carrying that day —
            // by the time disk went red, the swap that memory pressure would
            // reach for was already impossible. Ted: anything under 30 GB can be
            // eaten quickly; he is already alarmed at 25.
            disk: BandSpec::falling(60e9, 30e9, 5e9, 2),

            // perf-scan flags >7 days. Advisory only.
            uptime: BandSpec::rising(7.0, 21.0, 1.0, 1),

            // The kernel's reclaimability percentage, falling-is-worse. Advisory:
            // banded only to COLOUR the informational reading, never to drive the
            // level. Measured 39% while the pressure level read WARN (2026-09-09),
            // so ≤30% is yellow and ≤15% is red — below the observed under-pressure
            // reading, so the colour tracks genuine scarcity, not the target macOS
            // holds it near.
            kernel_free: BandSpec::falling(30.0, 15.0, 5.0, 2),

            // Compressed-pool occupancy in bytes, rising-is-worse. Advisory: it
            // backs the reboot finding (`banshee-xlw`) and colours its own reading,
            // it does not escalate the level. On a 24 GB machine several GB held
            // compressed is the "saturated pool" the finding describes; yellow at
            // 4 GB, red at 8 GB.
            compressor: BandSpec::rising(4e9, 8e9, 512e6, 2),

            // Jetsam kills in the window. ANY kill is red, confirmed in a single
            // sample (confirm 1): a kill is a logged fact, not a flapping threshold,
            // so it needs no smoothing and — via the `decide` branch — no sustain
            // grace. The 0.5 yellow line is unreachable in practice (kills are
            // integers); the red line at 1.0 is the one that matters.
            jetsam: BandSpec::rising(0.5, 1.0, 0.5, 1),

            red_grace_secs: 120,
            catastrophic_severity: 3.0,
            window_samples: 40,
            min_samples: 2,
            // Up: the red grace period — the episode opens as the level reaches
            // Wailing. Down: ten minutes, Netdata's customary `down` for a noisy
            // signal, and comfortably longer than the thrash dimension's 300s peak
            // window so a storm's own decay cannot close its episode. Repeat: the
            // old cooldown.
            episode_up_secs: 120,
            episode_down_secs: 600,
            episode_repeat_secs: 3600,
            corporate_min_life_secs: 600,
            who_limit: 3,
            disk_projection_red_secs: 3600.0,
            disk_projection_yellow_secs: 7200.0,
            disk_projection_notify_secs: vec![3600, 1800, 600],
            // perf-scan's "SATURATED" heuristic, repurposed as the run-queue
            // depth that triggers the contention framing. The 2026-09-09 incident
            // sat at 3.26× per core with the cores half idle.
            cpu_contention_demand_per_core: 3.0,

            // The tier cadences live in banshee-core, so the sampler and the model
            // cannot disagree about what "normal spacing" means.
            sample_interval_secs: crate::DEFAULT_SAMPLE_INTERVAL_SECS,
            census_interval_secs: crate::DEFAULT_CENSUS_INTERVAL_SECS,
            gap_tolerance: 3.0,
            thrash_peak_window_secs: 300,

            // The headroom ladder (ADR-0010 decision 5). Checking resolves within
            // a few sample ticks, so 30s; a fresh red is either a spike about to
            // clear or about to become sustained, and 60s is short enough to
            // learn which; Wailing is the red grace period; Shrieking is a
            // machine that needs a person, not a retry loop.
            headroom_memory_per_slot_bytes: 2 << 30,
            headroom_memory_reserve_fraction: 0.25,
            headroom_retry_checking_secs: 30,
            headroom_retry_red_secs: 60,
            headroom_retry_wailing_secs: 120,
            headroom_retry_shrieking_secs: 300,

            deltas_default_lookback_secs: 3600,
            deltas_max_lookback_secs: 86_400,
            deltas_min_consumer_bytes: 100_000_000,
        }
    }
}

impl PressureConfig {
    /// The longest interval that still counts as continuous observation for `d`.
    ///
    /// Per-TIER, because the census legitimately ticks 20× slower than the samples:
    /// a single threshold generous enough for the census (900s) would wave through
    /// a fifteen-minute hole in the sample series, which is the bug this exists to
    /// stop.
    pub fn max_interval_secs(&self, d: Dimension) -> u64 {
        let cadence = match d {
            Dimension::Agents | Dimension::Orphans | Dimension::Corporate => {
                self.census_interval_secs
            }
            _ => self.sample_interval_secs,
        };
        // Round up, and never return 0 — a zero ceiling would treat every interval
        // as a gap and report every run as instantaneous.
        ((cadence as f64) * self.gap_tolerance).ceil().max(1.0) as u64
    }

    pub fn spec(&self, d: Dimension) -> &BandSpec {
        match d {
            Dimension::Cpu => &self.cpu,
            Dimension::Thermal => &self.thermal,
            Dimension::Memory => &self.memory,
            Dimension::Swap => &self.swap,
            Dimension::KernelPressure => &self.kernel_pressure,
            Dimension::Thrash => &self.thrash,
            Dimension::Agents => &self.agents,
            Dimension::Orphans => &self.orphans,
            Dimension::Corporate => &self.corporate,
            Dimension::Disk => &self.disk,
            Dimension::Uptime => &self.uptime,
            Dimension::KernelFree => &self.kernel_free,
            Dimension::Compressor => &self.compressor,
            Dimension::Jetsam => &self.jetsam,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pressure::band::Band;

    #[test]
    fn every_dimension_has_a_spec_a_key_and_a_label() {
        let c = PressureConfig::default();
        let mut keys = Vec::new();
        for d in ALL_DIMENSIONS {
            let _ = c.spec(d);
            assert!(!d.key().is_empty());
            assert!(!d.label().is_empty());
            keys.push(d.key());
        }
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "keys must be unique: {keys:?}");
    }

    /// `ALL_DIMENSIONS` must actually be all of them, or a dimension silently
    /// stops being evaluated. Mutation-proof: delete an entry and this fails.
    #[test]
    fn all_dimensions_covers_every_variant() {
        assert_eq!(ALL_DIMENSIONS.len(), 14);
        for d in [
            Dimension::Cpu,
            Dimension::Thermal,
            Dimension::Memory,
            Dimension::Swap,
            Dimension::KernelPressure,
            Dimension::Thrash,
            Dimension::KernelFree,
            Dimension::Compressor,
            Dimension::Jetsam,
            Dimension::Agents,
            Dimension::Orphans,
            Dimension::Corporate,
            Dimension::Disk,
            Dimension::Uptime,
        ] {
            assert!(ALL_DIMENSIONS.contains(&d), "{d:?} is not evaluated");
        }
    }

    /// The memory story is many signals under one source-glyph, and only some of
    /// them BAND the level. ADR-0007 bands absolute swap bytes, the thrash rate and
    /// the kernel's own pressure level — never swap-percent, whose denominator
    /// moves; `banshee-yk8` adds jetsam kills as a banded (top-bell) signal and
    /// demotes availability to advisory. Availability, the kernel's reclaimability
    /// percentage and compressor occupancy are shown but advisory. Mutation-proof:
    /// collapse the source arm or move Memory out of `is_advisory` and this fails.
    #[test]
    fn the_banded_memory_dimensions_exclude_the_advisory_ones() {
        let banded: Vec<Dimension> = ALL_DIMENSIONS
            .iter()
            .copied()
            .filter(|d| d.source() == Source::Memory && !d.is_advisory())
            .collect();
        assert_eq!(
            banded,
            vec![
                Dimension::Swap,
                Dimension::KernelPressure,
                Dimension::Thrash,
                Dimension::Jetsam,
            ],
            "absolute swap, the kernel's verdict, thrash rate and jetsam kills \
             are the memory signals that drive the level"
        );

        // Availability, kernel reclaimability and compressor occupancy are memory
        // signals too, but advisory — seen, never a bell.
        let advisory_memory: Vec<Dimension> = ALL_DIMENSIONS
            .iter()
            .copied()
            .filter(|d| d.source() == Source::Memory && d.is_advisory())
            .collect();
        assert_eq!(
            advisory_memory,
            vec![
                Dimension::Memory,
                Dimension::KernelFree,
                Dimension::Compressor,
                // Uptime shares the memory source (long uptime + pinned swap =
                // fragmented memory) and is advisory too.
                Dimension::Uptime,
            ]
        );
    }

    /// The inverted dimensions are the ones where less is worse: free space, free
    /// memory, and the kernel's reclaimability percentage. Mutation-proof: make
    /// `disk` rising and a nearly-full volume reports Green.
    #[test]
    fn only_the_free_headroom_dimensions_are_inverted() {
        let c = PressureConfig::default();
        for d in ALL_DIMENSIONS {
            let inverted = c.spec(d).inverted;
            let expect = matches!(
                d,
                Dimension::Memory | Dimension::Disk | Dimension::KernelFree
            );
            assert_eq!(inverted, expect, "{d:?} inverted = {inverted}");
        }
    }

    /// The second-pass memory signals band where they were calibrated: ANY jetsam kill is red
    /// (the 1.0 line), a healthy zero-kill window is green; the kernel's
    /// reclaimability is green at the 39%-under-WARN reading and red only when it
    /// falls below the observed scarcity; and a saturated compressor pool colours
    /// red at several GB while an idle machine's is green. Mutation-proof: flip the
    /// jetsam red line past 1 and a real kill reads green.
    #[test]
    fn the_ws1c_memory_signals_band_where_they_were_calibrated() {
        let c = PressureConfig::default();
        // Jetsam: one kill is red, zero is green.
        assert_eq!(c.jetsam.raw_band(1.0), Band::Red, "any kill is red");
        assert_eq!(c.jetsam.raw_band(0.0), Band::Green, "no kills is green");
        // KernelFree (falling): 39% while WARN was the observed under-pressure
        // reading, so it must be green — the alarm is genuine scarcity below it.
        assert_eq!(c.kernel_free.raw_band(39.0), Band::Green);
        assert_eq!(c.kernel_free.raw_band(25.0), Band::Yellow);
        assert_eq!(c.kernel_free.raw_band(10.0), Band::Red);
        // Compressor (rising, bytes): a few hundred MB is green, several GB red.
        assert_eq!(c.compressor.raw_band(300e6), Band::Green);
        assert_eq!(c.compressor.raw_band(9e9), Band::Red);
    }

    /// The advisory dimensions are exactly the four display-only ones: availability
    /// and the kernel's reclaimability percentage (both held near a target by
    /// macOS), compressor occupancy (backs a finding), and uptime (material only
    /// alongside high swap). Jetsam is NOT advisory — a kill drives the level.
    /// Mutation-proof: remove Memory here and a decorative availability red starts
    /// driving the level again; add Jetsam and a real kill stops ringing the bell.
    #[test]
    fn the_advisory_dimensions_are_the_display_only_ones() {
        let advisory: Vec<Dimension> = ALL_DIMENSIONS
            .iter()
            .copied()
            .filter(|d| d.is_advisory())
            .collect();
        assert_eq!(
            advisory,
            vec![
                Dimension::Memory,
                Dimension::KernelFree,
                Dimension::Compressor,
                Dimension::Uptime,
            ]
        );
        assert!(
            !Dimension::Jetsam.is_advisory(),
            "a jetsam kill must drive the level, not merely inform"
        );
    }

    /// Sanity-check the defaults against the incidents they were derived from.
    /// If someone retunes a threshold past these, the retune is a decision that
    /// should break a test and be argued for.
    #[test]
    fn the_defaults_would_have_caught_the_incidents_they_came_from() {
        let c = PressureConfig::default();

        // Genuine saturation: cores 98% busy. The CPU dimension bands on MEASURED
        // utilization now (`banshee-87l.19`), so red means the cores really are
        // pegged — not that the run queue is long. 95%+ sustained is red.
        assert_eq!(c.cpu.raw_band(0.98), Band::Red);
        // …and 88% busy is yellow: working hard, keeping up, nothing pegged.
        assert_eq!(c.cpu.raw_band(0.88), Band::Yellow);
        // 2026-08-04: swap 35.6 GB used.
        assert_eq!(c.swap.raw_band(35.6e9), Band::Red);
        // 2026-08-31: availability collapsed during the swap crisis (the free
        // list read 217 MB and there was nothing reclaimable behind it —
        // 17.6 GB was out on swap).
        assert_eq!(c.memory.raw_band(217e6), Band::Red);
        // …while a HEALTHY machine's availability must be green: measured
        // 2026-09-01, 9.3 GB available (84 MB "free") while the kernel's
        // pressure level said NORMAL. This is the `banshee-cot` false-positive
        // case, and it is only distinguishable because the value is
        // availability now.
        assert_eq!(c.memory.raw_band(9.3e9), Band::Green);
        // 2026-08-31: ~9,300 swap operations per second under load.
        assert_eq!(c.thrash.raw_band(9300.0), Band::Red);
        // The kernel's scale IS the spec: CRITICAL (4) is red, WARN (2) —
        // the level measured during the banshee-10w parity run while
        // availability read green — is yellow, not a shrug.
        assert_eq!(c.kernel_pressure.raw_band(4.0), Band::Red);
        assert_eq!(c.kernel_pressure.raw_band(2.0), Band::Yellow);
        // 2026-08-04: ~45 orphaned helper processes.
        assert_eq!(c.orphans.raw_band(45.0), Band::Red);
        // 2026-07-16: 1.1 GB free on the data volume.
        assert_eq!(c.disk.raw_band(1.1e9), Band::Red);
    }

    /// …and would NOT be firing on a currently-healthy machine. A monitor that is
    /// permanently yellow is a monitor nobody reads.
    #[test]
    fn the_defaults_are_quiet_on_a_healthy_machine() {
        let c = PressureConfig::default();
        // Figures measured on this machine while idle-ish. 53% utilization is
        // comfortably green — and, critically, this stays green no matter how deep
        // the run queue is, because the value is measured busy-time, not load.
        assert_eq!(c.cpu.raw_band(0.53), Band::Green);
        assert_eq!(c.disk.raw_band(71e9), Band::Green);
        assert_eq!(c.orphans.raw_band(2.0), Band::Green);
        assert_eq!(c.agents.raw_band(0.0), Band::Green);
        // NORMAL (1) is what a healthy kernel reports all day.
        assert_eq!(c.kernel_pressure.raw_band(1.0), Band::Green);
        // 49.2% of one core is today's managed-agent baseline: deliberately
        // Green, so the dimension reports GROWTH rather than complaining forever
        // about something the user cannot remove.
        assert_eq!(
            c.corporate.raw_band(49.2),
            Band::Green,
            "today's baseline must not be a standing warning"
        );
        assert_eq!(c.corporate.raw_band(65.0), Band::Yellow, "but growth trips");
    }

    /// The short-lifetime guard exists because the corporate ratio is noisy for
    /// young processes: a log shipper 109s old reported 14.6% off 16s of CPU.
    #[test]
    fn the_corporate_minimum_lifetime_exceeds_the_observed_noise_case() {
        let c = PressureConfig::default();
        assert!(
            c.corporate_min_life_secs > 109,
            "must exclude the 109s restart that measured 14.6%"
        );
    }

    /// The episode timings must nest sensibly: the down window must outlast the
    /// thrash peak window (or a storm's own decay closes its episode), the repeat
    /// interval must exceed the down window (or "repeat" and "reopen" blur), and
    /// the up delay must not exceed the grace period by default (or the glyph
    /// says Wailing while no episode exists to explain it).
    #[test]
    fn the_episode_timings_nest() {
        let c = PressureConfig::default();
        assert!(c.episode_down_secs > c.thrash_peak_window_secs);
        assert!(c.episode_repeat_secs > c.episode_down_secs);
        assert!(c.episode_up_secs <= c.red_grace_secs);
        assert_eq!(
            c.episode_repeat_secs, 3600,
            "the point-event cooldown, unchanged"
        );
    }

    #[test]
    fn the_window_is_long_enough_for_a_trend_and_short_enough_to_react() {
        let c = PressureConfig::default();
        // At the 15s cadence.
        let window_secs = c.window_samples as u64 * 15;
        assert!(window_secs >= 300, "at least five minutes of history");
        assert!(window_secs <= 1800, "not so long that it lags reality");
        assert!(c.min_samples >= 2, "a rate needs two points");
        assert!(c.min_samples <= c.window_samples);
    }

    #[test]
    fn sources_have_distinct_glyphs_and_no_ghost() {
        let sources = [
            Source::Cpu,
            Source::Memory,
            Source::Disk,
            Source::Sprawl,
            Source::Corporate,
        ];
        let mut glyphs: Vec<&str> = sources.iter().map(|s| s.glyph()).collect();
        let n = glyphs.len();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), n, "source glyphs must be distinct");
        for s in sources {
            assert!(!s.glyph().is_empty());
            assert_ne!(s.glyph(), "👻", "the ghost belongs to another app");
            assert!(!s.label().is_empty());
        }
    }

    /// Every dimension maps to a source, and every source is reachable — an
    /// unreachable source is a glyph nobody will ever see.
    #[test]
    fn every_source_is_reachable_from_some_dimension() {
        for s in [
            Source::Cpu,
            Source::Memory,
            Source::Disk,
            Source::Sprawl,
            Source::Corporate,
        ] {
            assert!(
                ALL_DIMENSIONS.iter().any(|d| d.source() == s),
                "{s:?} has no dimension"
            );
        }
    }

    /// The gap ceiling is PER TIER. A census dimension's normal 300s spacing must
    /// not look like a gap, and a sample dimension must not tolerate one.
    ///
    /// Mutation-proof, verified: return one cadence for every dimension and this
    /// fails — which is the version that waves a 15-minute hole in the sample
    /// series through as continuous observation.
    #[test]
    fn the_gap_ceiling_is_per_tier() {
        let c = PressureConfig::default();
        // Sample tier: 15s × 3 = 45s. A missed tick (30s) is fine; a minute is not.
        assert_eq!(c.max_interval_secs(Dimension::Cpu), 45);
        assert_eq!(c.max_interval_secs(Dimension::Swap), 45);
        assert_eq!(c.max_interval_secs(Dimension::KernelPressure), 45);
        assert_eq!(c.max_interval_secs(Dimension::Disk), 45);
        // Census tier: 300s × 3 = 900s.
        assert_eq!(c.max_interval_secs(Dimension::Agents), 900);
        assert_eq!(c.max_interval_secs(Dimension::Orphans), 900);
        assert_eq!(c.max_interval_secs(Dimension::Corporate), 900);

        // The census ceiling must comfortably exceed one census interval, or every
        // census-derived run would be reported as instantaneous.
        assert!(c.max_interval_secs(Dimension::Agents) > c.census_interval_secs);
        // …and the sample ceiling must be well under one census interval, or a hole
        // the size of a census would count as continuous sampling.
        assert!(c.max_interval_secs(Dimension::Cpu) < c.census_interval_secs);
    }

    /// A zero or absurd tolerance must not make every interval a gap — that would
    /// report every run as instantaneous and make `is_sustained_red` unreachable,
    /// so nothing could ever escalate past Restless.
    #[test]
    fn the_gap_ceiling_is_never_zero() {
        let zero_tolerance = PressureConfig {
            gap_tolerance: 0.0,
            ..PressureConfig::default()
        };
        assert!(zero_tolerance.max_interval_secs(Dimension::Cpu) >= 1);

        let zero_cadence = PressureConfig {
            gap_tolerance: 0.0,
            sample_interval_secs: 0,
            ..PressureConfig::default()
        };
        assert!(zero_cadence.max_interval_secs(Dimension::Cpu) >= 1);
    }

    /// Config round-trips through the wire format, so the app can show and edit
    /// the thresholds the daemon is actually using.
    #[test]
    fn config_round_trips_as_camel_case_json() {
        let c = PressureConfig::default();
        let v = serde_json::to_value(&c).unwrap();
        assert!(
            v.get("redGraceSecs").is_some(),
            "camelCase at the top level"
        );
        assert!(v.get("windowSamples").is_some());
        assert!(v.get("catastrophicSeverity").is_some());
        assert!(v.get("corporateMinLifeSecs").is_some());
        assert!(v.get("cpuContentionDemandPerCore").is_some());
        assert!(v.get("episodeUpSecs").is_some());
        assert!(v.get("episodeDownSecs").is_some());
        assert!(v.get("episodeRepeatSecs").is_some());
        assert!(v.get("sampleIntervalSecs").is_some());
        assert!(v.get("gapTolerance").is_some());
        assert!(v.get("red_grace_secs").is_none(), "snake_case leaked");
        // Nested BandSpec objects too (Five Rules, rule 3).
        assert!(v["cpu"].get("confirmSamples").is_some());
        assert!(v["disk"].get("inverted").is_some());
        assert!(
            v.get("kernelPressure").is_some(),
            "the tenth spec is config"
        );

        let back: PressureConfig = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
    }
}
