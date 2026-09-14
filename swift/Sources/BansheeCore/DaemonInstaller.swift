// DaemonInstaller — install (or update) the LaunchAgent from the app bundle.
//
// This is the DMG user's `make daemon-install`: a drag-and-drop install has no
// source checkout, but the app bundle already ships the daemon in
// Contents/Helpers/ (build-app.sh), so the app can register it itself.
//
// INSTALLING does not make the app the daemon's supervisor (ADR-0004): this is a
// one-time act that registers *launchd* as the supervisor and then steps away.
// The app remains a client — it never spawns `banshee-api`, never holds a child
// process, and after install it re-probes /health exactly as if the user had run
// the shell installer by hand.
//
// The shape mirrors scripts/install-launchd.sh + scripts/lib/service.sh, which
// stay the canonical spelling; drift between the two installers is the same bug
// as drift between installer and checker, so every value here names the scar
// that fixed it there:
//   - install dir under ~/Library/Application Support, NEVER a build dir
//     (the eight-day deleted-inode outage; check-service.sh refuses target/)
//   - atomic `.incoming` copy + rename(2), so a running daemon keeps serving
//     from its old inode until kickstart
//   - plist 0600 (it CAN carry credentials when written by the shell installer;
//     same posture here even though this one writes none)
//   - ProcessType Standard, no LowPriorityIO (Background throttled an I/O-bound
//     job into missing its own schedule)
//   - BANSHEE_PORT pinned; prod does not climb the ladder
//   - launchctl bootstrap/bootout/kickstart, never load/unload; bootout is
//     asynchronous and must be polled
//   - poll /health, never sleep a fixed time

import Foundation

/// Installs the bundled daemon as a launchd LaunchAgent.
///
/// Every filesystem root and process boundary is injectable (the same pattern as
/// the scripts suite's `BANSHEE_*` env overrides) so tests never touch the real
/// LaunchAgents directory or launchctl.
public struct DaemonInstaller: Sendable {
    /// The boundaries: where to install, and how to reach launchd + the daemon.
    public struct Environment: Sendable {
        public var launchdDir: URL
        public var installDir: URL
        public var logDir: URL
        public var label: String
        public var port: UInt16
        public var uid: uid_t
        /// Runs `launchctl` with the given arguments; returns the exit status.
        public var runLaunchctl: @Sendable ([String]) async -> Int32
        /// Is a daemon answering /health right now?
        public var probeHealth: @Sendable () async -> Bool
        /// Delay between polls, in nanoseconds. Tests set 1 so a 100-step poll
        /// costs nothing; production keeps the shell installer's 100ms.
        public var pollIntervalNanos: UInt64

        public init(
            launchdDir: URL, installDir: URL, logDir: URL,
            label: String = DaemonService.label,
            port: UInt16 = 18769,
            uid: uid_t = getuid(),
            runLaunchctl: @escaping @Sendable ([String]) async -> Int32,
            probeHealth: @escaping @Sendable () async -> Bool,
            pollIntervalNanos: UInt64 = 100_000_000
        ) {
            self.launchdDir = launchdDir
            self.installDir = installDir
            self.logDir = logDir
            self.label = label
            self.port = port
            self.uid = uid
            self.runLaunchctl = runLaunchctl
            self.probeHealth = probeHealth
            self.pollIntervalNanos = pollIntervalNanos
        }

        /// The real machine: the same paths as scripts/lib/service.sh, launchctl
        /// via Process, health via DaemonService.probe on the pinned prod port.
        public static func live() -> Environment {
            let home = FileManager.default.homeDirectoryForCurrentUser
            return Environment(
                launchdDir: home.appending(path: "Library/LaunchAgents"),
                installDir: home.appending(path: "Library/Application Support/banshee/bin"),
                logDir: home.appending(path: "Library/Logs"),
                runLaunchctl: { arguments in
                    let process = Process()
                    process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
                    process.arguments = arguments
                    process.standardOutput = FileHandle.nullDevice
                    process.standardError = FileHandle.nullDevice
                    do {
                        try process.run()
                        await Task.detached { process.waitUntilExit() }.value
                        return process.terminationStatus
                    } catch {
                        return -1
                    }
                },
                probeHealth: {
                    // Resolved per probe, not once: the install that just ran is what
                    // CREATES the socket, so a client resolved before it would still be
                    // on TCP.
                    await DaemonService.shared.probe(APIClient.fromEnvironment())
                }
            )
        }
    }

    public enum InstallError: Error, Equatable, Sendable {
        /// A source binary is missing or not executable.
        case missingBinary(String)
        /// A source path is inside a build directory — a build directory is not
        /// an installation (ADR-0004), refuse exactly as check-service.sh would.
        case buildDirectorySource(String)
        /// A filesystem step failed (copy, rename, plist write, chmod).
        case io(String)
        /// `launchctl bootstrap` returned nonzero.
        case bootstrapFailed(Int32)
        /// launchd accepted the service but /health never answered.
        case neverHealthy

        /// One user-facing sentence, because the button that runs this needs
        /// something honest to show when it fails.
        public var userMessage: String {
            switch self {
            case .missingBinary(let path):
                return "The app bundle is missing its daemon binary (\(path)). Reinstall the app."
            case .buildDirectorySource(let path):
                return "Refusing to install from a build directory (\(path))."
            case .io(let detail):
                return "Install failed: \(detail)"
            case .bootstrapFailed(let status):
                return "launchd refused the service (bootstrap exited \(status)). See ~/Library/Logs/banshee-api.err.log."
            case .neverHealthy:
                return "The daemon was registered but never answered. See ~/Library/Logs/banshee-api.err.log."
            }
        }
    }

    public let environment: Environment

    public init(environment: Environment) {
        self.environment = environment
    }

    /// The bundled daemon + CLI, or nil when this is not an assembled app bundle
    /// (e.g. `swift run` during development). The helpers live in
    /// Contents/Helpers/, NOT Contents/MacOS/ — on the default case-insensitive
    /// filesystem `MacOS/Banshee` and `MacOS/banshee` are the same file, and the
    /// CLI once silently overwrote the app there (build-app.sh's scar). That
    /// also means `Bundle.url(forAuxiliaryExecutable:)` cannot find them; the
    /// path is spelled directly.
    public static func bundledBinaries(in bundleURL: URL) -> (api: URL, cli: URL)? {
        let helpers = bundleURL.appending(path: "Contents/Helpers")
        let api = helpers.appending(path: "banshee-api")
        let cli = helpers.appending(path: "banshee")
        let fm = FileManager.default
        guard fm.isExecutableFile(atPath: api.path), fm.isExecutableFile(atPath: cli.path)
        else { return nil }
        return (api, cli)
    }

    /// Install (or update) the LaunchAgent from `apiBinary` + `cliBinary`.
    /// Idempotent, like the shell installer: running it again is the documented
    /// way to update.
    public func install(apiBinary: URL, cliBinary: URL) async -> Result<Void, InstallError> {
        let fm = FileManager.default

        // Refuse a build-directory source BEFORE touching anything: installing
        // out of target/ recreates the eight-day outage with a different entry
        // point. Same predicate as service.sh's program_path_is_installed.
        for source in [apiBinary, cliBinary] {
            if source.pathComponents.contains("target") {
                return .failure(.buildDirectorySource(source.path))
            }
            guard fm.isExecutableFile(atPath: source.path) else {
                return .failure(.missingBinary(source.path))
            }
        }

        do {
            try fm.createDirectory(at: environment.installDir, withIntermediateDirectories: true)
            try fm.createDirectory(at: environment.launchdDir, withIntermediateDirectories: true)
            try fm.createDirectory(at: environment.logDir, withIntermediateDirectories: true)
        } catch {
            return .failure(.io("could not create install directories: \(error.localizedDescription)"))
        }

        // Atomic install: copy to `.incoming`, then rename(2) over the
        // destination. rename replaces the directory entry without touching the
        // inode a running daemon holds, so an update never corrupts a live
        // binary; the old process keeps serving until the kickstart below.
        let installedBin = environment.installDir.appending(path: "banshee-api")
        for (source, destination) in [
            (apiBinary, installedBin),
            (cliBinary, environment.installDir.appending(path: "banshee")),
        ] {
            if case let .failure(error) = installAtomically(from: source, to: destination) {
                return .failure(error)
            }
        }

        // The plist. Built as a dictionary and serialized, so it cannot be
        // syntactically invalid — the shell installer needs plutil -lint for its
        // heredoc; this construction is the lint.
        //
        // Deliberately NO Slack webhook block: the webhook is an operator
        // credential passed through the installing SHELL's environment
        // (service.sh), and an app-initiated install has no business exporting
        // one. EnvironmentVariables stays minimal.
        let plistURL = environment.launchdDir.appending(path: "\(environment.label).plist")
        let plist: [String: Any] = [
            "Label": environment.label,
            "ProgramArguments": [installedBin.path],
            "EnvironmentVariables": [
                "BANSHEE_PROFILE": "prod",
                "BANSHEE_PORT": String(environment.port),
            ],
            "RunAtLoad": true,
            "KeepAlive": ["SuccessfulExit": false],
            "ThrottleInterval": 5,
            "ProcessType": "Standard",
            "StandardOutPath": environment.logDir.appending(path: "banshee-api.log").path,
            "StandardErrorPath": environment.logDir.appending(path: "banshee-api.err.log").path,
        ]
        do {
            let data = try PropertyListSerialization.data(
                fromPropertyList: plist, format: .xml, options: 0)
            try data.write(to: plistURL, options: .atomic)
            // 0600: the plist written by the SHELL installer can carry the Slack
            // credential, and check-service/uninstall treat the file uniformly —
            // same posture here so permissions never depend on which installer
            // ran last (banshee-0sa).
            try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: plistURL.path)
        } catch {
            return .failure(.io("could not write \(plistURL.path): \(error.localizedDescription)"))
        }

        // bootstrap/bootout/kickstart, never load/unload (the legacy verbs fail
        // quietly). bootout is ASYNCHRONOUS: poll `print` for the service to
        // disappear rather than assuming it is gone.
        let target = "gui/\(environment.uid)/\(environment.label)"
        if await environment.runLaunchctl(["print", target]) == 0 {
            _ = await environment.runLaunchctl(["bootout", target])
            for _ in 0..<50 {
                if await environment.runLaunchctl(["print", target]) != 0 { break }
                try? await Task.sleep(nanoseconds: environment.pollIntervalNanos)
            }
        }

        let bootstrapStatus = await environment.runLaunchctl([
            "bootstrap", "gui/\(environment.uid)", plistURL.path,
        ])
        guard bootstrapStatus == 0 else {
            return .failure(.bootstrapFailed(bootstrapStatus))
        }
        // Restart in place if RunAtLoad already started one, so an UPDATE takes
        // effect now. Best-effort, exactly like the shell installer.
        _ = await environment.runLaunchctl(["kickstart", "-k", target])

        // Prove it is serving. Poll, never sleep a fixed time: a fixed sleep is
        // wrong on both fast and loaded machines.
        for _ in 0..<100 {
            if await environment.probeHealth() { return .success(()) }
            try? await Task.sleep(nanoseconds: environment.pollIntervalNanos)
        }
        return .failure(.neverHealthy)
    }

    private func installAtomically(from source: URL, to destination: URL) -> Result<Void, InstallError> {
        let fm = FileManager.default
        let incoming = destination.appendingPathExtension("incoming")
        do {
            try? fm.removeItem(at: incoming)
            try fm.copyItem(at: source, to: incoming)
            try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: incoming.path)
            // rename(2): atomic replacement of the directory entry.
            guard rename(incoming.path, destination.path) == 0 else {
                try? fm.removeItem(at: incoming)
                return .failure(.io("rename to \(destination.path) failed: \(String(cString: strerror(errno)))"))
            }
            return .success(())
        } catch {
            try? fm.removeItem(at: incoming)
            return .failure(.io("could not stage \(destination.lastPathComponent): \(error.localizedDescription)"))
        }
    }
}
