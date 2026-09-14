// DaemonInstaller pins.
//
// Everything runs against a TEMP directory and a fake launchctl, never the real
// machine — the same posture as scripts/tests/test-service.sh, which drives the
// SHELL installer with BANSHEE_* overrides. A gate that can only be tested by
// registering a real LaunchAgent is a gate nobody tests.
//
// The plist assertions mirror what scripts/check-service.sh enforces (installed
// path, prod profile, pinned port, ProcessType Standard), because the Swift
// installer must produce a plist the checker PASSES; the two installers must not
// drift.

import Foundation
import XCTest

@testable import BansheeCore

final class DaemonInstallerTests: XCTestCase {
    private var work: URL!

    override func setUpWithError() throws {
        work = FileManager.default.temporaryDirectory
            .appending(path: "banshee-installer-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: work)
    }

    /// A fake source binary at `path` (executable, distinctive content).
    private func makeBinary(_ name: String, under dir: URL? = nil) throws -> URL {
        let parent = dir ?? work.appending(path: "source")
        try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
        let url = parent.appending(path: name)
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: url)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    /// An Environment rooted in the temp dir. `launchctl` calls are recorded;
    /// `printStatuses` scripts what `launchctl print` returns per call (default:
    /// service absent). Poll interval is 1ns so retry loops cost nothing.
    private func makeEnvironment(
        bootstrapStatus: Int32 = 0,
        printStatuses: [Int32] = [],
        healthAnswers: [Bool] = [true],
        calls: Recorder = Recorder()
    ) -> (DaemonInstaller.Environment, Recorder) {
        let printBox = Box(printStatuses)
        let healthBox = Box(healthAnswers)
        let env = DaemonInstaller.Environment(
            launchdDir: work.appending(path: "LaunchAgents"),
            installDir: work.appending(path: "install/bin"),
            logDir: work.appending(path: "Logs"),
            label: "com.tedswinyar.banshee-api-TEST",
            port: 18991,
            uid: 501,
            runLaunchctl: { args in
                await calls.record(args)
                switch args.first {
                case "print": return await printBox.next(default: 1)
                case "bootstrap": return bootstrapStatus
                default: return 0
                }
            },
            probeHealth: { await healthBox.next(default: false) },
            pollIntervalNanos: 1
        )
        return (env, calls)
    }

    /// The typed error out of a Result<Void, _>, which is not itself Equatable.
    private func error(
        of result: Result<Void, DaemonInstaller.InstallError>
    ) -> DaemonInstaller.InstallError? {
        if case let .failure(error) = result { return error }
        return nil
    }

    private func loadPlist(_ env: DaemonInstaller.Environment) throws -> [String: Any] {
        let url = env.launchdDir.appending(path: "\(env.label).plist")
        let data = try Data(contentsOf: url)
        let any = try PropertyListSerialization.propertyList(from: data, format: nil)
        return try XCTUnwrap(any as? [String: Any])
    }

    // MARK: - The plist contract (what check-service.sh enforces)

    func testInstallWritesThePlistCheckServiceExpects() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        let (env, _) = makeEnvironment()

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        guard case .success = result else {
            return XCTFail("install failed: \(result)")
        }

        let plist = try loadPlist(env)
        XCTAssertEqual(plist["Label"] as? String, env.label)
        // ProgramArguments[0] must be the INSTALLED path — never the source, and
        // never anything under target/ (the eight-day outage predicate).
        let args = try XCTUnwrap(plist["ProgramArguments"] as? [String])
        XCTAssertEqual(args, [env.installDir.appending(path: "banshee-api").path])
        XCTAssertFalse(args[0].contains("/target/"))

        let envVars = try XCTUnwrap(plist["EnvironmentVariables"] as? [String: String])
        XCTAssertEqual(envVars["BANSHEE_PROFILE"], "prod")
        XCTAssertEqual(envVars["BANSHEE_PORT"], "18991", "the port is pinned; prod does not climb")
        // No credential pass-through from an app-initiated install.
        XCTAssertNil(envVars["BANSHEE_SLACK_WEBHOOK_URL"])
        XCTAssertEqual(envVars.count, 2, "EnvironmentVariables stays minimal: \(envVars)")

        XCTAssertEqual(plist["RunAtLoad"] as? Bool, true)
        let keepAlive = try XCTUnwrap(plist["KeepAlive"] as? [String: Bool])
        XCTAssertEqual(keepAlive["SuccessfulExit"], false, "respect a clean exit so bootout sticks")
        XCTAssertEqual(plist["ThrottleInterval"] as? Int, 5)
        XCTAssertEqual(
            plist["ProcessType"] as? String, "Standard",
            "Background + LowPriorityIO throttled a sampler into missing its own schedule")
        XCTAssertNil(plist["LowPriorityIO"])
        XCTAssertEqual(
            plist["StandardErrorPath"] as? String,
            env.logDir.appending(path: "banshee-api.err.log").path)
        XCTAssertEqual(
            plist["StandardOutPath"] as? String,
            env.logDir.appending(path: "banshee-api.log").path)
    }

    func testThePlistIsOwnerOnly() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        let (env, _) = makeEnvironment()

        _ = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)

        let plistPath = env.launchdDir.appending(path: "\(env.label).plist").path
        let mode = try XCTUnwrap(
            FileManager.default.attributesOfItem(atPath: plistPath)[.posixPermissions] as? Int)
        XCTAssertEqual(mode & 0o777, 0o600, "the plist may carry credentials; 0600 like the key file")
    }

    // MARK: - Atomic staging

    func testInstallIsAtomicAndLeavesNoIncomingBehind() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        let (env, _) = makeEnvironment()

        // Pre-existing installed binaries — the UPDATE case, which is the normal one.
        try FileManager.default.createDirectory(at: env.installDir, withIntermediateDirectories: true)
        try Data("old\n".utf8).write(to: env.installDir.appending(path: "banshee-api"))

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        guard case .success = result else {
            return XCTFail("install failed: \(result)")
        }

        let fm = FileManager.default
        for name in ["banshee-api", "banshee"] {
            let installed = env.installDir.appending(path: name)
            XCTAssertTrue(fm.isExecutableFile(atPath: installed.path), "\(name) not executable")
            XCTAssertEqual(
                try String(contentsOf: installed, encoding: .utf8), "#!/bin/sh\nexit 0\n",
                "\(name) was not replaced")
            XCTAssertFalse(
                fm.fileExists(atPath: installed.path + ".incoming"),
                "a leftover .incoming means the staging never completed atomically")
        }
    }

    // MARK: - Refusals

    func testABuildDirectorySourceIsRefusedBeforeAnythingIsWritten() async throws {
        // A REAL executable inside a fake target/ tree, so the only thing wrong
        // is the path (same construction as test-service.sh).
        let targetDir = work.appending(path: "repo/rust/target/release")
        let api = try makeBinary("banshee-api", under: targetDir)
        let cli = try makeBinary("banshee", under: targetDir)
        let (env, calls) = makeEnvironment()

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)

        XCTAssertEqual(error(of: result), .buildDirectorySource(api.path))
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: env.launchdDir.path),
            "a refused install must not leave a plist directory behind")
        let recorded = await calls.all()
        XCTAssertTrue(recorded.isEmpty, "launchctl must never run for a refused install: \(recorded)")
    }

    func testAMissingBinaryIsATypedFailure() async throws {
        let cli = try makeBinary("banshee")
        let missing = work.appending(path: "source/banshee-api-absent")
        let (env, _) = makeEnvironment()

        let result = await DaemonInstaller(environment: env).install(apiBinary: missing, cliBinary: cli)
        XCTAssertEqual(error(of: result), .missingBinary(missing.path))
    }

    // MARK: - launchd interaction

    func testBootstrapFailureIsTypedWithItsExitStatus() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        let (env, _) = makeEnvironment(bootstrapStatus: 5)

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        XCTAssertEqual(error(of: result), .bootstrapFailed(5))
    }

    func testAnExistingServiceIsBootedOutAndPolledBeforeBootstrap() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        // print: present (0), then still present once after bootout (0), then gone (1).
        let (env, calls) = makeEnvironment(printStatuses: [0, 0, 1])

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        guard case .success = result else {
            return XCTFail("install failed: \(result)")
        }

        let verbs = await calls.all().map(\.[0])
        let target = "gui/501/\(env.label)"
        XCTAssertEqual(
            verbs, ["print", "bootout", "print", "print", "bootstrap", "kickstart"],
            "bootout must be POLLED (it is asynchronous), and bootstrap must wait for it")
        let bootout = await calls.all().first { $0.first == "bootout" }
        XCTAssertEqual(bootout, ["bootout", target])
    }

    func testANeverHealthyDaemonIsATypedFailureNotSuccess() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        let (env, _) = makeEnvironment(healthAnswers: [])  // never healthy

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        XCTAssertEqual(
            error(of: result), .neverHealthy,
            "launchd accepting the service is not the same claim as the daemon serving")
    }

    func testHealthIsPolledNotProbedOnce() async throws {
        let api = try makeBinary("banshee-api")
        let cli = try makeBinary("banshee")
        // Healthy only on the third probe — a fixed single probe would fail here.
        let (env, _) = makeEnvironment(healthAnswers: [false, false, true])

        let result = await DaemonInstaller(environment: env).install(apiBinary: api, cliBinary: cli)
        guard case .success = result else {
            return XCTFail("a daemon that answers on the third poll is a healthy install: \(result)")
        }
    }

    // MARK: - Bundle discovery

    func testBundledBinariesRequiresBothHelpersExecutable() throws {
        let bundle = work.appending(path: "Fake.app")
        let helpers = bundle.appending(path: "Contents/Helpers")
        XCTAssertNil(DaemonInstaller.bundledBinaries(in: bundle), "no Helpers dir → not a bundle install")

        _ = try makeBinary("banshee-api", under: helpers)
        XCTAssertNil(DaemonInstaller.bundledBinaries(in: bundle), "the CLI is required too")

        _ = try makeBinary("banshee", under: helpers)
        let found = try XCTUnwrap(DaemonInstaller.bundledBinaries(in: bundle))
        XCTAssertEqual(found.api.lastPathComponent, "banshee-api")
        XCTAssertEqual(found.cli.lastPathComponent, "banshee")
        XCTAssertTrue(found.api.path.contains("Contents/Helpers"), "helpers live in Helpers/, not MacOS/")
    }
}

// MARK: - test doubles

/// Records launchctl invocations.
actor Recorder {
    private var calls: [[String]] = []
    func record(_ args: [String]) { calls.append(args) }
    func all() -> [[String]] { calls }
}

/// Hands out scripted values, then a default forever after.
actor Box<Value: Sendable> {
    private var values: [Value]
    init(_ values: [Value]) { self.values = values }
    func next(default fallback: Value) -> Value {
        values.isEmpty ? fallback : values.removeFirst()
    }
}
