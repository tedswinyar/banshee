// Every screen, drawn from the shared fixtures, in light and dark — with no app
// launched (banshee-v54.5).
//
// The views were in the executable target, so nothing could render them: the only
// way to see a colour or wording change was to build the app and launch it on a
// desktop. Three commits of visual change went out that way, verified by tests of
// values and by reading source, and looked at by nobody.
//
// This draws the REAL views offscreen — an `NSHostingView` with no window, read
// back with `cacheDisplay`, the technique a sibling app's UI rule guard proved on
// a headless build server. It needs no app process, no daemon, no socket and no
// screen, so it runs inside `swift test` on the runner, and any number of runs can
// go at once: each is one process that writes only where it is told.
//
// Two jobs:
//   1. A gate, always: every screen renders SOMETHING in both appearances, and the
//      two appearances differ. A view that crashes, draws nothing, or ignores the
//      appearance fails here.
//   2. A capture, on request: with `BANSHEE_CAPTURE_DIR=<dir>` set, each render is
//      also written there as `<screen>-<light|dark>.png` for a person to look at.
//
// What it is not: the app. There is no window chrome, no popover material, no
// menu bar. It shows what the views draw.

import AppKit
import SwiftUI
import XCTest
@testable import BansheeCore
@testable import BansheeUI
import DesignKit

/// The shared wire fixtures (`tests/fixtures/`), served through the one network
/// boundary. Not a behaviour mock: every answer is the fixture's raw bytes through
/// the real decoder, the same bytes the Rust suite and the e2e harness read.
struct FixtureClient: APIClientProtocol {
    /// Which `pressure-<name>.json` the verdict comes from.
    let pressureFixture: String
    /// A NEW INSTALL: no rollups, no episodes yet. The daemon answers both reads
    /// with an empty list, which is what the History and Alerts empty states are for.
    var newInstall = false

    static func data(_ name: String) throws -> Data {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // → BansheeUITests/
            .deletingLastPathComponent() // → Tests/
            .deletingLastPathComponent() // → swift/
            .deletingLastPathComponent() // → repo root
            .appending(path: "tests/fixtures/\(name)")
        return try Data(contentsOf: url)
    }

    static func decode<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        try Wire.decoder().decode(type, from: data(name))
    }

    func pressure() async throws -> Pressure { try Self.decode(Pressure.self, "pressure-\(pressureFixture).json") }
    func headroom() async throws -> Headroom { try Self.decode(Headroom.self, "headroom-shrieking.json") }
    func health() async throws -> Bool { true }
    func healthInfo() async throws -> HealthInfo { HealthInfo(status: "ok", version: "0.0.0", gitRev: "fixture") }
    func samples(limit: Int?, from: Date?, to: Date?) async throws -> [Sample] {
        try Self.decode([Sample].self, "samples.json")
    }
    func rollups(limit: Int?, from: Date?, to: Date?) async throws -> [Rollup] {
        newInstall ? [] : try Self.decode([Rollup].self, "rollups.json")
    }
    func census() async throws -> Census? { try Self.decode(Census.self, "census-full.json") }
    func alerts(since: Date?, limit: Int?) async throws -> [AlertEpisode] {
        newInstall ? [] : [try Self.decode(AlertEpisode.self, "alert-episode.json")]
    }
    func deltas(vs: String?) async throws -> Deltas { try Self.decode(Deltas.self, "deltas-normal.json") }
    func stats() async throws -> Stats { try Self.decode(Stats.self, "stats.json") }
    func reapStaleSessionsPreview() async throws -> SessionReapReport {
        try Self.decode(SessionReapReport.self, "reap-sessions.json")
    }
    func reapStaleSessionsExecute() async throws -> SessionReapReport { try await reapStaleSessionsPreview() }
    func reapOrphansPreview() async throws -> OrphanReapReport {
        try Self.decode(OrphanReapReport.self, "reap-orphans.json")
    }
    func reapOrphansExecute() async throws -> OrphanReapReport { try await reapOrphansPreview() }
    func reapRunawaysPreview() async throws -> RunawayReapReport {
        try Self.decode(RunawayReapReport.self, "reap-runaways.json")
    }
    func reapRunawaysExecute() async throws -> RunawayReapReport { try await reapRunawaysPreview() }
}

@MainActor
final class ScreenRenderTests: XCTestCase {
    enum Look: String, CaseIterable {
        case light, dark
        var appearance: NSAppearance.Name { self == .light ? .aqua : .darkAqua }
    }

    /// One named screen: its view, and the size it is drawn at.
    struct Screen {
        let name: String
        let size: CGSize
        let view: AnyView
    }

    // ---- models ---------------------------------------------------------------

    private func pressureModel(_ fixture: String) async -> PressureModel {
        let model = PressureModel(client: FixtureClient(pressureFixture: fixture))
        await model.refresh()
        return model
    }

    private func detailModel(newInstall: Bool = false) async -> DetailModel {
        let model = DetailModel(client: FixtureClient(pressureFixture: "shrieking", newInstall: newInstall))
        await model.refreshHistory()
        await model.refreshCensus()
        await model.refreshAlerts()
        await model.refreshStats()
        return model
    }

    /// The screens, each loaded the way the app loads it. The window's default
    /// size is 560×460 (`BansheeApp`); the popover sets its own width.
    private func screens() async throws -> [Screen] {
        let window = CGSize(width: 560, height: 460)
        let shrieking = await pressureModel("shrieking")
        let quiet = await pressureModel("quiet")
        let detail = await detailModel()
        let newInstall = await detailModel(newInstall: true)
        let sessions = try FixtureClient.decode(SessionReapReport.self, "reap-sessions.json")
        // A throwaway defaults suite, so drawing Settings never reads or writes
        // the real preferences of whoever runs the suite.
        let suite = "banshee.render.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }

        return [
            Screen(name: "verdict-shrieking", size: CGSize(width: 560, height: 620),
                   view: AnyView(ContentView().environment(shrieking))),
            Screen(name: "verdict-quiet", size: window, view: AnyView(ContentView().environment(quiet))),
            Screen(name: "popover-shrieking", size: CGSize(width: 320, height: 300),
                   view: AnyView(MenuBarPanel().environment(shrieking))),
            Screen(name: "popover-quiet", size: CGSize(width: 320, height: 220),
                   view: AnyView(MenuBarPanel().environment(quiet))),
            Screen(name: "history", size: window, view: AnyView(HistoryView().environment(detail))),
            Screen(name: "census", size: CGSize(width: 560, height: 700),
                   view: AnyView(CensusView().environment(detail))),
            Screen(name: "alerts", size: window, view: AnyView(AlertsView().environment(detail))),
            // A new install's first look: no rollups and no episodes yet.
            Screen(name: "history-empty", size: window, view: AnyView(HistoryView().environment(newInstall))),
            Screen(name: "alerts-empty", size: window, view: AnyView(AlertsView().environment(newInstall))),
            Screen(name: "reap-preview", size: CGSize(width: 460, height: 360),
                   view: AnyView(SessionCandidateList(report: sessions).padding(Spacing.lg))),
            Screen(name: "settings", size: CGSize(width: 460, height: 520),
                   view: AnyView(SettingsView(preferences: Preferences(defaults: defaults)))),
        ]
    }

    // ---- drawing --------------------------------------------------------------

    /// Draw a view offscreen at a fixed 2x in one appearance.
    ///
    /// Fixed scale and PINNED appearance, because a headless runner has no screen
    /// (it would draw at 1x) and no appearance preference (it would draw light):
    /// the pixels must not depend on who runs this.
    private func draw(_ screen: Screen, _ look: Look) throws -> NSBitmapImageRep {
        let size = screen.size
        let hosting = NSHostingView(rootView:
            screen.view
                .tint(Palette.accent)
                .frame(width: size.width, height: size.height, alignment: .topLeading)
                // The window paints this behind its content; offscreen nothing does.
                .background(Palette.background)
        )
        hosting.appearance = NSAppearance(named: look.appearance)
        hosting.frame = NSRect(origin: .zero, size: size)
        hosting.layoutSubtreeIfNeeded()
        // Let SwiftUI commit its first pass and any `.task` over loaded state.
        RunLoop.main.run(until: Date().addingTimeInterval(0.4))
        hosting.layoutSubtreeIfNeeded()

        let scale: CGFloat = 2
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale),
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0), "no bitmap for \(screen.name)")
        rep.size = size
        hosting.cacheDisplay(in: hosting.bounds, to: rep)
        return rep
    }

    /// What a render amounts to, sampled on a grid: how many distinct colours it
    /// has (a blank or single-colour canvas has one or two) and how bright it is.
    private func measure(_ rep: NSBitmapImageRep) -> (colours: Int, brightness: Double) {
        var seen = Set<UInt32>()
        var total = 0.0, count = 0.0
        for y in stride(from: 0, to: rep.pixelsHigh, by: 4) {
            for x in stride(from: 0, to: rep.pixelsWide, by: 4) {
                guard let c = rep.colorAt(x: x, y: y) else { continue }
                let (r, g, b) = (c.redComponent, c.greenComponent, c.blueComponent)
                // Quantised to 5 bits a channel, so antialiasing noise alone does
                // not read as "many colours".
                seen.insert(UInt32(r * 31) << 10 | UInt32(g * 31) << 5 | UInt32(b * 31))
                total += Double(0.2126 * r + 0.7152 * g + 0.0722 * b)
                count += 1
            }
        }
        return (seen.count, count > 0 ? total / count : 0)
    }

    private func captureDirectory() throws -> URL? {
        guard let path = ProcessInfo.processInfo.environment["BANSHEE_CAPTURE_DIR"], !path.isEmpty else {
            return nil
        }
        let url = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    // ---- the gate (and, on request, the capture) ------------------------------

    /// Every screen draws real content in both appearances, and the appearances
    /// differ. With `BANSHEE_CAPTURE_DIR` set, the renders are also written out.
    ///
    /// The colour floor is what tells "drew the screen" from "drew the backdrop":
    /// `testABlankViewFailsTheSameMeasure` proves a blank canvas sits below it.
    func testEveryScreenDrawsInLightAndDark() async throws {
        let directory = try captureDirectory()
        var written: [String] = []
        for screen in try await screens() {
            var brightness: [Look: Double] = [:]
            for look in Look.allCases {
                let rep = try draw(screen, look)
                let m = measure(rep)
                XCTAssertGreaterThanOrEqual(
                    m.colours, Self.colourFloor, "\(screen.name) (\(look.rawValue)) drew almost nothing")
                brightness[look] = m.brightness
                if let directory {
                    let file = directory.appending(path: "\(screen.name)-\(look.rawValue).png")
                    let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
                    try png.write(to: file)
                    written.append(file.lastPathComponent)
                }
            }
            let (light, dark) = (try XCTUnwrap(brightness[.light]), try XCTUnwrap(brightness[.dark]))
            XCTAssertGreaterThan(
                light, dark + 0.2, "\(screen.name): light is not clearly brighter than dark (\(light) vs \(dark))")
        }
        if let directory {
            print("banshee-capture: wrote \(written.count) images to \(directory.path)")
        }
    }

    /// A render with this few distinct colours is a backdrop, not a screen.
    static let colourFloor = 12

    /// The measure can tell a drawn screen from nothing: a view that draws only
    /// its background lands under the floor. Without this, a floor set too low
    /// would pass every blank render and the gate above would prove nothing.
    func testABlankViewFailsTheSameMeasure() throws {
        let blank = Screen(name: "blank", size: CGSize(width: 320, height: 200), view: AnyView(Color.clear))
        for look in Look.allCases {
            XCTAssertLessThan(measure(try draw(blank, look)).colours, Self.colourFloor, look.rawValue)
        }
    }
}
