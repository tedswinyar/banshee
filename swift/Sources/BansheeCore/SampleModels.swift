// The wire-format sample and rollup types — the cheap tier as stored, and the
// 5-minute bucket it collapses into. Field names map 1:1 to the camelCase JSON
// keys (docs/wire-format.md); mirrors `rust/banshee-core/src/sample.rs`.
//
// Decoded by the history chart (P6b). Nothing here computes an aggregate: the
// avg/max/min per field were decided once, in core's `rollup()`, and travel over
// the wire. `/samples` and `/rollups` are two routes because a rollup has
// `load1mAvg`/`load1mMax` where a sample has `load1m` — one route returning
// either shape would be a union type every client had to sniff.

import Foundation

/// One cheap-tier reading, as stored.
public struct Sample: Codable, Identifiable, Equatable, Sendable {
    public let id: UUID
    public let sampledAt: Date
    /// The reboot discriminator: two samples with different values have NO valid
    /// counter delta between them. A consumer computing a rate must check it.
    public let bootTimeSecs: Int64
    public let ncpu: Int
    public let memTotalBytes: UInt64
    /// Page size in bytes. The `pages*` fields below are in PAGES; multiply by
    /// this to get bytes. Rollups do not carry it — it lives here only.
    public let pageSize: UInt64
    public let load1m: Double
    public let load5m: Double
    public let load15m: Double
    public let swapTotalBytes: UInt64
    public let swapUsedBytes: UInt64
    public let pagesFree: UInt64
    public let pagesActive: UInt64
    public let pagesInactive: UInt64
    public let pagesWired: UInt64
    public let pagesSpeculative: UInt64
    public let pagesCompressor: UInt64
    /// App-marked discardable pages. Counts toward availability.
    public let pagesPurgeable: UInt64
    /// File-backed pages (the page cache). Counts toward availability — the
    /// free list alone is NOT a pressure signal (`banshee-cot`).
    public let pagesExternal: UInt64
    public let swapins: UInt64
    public let swapouts: UInt64
    /// The kernel's own memory verdict (`kern.memorystatus_vm_pressure_level`):
    /// 1 normal, 2 warn, 4 critical. 0 means a pre-v8 row never recorded it.
    public let memoryPressureLevel: UInt32
    /// The kernel's thermal pressure level (0 nominal … 4 sleeping), schema v11.
    /// Nullable: nil is "not recorded" (a pre-v11 row or daemon) — never 0, which
    /// is a real reading. Present-as-null on the wire.
    public let thermalPressureLevel: UInt32?
    /// IOKit's CPU speed limit percent. Intel Macs only; nil on Apple silicon,
    /// where the platform does not publish one.
    public let cpuSpeedLimitPercent: UInt32?
    /// Cumulative CPU tick counters summed across all cores (schema v12): `total`
    /// is user+system+idle+nice, `idle` the idle state alone. The CPU dimension
    /// bands on the utilization RATE core derives from these between consecutive
    /// samples, never on load-per-core. Nil = "not recorded" (a pre-v12 row);
    /// present-as-null on the wire. The two move together: both set or both nil.
    public let cpuTicksTotal: UInt64?
    public let cpuTicksIdle: UInt64?
    /// The kernel's reclaimable-memory percentage (`kern.memorystatus_level`),
    /// schema v13 (`banshee-yk8`). Advisory: shown, never bands the level. Nil =
    /// "not recorded" (a pre-v13 row); present-as-null on the wire.
    public let kernelFreePercent: UInt32?
    /// Lifetime count of kernel jetsam kills under sustained memory pressure
    /// (`kern.memorystatus.kill_on_sustained_pressure_count`), schema v13. A
    /// window-delta of this is the Jetsam dimension. Nil = pre-v13; present-as-null.
    public let jetsamKills: UInt64?
    public let volumes: [VolumeSample]

    public init(
        id: UUID, sampledAt: Date, bootTimeSecs: Int64, ncpu: Int,
        memTotalBytes: UInt64, pageSize: UInt64,
        load1m: Double, load5m: Double, load15m: Double,
        swapTotalBytes: UInt64, swapUsedBytes: UInt64,
        pagesFree: UInt64, pagesActive: UInt64, pagesInactive: UInt64,
        pagesWired: UInt64, pagesSpeculative: UInt64, pagesCompressor: UInt64,
        pagesPurgeable: UInt64, pagesExternal: UInt64,
        swapins: UInt64, swapouts: UInt64, memoryPressureLevel: UInt32,
        thermalPressureLevel: UInt32? = nil, cpuSpeedLimitPercent: UInt32? = nil,
        cpuTicksTotal: UInt64? = nil, cpuTicksIdle: UInt64? = nil,
        kernelFreePercent: UInt32? = nil, jetsamKills: UInt64? = nil,
        volumes: [VolumeSample]
    ) {
        self.id = id
        self.sampledAt = sampledAt
        self.bootTimeSecs = bootTimeSecs
        self.ncpu = ncpu
        self.memTotalBytes = memTotalBytes
        self.pageSize = pageSize
        self.load1m = load1m
        self.load5m = load5m
        self.load15m = load15m
        self.swapTotalBytes = swapTotalBytes
        self.swapUsedBytes = swapUsedBytes
        self.pagesFree = pagesFree
        self.pagesActive = pagesActive
        self.pagesInactive = pagesInactive
        self.pagesWired = pagesWired
        self.pagesSpeculative = pagesSpeculative
        self.pagesCompressor = pagesCompressor
        self.pagesPurgeable = pagesPurgeable
        self.pagesExternal = pagesExternal
        self.swapins = swapins
        self.swapouts = swapouts
        self.memoryPressureLevel = memoryPressureLevel
        self.thermalPressureLevel = thermalPressureLevel
        self.cpuSpeedLimitPercent = cpuSpeedLimitPercent
        self.cpuTicksTotal = cpuTicksTotal
        self.cpuTicksIdle = cpuTicksIdle
        self.kernelFreePercent = kernelFreePercent
        self.jetsamKills = jetsamKills
        self.volumes = volumes
    }

    enum CodingKeys: String, CodingKey {
        case id, sampledAt, bootTimeSecs, ncpu, memTotalBytes, pageSize
        case load1m, load5m, load15m, swapTotalBytes, swapUsedBytes
        case pagesFree, pagesActive, pagesInactive, pagesWired
        case pagesSpeculative, pagesCompressor, pagesPurgeable, pagesExternal
        case swapins, swapouts, memoryPressureLevel, volumes
        case thermalPressureLevel, cpuSpeedLimitPercent
        case cpuTicksTotal, cpuTicksIdle
        case kernelFreePercent, jetsamKills
    }

    // Hand-written so an older daemon (no thermal keys pre-v11, no cpu-tick keys
    // pre-v12) still decodes: the nullable fields are decodeIfPresent, everything
    // else stays required.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(UUID.self, forKey: .id)
        sampledAt = try c.decode(Date.self, forKey: .sampledAt)
        bootTimeSecs = try c.decode(Int64.self, forKey: .bootTimeSecs)
        ncpu = try c.decode(Int.self, forKey: .ncpu)
        memTotalBytes = try c.decode(UInt64.self, forKey: .memTotalBytes)
        pageSize = try c.decode(UInt64.self, forKey: .pageSize)
        load1m = try c.decode(Double.self, forKey: .load1m)
        load5m = try c.decode(Double.self, forKey: .load5m)
        load15m = try c.decode(Double.self, forKey: .load15m)
        swapTotalBytes = try c.decode(UInt64.self, forKey: .swapTotalBytes)
        swapUsedBytes = try c.decode(UInt64.self, forKey: .swapUsedBytes)
        pagesFree = try c.decode(UInt64.self, forKey: .pagesFree)
        pagesActive = try c.decode(UInt64.self, forKey: .pagesActive)
        pagesInactive = try c.decode(UInt64.self, forKey: .pagesInactive)
        pagesWired = try c.decode(UInt64.self, forKey: .pagesWired)
        pagesSpeculative = try c.decode(UInt64.self, forKey: .pagesSpeculative)
        pagesCompressor = try c.decode(UInt64.self, forKey: .pagesCompressor)
        pagesPurgeable = try c.decode(UInt64.self, forKey: .pagesPurgeable)
        pagesExternal = try c.decode(UInt64.self, forKey: .pagesExternal)
        swapins = try c.decode(UInt64.self, forKey: .swapins)
        swapouts = try c.decode(UInt64.self, forKey: .swapouts)
        memoryPressureLevel = try c.decode(UInt32.self, forKey: .memoryPressureLevel)
        thermalPressureLevel = try c.decodeIfPresent(UInt32.self, forKey: .thermalPressureLevel)
        cpuSpeedLimitPercent = try c.decodeIfPresent(UInt32.self, forKey: .cpuSpeedLimitPercent)
        cpuTicksTotal = try c.decodeIfPresent(UInt64.self, forKey: .cpuTicksTotal)
        cpuTicksIdle = try c.decodeIfPresent(UInt64.self, forKey: .cpuTicksIdle)
        kernelFreePercent = try c.decodeIfPresent(UInt32.self, forKey: .kernelFreePercent)
        jetsamKills = try c.decodeIfPresent(UInt64.self, forKey: .jetsamKills)
        volumes = try c.decode([VolumeSample].self, forKey: .volumes)
    }

    // Hand-written for the UUID: Foundation encodes `uuidString` UPPERCASE, and
    // the wire contract is lowercase-out (decoders accept any case). Same
    // foot-gun as the other hand-written encoders: this is a HARD-CODED field
    // list, pinned by `testEncodeCoversEverySampleField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id.uuidString.lowercased(), forKey: .id)
        try c.encode(sampledAt, forKey: .sampledAt)
        try c.encode(bootTimeSecs, forKey: .bootTimeSecs)
        try c.encode(ncpu, forKey: .ncpu)
        try c.encode(memTotalBytes, forKey: .memTotalBytes)
        try c.encode(pageSize, forKey: .pageSize)
        try c.encode(load1m, forKey: .load1m)
        try c.encode(load5m, forKey: .load5m)
        try c.encode(load15m, forKey: .load15m)
        try c.encode(swapTotalBytes, forKey: .swapTotalBytes)
        try c.encode(swapUsedBytes, forKey: .swapUsedBytes)
        try c.encode(pagesFree, forKey: .pagesFree)
        try c.encode(pagesActive, forKey: .pagesActive)
        try c.encode(pagesInactive, forKey: .pagesInactive)
        try c.encode(pagesWired, forKey: .pagesWired)
        try c.encode(pagesSpeculative, forKey: .pagesSpeculative)
        try c.encode(pagesCompressor, forKey: .pagesCompressor)
        try c.encode(pagesPurgeable, forKey: .pagesPurgeable)
        try c.encode(pagesExternal, forKey: .pagesExternal)
        try c.encode(swapins, forKey: .swapins)
        try c.encode(swapouts, forKey: .swapouts)
        try c.encode(memoryPressureLevel, forKey: .memoryPressureLevel)
        // Present-as-null (wire contract): encodeIfPresent would DROP a nil key.
        if let thermalPressureLevel {
            try c.encode(thermalPressureLevel, forKey: .thermalPressureLevel)
        } else {
            try c.encodeNil(forKey: .thermalPressureLevel)
        }
        if let cpuSpeedLimitPercent {
            try c.encode(cpuSpeedLimitPercent, forKey: .cpuSpeedLimitPercent)
        } else {
            try c.encodeNil(forKey: .cpuSpeedLimitPercent)
        }
        if let cpuTicksTotal {
            try c.encode(cpuTicksTotal, forKey: .cpuTicksTotal)
        } else {
            try c.encodeNil(forKey: .cpuTicksTotal)
        }
        if let cpuTicksIdle {
            try c.encode(cpuTicksIdle, forKey: .cpuTicksIdle)
        } else {
            try c.encodeNil(forKey: .cpuTicksIdle)
        }
        if let kernelFreePercent {
            try c.encode(kernelFreePercent, forKey: .kernelFreePercent)
        } else {
            try c.encodeNil(forKey: .kernelFreePercent)
        }
        if let jetsamKills {
            try c.encode(jetsamKills, forKey: .jetsamKills)
        } else {
            try c.encodeNil(forKey: .jetsamKills)
        }
        try c.encode(volumes, forKey: .volumes)
    }
}

public struct VolumeSample: Codable, Equatable, Sendable {
    public let mountPoint: String
    public let totalBytes: UInt64
    public let availBytes: UInt64

    public init(mountPoint: String, totalBytes: UInt64, availBytes: UInt64) {
        self.mountPoint = mountPoint
        self.totalBytes = totalBytes
        self.availBytes = availBytes
    }
}

/// A 5-minute aggregate — what survives past 24 hours.
///
/// The choice of aggregate per field is core's, and deliberate: **averages hide
/// the spike that matters**, so load and swap carry a max, free memory a min. A
/// chart that plots only the averages hides exactly what the max exists to show.
public struct Rollup: Codable, Identifiable, Equatable, Sendable {
    /// One rollup per bucket, so the bucket start IS the identity — which is what
    /// lets SwiftUI/Charts treat a refreshed series as the same points.
    public var id: Date { bucketStart }

    public let bucketStart: Date
    public let sampleCount: Int
    public let load1mAvg: Double
    public let load1mMax: Double
    public let swapUsedAvg: UInt64
    public let swapUsedMax: UInt64
    /// In PAGES. The page size lives on the raw sample, not here.
    public let pagesFreeMin: UInt64
    public let pagesCompressorMax: UInt64
    /// Increase across the bucket. **Nil when a reboot inside it voided the
    /// counters, or a single sample had nothing to diff — nil is NOT zero.** A
    /// client rendering the void as 0 reports a calm five minutes that was never
    /// observed.
    public let swapinsDelta: UInt64?
    public let swapoutsDelta: UInt64?
    /// The census scalars (banshee-yj1) — the month-scale sprawl trend. **Nil
    /// means no census fell in this bucket (or the rollup pre-dates schema v9),
    /// which is a different fact from zero of anything.**
    public let staleSessionsMax: UInt32?
    public let orphansMax: UInt32?
    /// The DEDUPLICATED monitor percent (never a sum of per-agent rows).
    public let monitorPercentMax: Double?
    public let totalProcsMax: UInt32?
    public let volumes: [VolumeRollup]

    public init(
        bucketStart: Date, sampleCount: Int,
        load1mAvg: Double, load1mMax: Double,
        swapUsedAvg: UInt64, swapUsedMax: UInt64,
        pagesFreeMin: UInt64, pagesCompressorMax: UInt64,
        swapinsDelta: UInt64?, swapoutsDelta: UInt64?,
        staleSessionsMax: UInt32?, orphansMax: UInt32?,
        monitorPercentMax: Double?, totalProcsMax: UInt32?,
        volumes: [VolumeRollup]
    ) {
        self.bucketStart = bucketStart
        self.sampleCount = sampleCount
        self.load1mAvg = load1mAvg
        self.load1mMax = load1mMax
        self.swapUsedAvg = swapUsedAvg
        self.swapUsedMax = swapUsedMax
        self.pagesFreeMin = pagesFreeMin
        self.pagesCompressorMax = pagesCompressorMax
        self.swapinsDelta = swapinsDelta
        self.swapoutsDelta = swapoutsDelta
        self.staleSessionsMax = staleSessionsMax
        self.orphansMax = orphansMax
        self.monitorPercentMax = monitorPercentMax
        self.totalProcsMax = totalProcsMax
        self.volumes = volumes
    }

    enum CodingKeys: String, CodingKey {
        case bucketStart, sampleCount, load1mAvg, load1mMax
        case swapUsedAvg, swapUsedMax, pagesFreeMin, pagesCompressorMax
        case swapinsDelta, swapoutsDelta
        case staleSessionsMax, orphansMax, monitorPercentMax, totalProcsMax
        case volumes
    }

    // Hand-written on purpose: the synthesized encoder OMITS nil keys, violating
    // present-as-null. Hard-coded field list, pinned by
    // `testEncodeCoversEveryRollupField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(bucketStart, forKey: .bucketStart)
        try c.encode(sampleCount, forKey: .sampleCount)
        try c.encode(load1mAvg, forKey: .load1mAvg)
        try c.encode(load1mMax, forKey: .load1mMax)
        try c.encode(swapUsedAvg, forKey: .swapUsedAvg)
        try c.encode(swapUsedMax, forKey: .swapUsedMax)
        try c.encode(pagesFreeMin, forKey: .pagesFreeMin)
        try c.encode(pagesCompressorMax, forKey: .pagesCompressorMax)
        if let swapinsDelta {
            try c.encode(swapinsDelta, forKey: .swapinsDelta)
        } else {
            try c.encodeNil(forKey: .swapinsDelta)
        }
        if let swapoutsDelta {
            try c.encode(swapoutsDelta, forKey: .swapoutsDelta)
        } else {
            try c.encodeNil(forKey: .swapoutsDelta)
        }
        if let staleSessionsMax {
            try c.encode(staleSessionsMax, forKey: .staleSessionsMax)
        } else {
            try c.encodeNil(forKey: .staleSessionsMax)
        }
        if let orphansMax {
            try c.encode(orphansMax, forKey: .orphansMax)
        } else {
            try c.encodeNil(forKey: .orphansMax)
        }
        if let monitorPercentMax {
            try c.encode(monitorPercentMax, forKey: .monitorPercentMax)
        } else {
            try c.encodeNil(forKey: .monitorPercentMax)
        }
        if let totalProcsMax {
            try c.encode(totalProcsMax, forKey: .totalProcsMax)
        } else {
            try c.encodeNil(forKey: .totalProcsMax)
        }
        try c.encode(volumes, forKey: .volumes)
    }
}

public struct VolumeRollup: Codable, Equatable, Sendable {
    public let mountPoint: String
    public let totalBytes: UInt64
    public let availMin: UInt64
    public let availAvg: UInt64

    public init(mountPoint: String, totalBytes: UInt64, availMin: UInt64, availAvg: UInt64) {
        self.mountPoint = mountPoint
        self.totalBytes = totalBytes
        self.availMin = availMin
        self.availAvg = availAvg
    }
}
