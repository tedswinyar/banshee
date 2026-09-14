// sys — the ONLY module in banshee-core that contains `unsafe`, and the only
// one that talks to the kernel. Everything above it consumes the `SysProbe`
// trait, so the sampler is testable without a real machine and the derivation
// logic is testable without either.
//
// The cheap tier deliberately spawns NO subprocesses; see
// docs/adr/0007-syscalls-not-subprocesses.md. A monitor that fork/exec'd six
// times every 15 seconds would bill ~28,800 spawns/day to the event-driven
// endpoint-security agents it is supposed to be measuring, inflating one of its
// own dimensions.

use std::ffi::CString;

use crate::{CoreError, Result};

/// `HOST_VM_INFO64`. Neither `libc` nor `mach2` exports this as a plain
/// constant — `mach2` has only the `_COUNT` derivations — so it lives here.
const HOST_VM_INFO64: libc::c_int = 4;

/// macOS `struct loadavg` (`<sys/resource.h>`). **`libc` does not define this
/// on Apple targets**, so we declare it. `ldavg` is fixed-point and must be
/// divided by `fscale` (2048 in practice); reading it raw yields load figures
/// in the thousands.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct LoadAvg {
    ldavg: [u32; 3],
    fscale: u64,
}

/// One reading of the kernel counters the cheap tier needs.
///
/// Everything here is a RAW kernel value. No banding, no rates, no
/// interpretation — those belong to the pressure model, which must be testable
/// against synthetic values (ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SysReading {
    // Static-per-boot facts. Cheap enough to re-read, and re-reading means a
    // caller can never hold a stale core count.
    pub ncpu: u32,
    pub mem_total_bytes: u64,
    pub page_size: u64,
    /// Unix seconds. A CHANGE in this value means the machine rebooted, which
    /// invalidates every counter delta — see `rates`.
    pub boot_time_secs: i64,

    // Load, already divided by fscale.
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,

    // Swap. `total` is NOT a constant: macOS grows and shrinks swap files on
    // demand (observed 8.59 GB where 36 GB was seen four weeks earlier), so a
    // percentage computed from it has a moving denominator and can FALL while
    // the machine gets worse. Band on `used_bytes` and the swap rates instead.
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,

    // Page counts from host_statistics64. Multiply by `page_size` for bytes.
    pub pages_free: u64,
    pub pages_active: u64,
    pub pages_inactive: u64,
    pub pages_wired: u64,
    pub pages_speculative: u64,
    pub pages_compressor: u64,
    /// Pages an app has marked discardable-under-pressure (caches it can rebuild).
    pub pages_purgeable: u64,
    /// File-backed pages — the page cache. Clean ones drop without I/O.
    pub pages_external: u64,

    // Lifetime monotonic counters. Only their RATE is meaningful.
    pub swapins: u64,
    pub swapouts: u64,

    /// `kern.memorystatus_vm_pressure_level` — the kernel's OWN verdict on its
    /// memory subsystem: 1 = normal, 2 = warn, 4 = critical. This is the arbiter
    /// the `banshee-cot` incident named ("the one signal that cannot disagree
    /// with the kernel"), and the `banshee-10w` parity run measured it at WARN
    /// while `available_bytes` was comfortably green — the two genuinely come
    /// apart, so it is a signal in its own right, not a restatement.
    /// 0 is never returned by the kernel; it marks a pre-v8 stored sample.
    pub memory_pressure_level: u32,

    /// The kernel's thermal pressure level, from
    /// `notify_get_state("com.apple.system.thermalpressurelevel")`: 0 nominal,
    /// 1 moderate, 2 heavy, 3 trapping, 4 sleeping (`OSThermalNotification.h`,
    /// the macOS arm of the enum). Foundation's `ProcessInfo.thermalState`
    /// collapses this to nominal/fair/serious/critical; Hot and MacThrottle build
    /// whole apps on it. The kernel's OWN verdict again, like
    /// `memory_pressure_level` — a throttled machine's load figures understate
    /// the work it is being asked to do, and this is the signal that says so.
    /// `None` = not recorded (a pre-v11 row); 0 is a real reading.
    pub thermal_pressure_level: Option<u32>,
    /// `CPU_Speed_Limit` from `IOPMCopyCPUPowerStatus`, 0–100. **Absent on Apple
    /// silicon** — the call returns `kIOReturnNotFound` there (probed 2026-09-08;
    /// `pmset -g therm` says "No CPU power status has been recorded") — so this is
    /// `None` on every modern Mac and only an Intel Mac ever fills it. Kept
    /// because when it IS present it is the direct measurement of throttling.
    pub cpu_speed_limit_percent: Option<u32>,

    /// Cumulative CPU tick counters, summed across every core
    /// (`host_processor_info(PROCESSOR_CPU_LOAD_INFO)`): `total` is all four
    /// states (user+system+idle+nice), `idle` is the idle state alone. Only
    /// their RATE means anything — utilization is `Δ(total − idle) / Δtotal`
    /// between two samples, which the pressure model computes exactly as it
    /// computes the swap-in/out rate (ADR-0005). **This is the signal the CPU
    /// dimension bands on** (`banshee-87l.19`): a high run queue over idle cores
    /// is scheduler contention, not saturation, and load-per-core alone could not
    /// tell the two apart. `None` = not recorded (a pre-v12 row); the two move
    /// together, so both are Some or both are None.
    pub cpu_ticks_total: Option<u64>,
    pub cpu_ticks_idle: Option<u64>,

    /// `kern.memorystatus_level` — the kernel's OWN reclaimability estimate as a
    /// PERCENTAGE (0–100). This is NOT `memory_pressure_level` and NOT
    /// `available_bytes`: it is the figure `memory_pressure` prints as "free
    /// percentage", and it is a much larger, differently-shaped quantity —
    /// measured 40% here while `available_bytes` computed 11.5% at the same
    /// instant (2026-09-01 parity), and 39% under a WARN pressure level
    /// (2026-09-09). Read beside the pressure level because the two genuinely
    /// come apart. `None` = not recorded (a pre-v13 row); a real reading is
    /// `Some` even at 0.
    pub kernel_free_percent: Option<u32>,

    /// `kern.memorystatus.kill_on_sustained_pressure_count` — a monotonic LIFETIME
    /// counter of processes the kernel has jetsam-killed under sustained memory
    /// pressure. 0 on a healthy machine; a POSITIVE DELTA between two samples means
    /// the kernel killed something to stay alive, which is Shrieking-grade — the
    /// machine already lost a process, it is not a prediction. Reset by a reboot,
    /// so its rate is only meaningful within one boot (`rates` invalidates deltas
    /// across a `boot_time_secs` change). `None` = not recorded (a pre-v13 row).
    pub jetsam_kills: Option<u64>,
}

impl SysReading {
    /// Load average divided by core count — the only form in which a load
    /// figure means anything. `perf-scan`: "raw load average is meaningless
    /// without the core count beside it."
    pub fn load_per_core_1m(&self) -> f64 {
        self.per_core(self.load_1m)
    }

    pub fn load_per_core_5m(&self) -> f64 {
        self.per_core(self.load_5m)
    }

    fn per_core(&self, load: f64) -> f64 {
        if self.ncpu == 0 {
            // A zero core count is impossible from a live kernel, but a mocked
            // or corrupt reading must not produce inf/NaN and poison every
            // downstream band comparison.
            return 0.0;
        }
        load / f64::from(self.ncpu)
    }

    /// Familiar display value only — **do not band on this.** See the struct
    /// comment on `swap_total_bytes` and ADR-0007's addendum.
    pub fn swap_percent(&self) -> f64 {
        if self.swap_total_bytes == 0 {
            return 0.0;
        }
        self.swap_used_bytes as f64 * 100.0 / self.swap_total_bytes as f64
    }

    pub fn compressed_bytes(&self) -> u64 {
        self.pages_compressor.saturating_mul(self.page_size)
    }

    /// Bytes on the free list, strictly. **Do not band on this** — macOS keeps
    /// the free list deliberately tiny and parks everything reclaimable in the
    /// inactive/file-cache pools, so a healthy machine reads 100–500 MB "free"
    /// forever. Measured 2026-09-01: 84 MB here while the kernel's own
    /// `kern.memorystatus_vm_pressure_level` said NORMAL and `memory_pressure`
    /// reported 56% free. Banding on this produced six overnight false alerts.
    /// It exists for display beside `available_bytes`.
    pub fn free_bytes(&self) -> u64 {
        self.pages_free.saturating_mul(self.page_size)
    }

    /// Bytes the kernel could hand to an allocating app without swapping:
    /// the free list, plus speculative reads, plus purgeable allocations, plus
    /// the file cache (clean file-backed pages drop without I/O). This is the
    /// number that behaves like what "free memory" MEANS in the thresholds —
    /// and what `memory_pressure`'s "free percentage" tracks, which is the
    /// source the port spec named (docs/signal-collection.md).
    pub fn available_bytes(&self) -> u64 {
        self.pages_free
            .saturating_add(self.pages_speculative)
            .saturating_add(self.pages_purgeable)
            .saturating_add(self.pages_external)
            .saturating_mul(self.page_size)
    }
}

/// The kernel's thermal pressure level, 0 (nominal) to 4 (sleeping). Syscall-only
/// (ADR-0007): `notify_register_check` + `notify_get_state` on the kernel's
/// well-known thermal key, then `notify_cancel`. Any failure is an error, not a
/// zero — a zero here would read as "nominal".
pub fn thermal_pressure_level() -> Result<u32> {
    const KEY: &[u8] = b"com.apple.system.thermalpressurelevel\0";
    let mut token: i32 = 0;
    let rc = unsafe { thermal_ffi::notify_register_check(KEY.as_ptr().cast(), &raw mut token) };
    if rc != 0 {
        return Err(CoreError::Schema(format!(
            "notify_register_check(thermalpressurelevel) failed: status {rc}"
        )));
    }
    let mut state: u64 = 0;
    let rc = unsafe { thermal_ffi::notify_get_state(token, &raw mut state) };
    unsafe { thermal_ffi::notify_cancel(token) };
    if rc != 0 {
        return Err(CoreError::Schema(format!(
            "notify_get_state(thermalpressurelevel) failed: status {rc}"
        )));
    }
    u32::try_from(state).map_err(|_| {
        CoreError::Schema(format!(
            "thermal pressure level {state} does not fit the kernel's 0–4 enum"
        ))
    })
}

/// `CPU_Speed_Limit` (percent) from `IOPMCopyCPUPowerStatus`, or `None` when the
/// platform does not publish one — which is every Apple-silicon Mac
/// (`kIOReturnNotFound`, probed 2026-09-08). Never an error: "not published" is
/// a fact about the platform, not a failed read.
pub fn cpu_speed_limit_percent() -> Option<u32> {
    use thermal_ffi::*;
    let mut dict: CFDictionaryRef = std::ptr::null();
    let rc = unsafe { IOPMCopyCPUPowerStatus(&raw mut dict) };
    if rc != 0 || dict.is_null() {
        return None;
    }
    let key = unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            c"CPU_Speed_Limit".as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    let mut out: Option<u32> = None;
    if !key.is_null() {
        let num = unsafe { CFDictionaryGetValue(dict, key) };
        if !num.is_null() {
            let mut v: i64 = 0;
            let ok = unsafe { CFNumberGetValue(num, K_CF_NUMBER_SINT64, (&raw mut v).cast()) };
            if ok {
                out = u32::try_from(v.clamp(0, 100)).ok();
            }
        }
        unsafe { CFRelease(key) };
    }
    unsafe { CFRelease(dict) };
    out
}

/// Cumulative CPU tick counters summed across every core: `(total, idle)`.
///
/// `host_processor_info(PROCESSOR_CPU_LOAD_INFO)` returns one
/// `processor_cpu_load_info` per core, each holding four monotonic tick counters
/// (user, system, idle, nice). We sum all four into `total` and the idle state
/// into `idle`; the model turns the DELTA into a utilization fraction (busy =
/// total − idle). Syscall-only (ADR-0007).
///
/// The kernel ALLOCATES the returned array in this task's VM; it must be handed
/// back with `vm_deallocate` or the sampler leaks one region every cycle.
pub fn cpu_ticks() -> Result<(u64, u64)> {
    let host = unsafe { mach2::mach_init::mach_host_self() };
    let mut ncpu: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut info_count: libc::mach_msg_type_number_t = 0;
    let kr = unsafe {
        libc::host_processor_info(
            host,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &raw mut ncpu,
            &raw mut info,
            &raw mut info_count,
        )
    };
    if kr != 0 {
        return Err(CoreError::Schema(format!(
            "host_processor_info(PROCESSOR_CPU_LOAD_INFO) failed with kern_return {kr}"
        )));
    }
    if info.is_null() || ncpu == 0 {
        return Err(CoreError::Schema(
            "host_processor_info returned no processors".into(),
        ));
    }
    // `ncpu` structs of `[u32; CPU_STATE_MAX]` laid end to end.
    let loads = unsafe {
        std::slice::from_raw_parts(info.cast::<libc::processor_cpu_load_info>(), ncpu as usize)
    };
    let mut total: u64 = 0;
    let mut idle: u64 = 0;
    for core in loads {
        for (state, ticks) in core.cpu_ticks.iter().enumerate() {
            total = total.saturating_add(u64::from(*ticks));
            if state == libc::CPU_STATE_IDLE as usize {
                idle = idle.saturating_add(u64::from(*ticks));
            }
        }
    }
    // Hand the array back or leak a VM region every sample.
    unsafe {
        libc::vm_deallocate(
            mach2::traps::mach_task_self(),
            info as usize as libc::vm_address_t,
            info_count as usize * std::mem::size_of::<libc::integer_t>(),
        );
    }
    Ok((total, idle))
}

/// The three C surfaces the thermal reading needs, declared by hand: libnotify
/// (in libSystem, no link flag), plus the four CoreFoundation and one IOKit
/// symbols `cpu_speed_limit_percent` touches. A hand-rolled 30 lines beats two
/// more crates through cargo-deny for one dictionary lookup.
mod thermal_ffi {
    use std::ffi::{c_char, c_void};
    pub type CFTypeRef = *const c_void;
    pub type CFDictionaryRef = CFTypeRef;
    pub type CFStringRef = CFTypeRef;
    pub type CFNumberRef = CFTypeRef;
    /// `kCFNumberSInt64Type`.
    pub const K_CF_NUMBER_SINT64: isize = 4;
    /// `kCFStringEncodingUTF8`.
    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    unsafe extern "C" {
        pub fn notify_register_check(name: *const c_char, out_token: *mut i32) -> u32;
        pub fn notify_get_state(token: i32, state64: *mut u64) -> u32;
        pub fn notify_cancel(token: i32) -> u32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
        pub fn CFNumberGetValue(
            number: CFNumberRef,
            the_type: isize,
            value_ptr: *mut c_void,
        ) -> bool;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFStringCreateWithCString(
            alloc: CFTypeRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOPMCopyCPUPowerStatus(cpu_power_status: *mut CFDictionaryRef) -> i32;
    }
}

/// Free space on one mounted volume. `statfs`-derived, O(1) per volume.
///
/// Banshee reads volumes and NOTHING ELSE about the filesystem — no walk, no
/// per-path sizes (ADR-0003). "What is taking the space" is a disk-usage tool's question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeReading {
    pub mount_point: String,
    pub total_bytes: u64,
    /// Blocks available to a NON-root process (`f_bavail`), which is what a
    /// user actually gets. `f_bfree` counts root-reserved blocks and would
    /// overstate headroom.
    pub avail_bytes: u64,
}

impl VolumeReading {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.avail_bytes)
    }
}

/// The seam. `RealProbe` talks to the kernel; tests use their own
/// implementation. Mocking happens HERE and nowhere else — the sampler,
/// the rate math, and the pressure model all run for real against a fake
/// kernel, which is the point.
pub trait SysProbe: Send + Sync {
    fn read(&self) -> Result<SysReading>;
    fn volumes(&self, mount_points: &[String]) -> Result<Vec<VolumeReading>>;
}

/// The real kernel.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProbe;

impl RealProbe {
    /// `sysctlbyname` into a `T`-shaped buffer.
    ///
    /// # Safety contract for callers
    /// `T` must be the exact layout the named sysctl returns. A mismatch is
    /// caught below by comparing the kernel's returned size against
    /// `size_of::<T>()` rather than trusting it, because a short read would
    /// otherwise leave part of `T` zeroed and silently produce plausible
    /// garbage (a load average of 0.00, a boot time of 1970).
    fn sysctl<T: Copy>(name: &str) -> Result<T> {
        let cname = CString::new(name)
            .map_err(|_| CoreError::InvalidInput(format!("sysctl name has a NUL: {name:?}")))?;
        let mut value: T = unsafe { std::mem::zeroed() };
        let want = std::mem::size_of::<T>();
        let mut size = want;
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                (&raw mut value).cast::<libc::c_void>(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(CoreError::Schema(format!(
                "sysctlbyname({name}) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        if size != want {
            return Err(CoreError::Schema(format!(
                "sysctlbyname({name}) returned {size} bytes, expected {want}; \
                 the kernel struct layout does not match this build"
            )));
        }
        Ok(value)
    }

    fn vm_stats() -> Result<libc::vm_statistics64> {
        // mach_host_self comes from mach2 because libc's is deprecated and this
        // workspace builds clippy with -D warnings.
        let host = unsafe { mach2::mach_init::mach_host_self() };
        let mut info: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = (std::mem::size_of::<libc::vm_statistics64>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let kr = unsafe {
            libc::host_statistics64(
                host,
                HOST_VM_INFO64,
                (&raw mut info).cast::<libc::integer_t>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return Err(CoreError::Schema(format!(
                "host_statistics64(HOST_VM_INFO64) failed with kern_return {kr}"
            )));
        }
        Ok(info)
    }
}

impl SysProbe for RealProbe {
    fn read(&self) -> Result<SysReading> {
        let la: LoadAvg = Self::sysctl("vm.loadavg")?;
        let scale = if la.fscale == 0 {
            // Guard rather than divide by zero; fscale is 2048 in practice.
            return Err(CoreError::Schema(
                "vm.loadavg reported fscale=0, which would make every load figure infinite".into(),
            ));
        } else {
            la.fscale as f64
        };

        let swap: libc::xsw_usage = Self::sysctl("vm.swapusage")?;
        let boot: libc::timeval = Self::sysctl("kern.boottime")?;
        let ncpu: i32 = Self::sysctl("hw.ncpu")?;
        let mem_total: u64 = Self::sysctl("hw.memsize")?;
        let page_size: u32 = Self::sysctl("hw.pagesize")?;
        let pressure: u32 = Self::sysctl("kern.memorystatus_vm_pressure_level")?;
        let kernel_free: u32 = Self::sysctl("kern.memorystatus_level")?;
        let jetsam: u64 = Self::sysctl("kern.memorystatus.kill_on_sustained_pressure_count")?;
        let vm = Self::vm_stats()?;
        let thermal = thermal_pressure_level()?;
        let (cpu_ticks_total, cpu_ticks_idle) = cpu_ticks()?;

        Ok(SysReading {
            ncpu: ncpu.max(0) as u32,
            mem_total_bytes: mem_total,
            page_size: u64::from(page_size),
            boot_time_secs: boot.tv_sec,
            load_1m: f64::from(la.ldavg[0]) / scale,
            load_5m: f64::from(la.ldavg[1]) / scale,
            load_15m: f64::from(la.ldavg[2]) / scale,
            swap_total_bytes: swap.xsu_total,
            swap_used_bytes: swap.xsu_used,
            pages_free: u64::from(vm.free_count),
            pages_active: u64::from(vm.active_count),
            pages_inactive: u64::from(vm.inactive_count),
            pages_wired: u64::from(vm.wire_count),
            pages_speculative: u64::from(vm.speculative_count),
            pages_compressor: u64::from(vm.compressor_page_count),
            pages_purgeable: u64::from(vm.purgeable_count),
            pages_external: u64::from(vm.external_page_count),
            swapins: vm.swapins,
            swapouts: vm.swapouts,
            memory_pressure_level: pressure,
            thermal_pressure_level: Some(thermal),
            cpu_speed_limit_percent: cpu_speed_limit_percent(),
            cpu_ticks_total: Some(cpu_ticks_total),
            cpu_ticks_idle: Some(cpu_ticks_idle),
            kernel_free_percent: Some(kernel_free),
            jetsam_kills: Some(jetsam),
        })
    }

    fn volumes(&self, mount_points: &[String]) -> Result<Vec<VolumeReading>> {
        let mut out = Vec::with_capacity(mount_points.len());
        for mp in mount_points {
            let cpath = CString::new(mp.as_str())
                .map_err(|_| CoreError::InvalidInput(format!("mount point has a NUL: {mp:?}")))?;
            let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statfs(cpath.as_ptr(), &raw mut sfs) };
            if rc != 0 {
                // A volume that has gone away is not a fatal error for a
                // sampler — it is a fact about the machine. Skip it; the
                // caller sees a shorter list, and the dimension for that
                // volume simply has no reading this cycle.
                continue;
            }
            let bsize = sfs.f_bsize as u64;
            out.push(VolumeReading {
                mount_point: mp.clone(),
                total_bytes: sfs.f_blocks.saturating_mul(bsize),
                avail_bytes: sfs.f_bavail.saturating_mul(bsize),
            });
        }
        Ok(out)
    }
}

/// What THIS process costs the machine it is watching — its physical memory
/// footprint and the CPU time it has consumed. Served on `/stats` and quoted in
/// the README: a monitor that will not say what
/// it costs has no standing to complain about anything else (ADR-0006), and a
/// monitor that inflated one of its own dimensions would be a bad joke
/// (ADR-0007). Syscall-only, like everything in the cheap tier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfUsage {
    /// `ri_phys_footprint` from `proc_pid_rusage` — the number Activity Monitor
    /// labels "Memory". Deliberately NOT resident size: on a machine already in
    /// swap, RSS falls while the process's real cost does not (`banshee-75z`),
    /// and footprint is the figure the kernel's own jetsam accounting uses.
    pub footprint_bytes: u64,
    /// User + system CPU seconds consumed since the process started
    /// (`getrusage(RUSAGE_SELF)`). Cumulative, so any client can divide by the
    /// daemon's uptime for an honest lifetime average — the burst-vs-sustained
    /// question `%CPU` cannot answer.
    pub cpu_secs: f64,
}

/// Read [`SelfUsage`] for the calling process.
pub fn self_usage() -> Result<SelfUsage> {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut ru) };
    if rc != 0 {
        return Err(CoreError::Schema(format!(
            "getrusage(RUSAGE_SELF) failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + f64::from(t.tv_usec) / 1e6;
    let cpu_secs = tv(ru.ru_utime) + tv(ru.ru_stime);

    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return Err(CoreError::Schema(format!(
            "proc_pid_rusage(RUSAGE_INFO_V4) failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(SelfUsage {
        footprint_bytes: info.ri_phys_footprint,
        cpu_secs,
    })
}

/// The macOS data volume. **Not `/`** — on APFS `/` is the sealed system
/// snapshot, and reading it is how a disk-usage tool once reported "140Mi avail"
/// while the data volume actually had 21Gi (2026-08-20).
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

#[cfg(test)]
mod tests {
    use super::*;

    fn reading() -> SysReading {
        SysReading {
            ncpu: 8,
            mem_total_bytes: 25_769_803_776,
            page_size: 16384,
            boot_time_secs: 1_788_122_573,
            load_1m: 4.20,
            load_5m: 10.45,
            load_15m: 11.00,
            swap_total_bytes: 8_589_934_592,
            swap_used_bytes: 7_078_019_072,
            pages_free: 13_267,
            pages_active: 324_446,
            pages_inactive: 319_599,
            pages_wired: 191_846,
            pages_speculative: 4_289,
            pages_compressor: 685_141,
            pages_purgeable: 5_581,
            pages_external: 550_000,
            swapins: 1_284_918,
            swapouts: 1_827_027,
            memory_pressure_level: 1,
            thermal_pressure_level: Some(0),
            cpu_speed_limit_percent: None,
            cpu_ticks_total: Some(80_000_000),
            cpu_ticks_idle: Some(60_000_000),
            kernel_free_percent: Some(50),
            jetsam_kills: Some(0),
        }
    }

    /// Load must be divided by core count. Mutation-proof: return `self.load_1m`
    /// unchanged from `per_core` and this fails — 4.20 is "busy" per-core on one
    /// core but 0.525 on eight, which is the difference between a yellow band
    /// and a green one.
    #[test]
    fn load_is_reported_per_core() {
        let r = reading();
        assert!((r.load_per_core_1m() - 0.525).abs() < 1e-9);
        assert!((r.load_per_core_5m() - 1.30625).abs() < 1e-9);
    }

    /// A zero core count must not produce inf/NaN. NaN compares false against
    /// every threshold, so a single corrupt reading would silently report
    /// "healthy" forever. Mutation-proof: delete the `ncpu == 0` guard and this
    /// fails on `is_finite`.
    #[test]
    fn zero_core_count_does_not_poison_the_band_comparison() {
        let mut r = reading();
        r.ncpu = 0;
        let v = r.load_per_core_1m();
        assert!(v.is_finite(), "got {v}, which would defeat every threshold");
        assert_eq!(v, 0.0);
    }

    /// Same hazard on the swap denominator, which really can be zero: a machine
    /// with swap disabled reports total = 0.
    #[test]
    fn zero_swap_total_does_not_divide_by_zero() {
        let mut r = reading();
        r.swap_total_bytes = 0;
        r.swap_used_bytes = 0;
        assert_eq!(r.swap_percent(), 0.0);
        assert!(r.swap_percent().is_finite());
    }

    #[test]
    fn swap_percent_matches_the_observed_reading() {
        // The live 2026-08-31 reading: 7.08 GB of 8.59 GB.
        assert!((reading().swap_percent() - 82.4).abs() < 0.1);
    }

    /// Page counts are useless without the page size multiplier. Mutation-proof:
    /// drop the multiply and 685,141 pages reads as 685 KB instead of 11.2 GB.
    #[test]
    fn page_counts_convert_to_bytes_via_page_size() {
        let r = reading();
        assert_eq!(r.compressed_bytes(), 685_141 * 16384);
        assert!((r.compressed_bytes() as f64 / 1e9 - 11.2).abs() < 0.1);
        assert_eq!(r.free_bytes(), 13_267 * 16384);
    }

    /// `compressed_bytes` multiplies two u64s. A pathological page count must
    /// saturate rather than wrap to a small number, which would read as
    /// "plenty of headroom" at exactly the wrong moment.
    #[test]
    fn byte_conversion_saturates_instead_of_wrapping() {
        let mut r = reading();
        r.pages_compressor = u64::MAX;
        assert_eq!(r.compressed_bytes(), u64::MAX);
    }

    #[test]
    fn volume_used_is_total_minus_available_and_never_underflows() {
        let v = VolumeReading {
            mount_point: DATA_VOLUME.to_string(),
            total_bytes: 494_384_795_648,
            avail_bytes: 71_234_567_890,
        };
        assert_eq!(v.used_bytes(), 494_384_795_648 - 71_234_567_890);

        // avail > total is nonsense, but statfs on a weird mount can produce it;
        // saturating_sub keeps it at 0 rather than wrapping to ~18 exabytes.
        let odd = VolumeReading {
            mount_point: "/weird".into(),
            total_bytes: 1,
            avail_bytes: 100,
        };
        assert_eq!(odd.used_bytes(), 0);
    }

    // ---- Tests that touch the real kernel. -------------------------------
    // These assert INVARIANTS, not values, so they hold on any Mac and in any
    // load condition. They are not skipped: a probe that cannot read the
    // kernel is a real failure, and an XCTSkip-style
    // "green because it did not run" is worse than a red suite.

    /// `self_usage` must report THIS process, and both figures must move with
    /// what the process does — otherwise a constant would pass. Footprint is
    /// checked by touching 64 MB and asserting growth; CPU by spinning and
    /// asserting the counter rose. A reading that only asserted `> 0` could
    /// not tell the real syscalls from a hardcoded plausible number.
    #[test]
    fn self_usage_tracks_this_process() {
        let before = self_usage().expect("getrusage/proc_pid_rusage must succeed");
        assert!(
            before.footprint_bytes > 1 << 20,
            "a running test binary is over 1 MB"
        );

        // Touch every page, or the allocation stays virtual and the footprint
        // does not move.
        let mut ballast = vec![0u8; 64 << 20];
        for (i, b) in ballast.iter_mut().enumerate().step_by(4096) {
            *b = (i % 251) as u8;
        }
        std::hint::black_box(&ballast);
        let mut spin = 0u64;
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(30) {
            spin = spin.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(spin);

        let after = self_usage().unwrap();
        assert!(
            after.footprint_bytes >= before.footprint_bytes + (32 << 20),
            "footprint did not grow with a touched 64 MB allocation: {} -> {}",
            before.footprint_bytes,
            after.footprint_bytes
        );
        assert!(
            after.cpu_secs > before.cpu_secs,
            "cpu_secs did not rise across a 30 ms spin: {} -> {}",
            before.cpu_secs,
            after.cpu_secs
        );
        drop(ballast);
    }

    #[test]
    fn real_probe_reads_plausible_values() {
        let r = RealProbe.read().expect("kernel read must succeed on macOS");

        assert!(r.ncpu >= 1, "ncpu = {}", r.ncpu);
        assert!(
            r.mem_total_bytes > 1 << 30,
            "memsize = {}",
            r.mem_total_bytes
        );
        assert!(
            r.page_size.is_power_of_two() && r.page_size >= 4096,
            "page size = {}",
            r.page_size
        );
        // If the fscale division were missing, load would be in the thousands.
        assert!(
            r.load_1m >= 0.0 && r.load_1m < 1000.0,
            "load_1m = {} — is fscale being applied?",
            r.load_1m
        );
        assert!(
            r.boot_time_secs > 1_600_000_000,
            "boot time = {} — sysctl struct layout is probably wrong",
            r.boot_time_secs
        );
        assert!(r.swap_used_bytes <= r.swap_total_bytes || r.swap_total_bytes == 0);
        // A live machine always has SOME resident pages.
        assert!(r.pages_active + r.pages_wired > 0);
        // The kernel's pressure level is an enum, not a scale: 1 normal, 2 warn,
        // 4 critical. Any other value means the sysctl's contract changed and
        // the banding on it is no longer meaningful.
        assert!(
            matches!(r.memory_pressure_level, 1 | 2 | 4),
            "kern.memorystatus_vm_pressure_level = {} — not one of 1/2/4",
            r.memory_pressure_level
        );
        // A live read ALWAYS records the thermal level (None is reserved for
        // pre-v11 rows), and the kernel's enum has five values on macOS.
        assert!(
            r.thermal_pressure_level.is_some_and(|l| l <= 4),
            "thermal_pressure_level = {:?} — must be Some(0..=4) from a live kernel",
            r.thermal_pressure_level
        );
        // Absent on Apple silicon, 1..=100 where an Intel PMU publishes it.
        assert!(
            r.cpu_speed_limit_percent
                .is_none_or(|p| (1..=100).contains(&p)),
            "cpu_speed_limit_percent = {:?}",
            r.cpu_speed_limit_percent
        );
        // A live read ALWAYS records the CPU ticks (None is reserved for pre-v12
        // rows), the two move together, and idle can never exceed total.
        let total = r.cpu_ticks_total.expect("live read records total ticks");
        let idle = r.cpu_ticks_idle.expect("live read records idle ticks");
        assert!(total > 0, "cpu_ticks_total = {total}");
        assert!(idle <= total, "idle {idle} exceeds total {total}");
    }

    /// The tick read is a real syscall, not a constant: the counters are
    /// monotonic, so a second read a spin later must not go backwards, and the
    /// busy delta must be non-negative. A reading that only asserted `> 0` could
    /// not tell the real `host_processor_info` from a hardcoded number.
    #[test]
    fn cpu_ticks_are_monotonic_across_two_reads() {
        let (t0, i0) = cpu_ticks().expect("host_processor_info must succeed on macOS");
        let start = std::time::Instant::now();
        let mut spin = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(30) {
            spin = spin.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(spin);
        let (t1, i1) = cpu_ticks().unwrap();
        assert!(t1 >= t0, "total ticks went backwards: {t0} -> {t1}");
        assert!(i1 >= i0, "idle ticks went backwards: {i0} -> {i1}");
        assert!(i0 <= t0 && i1 <= t1, "idle exceeded total");
    }

    /// The thermal read is a real syscall pair, not a constant: it must succeed,
    /// stay inside the kernel's enum, and agree with itself across two reads a
    /// moment apart (a level does not flap within a millisecond).
    #[test]
    fn thermal_pressure_level_reads_the_kernels_enum() {
        let a = thermal_pressure_level().expect("notify_get_state must succeed on macOS");
        let b = thermal_pressure_level().unwrap();
        assert!(a <= 4, "level {a} is outside 0..=4");
        assert_eq!(a, b, "two immediate reads disagree");
    }

    /// The size check in `sysctl` must reject a layout mismatch rather than
    /// return a half-zeroed struct. Mutation-proof: delete the
    /// `size != want` branch and this passes, because the kernel happily
    /// short-writes into an oversized buffer.
    #[test]
    fn sysctl_rejects_a_struct_of_the_wrong_size() {
        // hw.ncpu returns 4 bytes; ask for 16 and it must be refused.
        #[repr(C)]
        #[derive(Clone, Copy, Debug)]
        struct TooBig([u8; 16]);
        let err = RealProbe::sysctl::<TooBig>("hw.ncpu").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected"),
            "expected a size-mismatch error, got: {msg}"
        );
    }

    #[test]
    fn sysctl_reports_an_unknown_name_as_an_error() {
        let err = RealProbe::sysctl::<u32>("banshee.no.such.knob").unwrap_err();
        assert!(err.to_string().contains("banshee.no.such.knob"));
    }

    #[test]
    fn real_probe_reads_the_data_volume() {
        let vols = RealProbe
            .volumes(&[DATA_VOLUME.to_string()])
            .expect("statfs on the data volume must succeed");
        assert_eq!(vols.len(), 1);
        let v = &vols[0];
        assert!(v.total_bytes > 1 << 30, "total = {}", v.total_bytes);
        assert!(v.avail_bytes <= v.total_bytes);
    }

    /// A vanished mount point is a fact, not a fatal error — it must be skipped
    /// so one bad volume cannot stop the whole sample.
    #[test]
    fn a_missing_volume_is_skipped_not_fatal() {
        let vols = RealProbe
            .volumes(&[
                "/definitely/not/a/mount/point".to_string(),
                DATA_VOLUME.to_string(),
            ])
            .expect("a missing volume must not fail the read");
        assert_eq!(vols.len(), 1, "only the real volume should survive");
        assert_eq!(vols[0].mount_point, DATA_VOLUME);
    }
}
