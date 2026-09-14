// DaemonService — what the app knows about the LaunchAgent it does NOT own.
//
// This file used to be `APIServerManager`, and it used to launch `banshee-api` as
// a child process and parse its stdout for the bound port. **That whole path is
// gone** (ADR-0004): the daemon is a launchd LaunchAgent now, so the app is a
// CLIENT of it, not its supervisor.
//
// The deletion was not tidying. Keeping a spawn path alive alongside an installed
// agent means the app opens a SECOND `banshee-api` on the same database file the
// moment the agent is running — two writing processes, which is the one invariant
// ADR-0001 rests on ("one writer, one place for auth and invariants"). The second
// instance would also fail to bind 18769, and in the dev profile would climb to
// 18770 and start sampling in parallel. Nothing about that is visible from the UI.
//
// So the app's posture is: connect, and treat "daemon not running" as a
// first-class state with a repair action.

import Foundation
import os

public struct DaemonService: Sendable {
    public static let shared = DaemonService()

    /// The launchd label. Matches `scripts/lib/service.sh`, which is the one place
    /// the install and check scripts read it from.
    public static let label = "com.tedswinyar.banshee-api"

    private let logger = Logger(subsystem: "com.tedswinyar.banshee", category: "Daemon")

    public init() {}

    /// Where launchd sends the daemon's output. `banshee-api` logs to stderr, and
    /// **a `tracing::error!` is visible ONLY here** — if a user or operator has to
    /// see something, it must travel over the API instead.
    public static var logFileURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Logs/banshee-api.log")
    }

    public static var errorLogFileURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Logs/banshee-api.err.log")
    }

    public static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/LaunchAgents/\(label).plist")
    }

    /// Whether a plist exists at all — "never installed" and "installed but not
    /// running" need different repair actions, and conflating them sends the user
    /// to the wrong one.
    public static var isInstalled: Bool {
        FileManager.default.fileExists(atPath: plistURL.path)
    }

    /// Parse the daemon's announcement line out of its log.
    ///
    /// Kept even though the app no longer reads a pipe, because the announcement
    /// is still **the only truthful source of the bound port**: the configured port
    /// is a request. The prod profile pins it and refuses to climb (ADR-0004), so
    /// in practice a daemon either serves 18769 or is not serving — but a dev or
    /// foreground instance can be anywhere on the ladder, and this is how anything
    /// finds it.
    ///
    /// Format: `banshee-api listening on http://127.0.0.1:18769`
    public static func parseAnnouncement(_ line: String) -> URL? {
        let prefix = "banshee-api listening on "
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix(prefix) else { return nil }
        return URL(string: String(trimmed.dropFirst(prefix.count)))
    }

    /// The LAST announced URL in a log, or nil.
    ///
    /// Last, not first: launchd appends across restarts, so the top of the file is
    /// whatever port the daemon bound days ago. Reading the first match is how you
    /// end up talking to a port nothing has served since Tuesday.
    public static func lastAnnouncedURL(inLog contents: String) -> URL? {
        var found: URL?
        for line in contents.split(separator: "\n", omittingEmptySubsequences: true) {
            if let url = parseAnnouncement(String(line)) { found = url }
        }
        return found
    }

    /// What to tell the user, and what to run, when nothing is answering.
    ///
    /// The message names a COMMAND rather than describing a state, because "the
    /// daemon is not running" is not actionable and `make daemon-install` is.
    public static func repairAdvice() -> String {
        if isInstalled {
            return """
                Banshee's background monitor is installed but not responding right \
                now. Click Restart to relaunch it; if it keeps happening, quit and \
                reopen Banshee.
                """
        }
        return """
            Banshee's background monitor isn't running yet, so nothing is being \
            sampled. Click Install to set it up — it keeps watching after you \
            close this window. Until then there's no history to judge, which is \
            not the same as a healthy machine.
            """
    }

    /// The `launchctl` arguments that restart an INSTALLED agent — the same
    /// command `repairAdvice()` tells the user to type, which is the point: the
    /// button and the advice must never name different repairs.
    ///
    /// `kickstart -k` kills and restarts the service in place. It is only a
    /// repair for the installed-but-not-answering state; when no plist exists
    /// there is nothing to kick, which is why the UI offers the button only
    /// when `isInstalled`.
    public static func repairCommandArguments(uid: uid_t = getuid()) -> [String] {
        ["kickstart", "-k", "gui/\(uid)/\(label)"]
    }

    /// Run the repair. Returns whether `launchctl` reported success — which
    /// means "launchd accepted the kick", not "the daemon is healthy"; the
    /// caller still re-probes, exactly as it would after the user ran the
    /// command by hand.
    public func repair() async -> Bool {
        let arguments = Self.repairCommandArguments()
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            // Off the calling actor: waitUntilExit blocks its thread.
            await Task.detached { process.waitUntilExit() }.value
            let ok = process.terminationStatus == 0
            if !ok {
                logger.error("launchctl \(arguments.joined(separator: " ")) exited \(process.terminationStatus)")
            }
            return ok
        } catch {
            logger.error("could not run launchctl: \(error.localizedDescription)")
            return false
        }
    }

    /// Is a daemon answering through `client`?
    ///
    /// Goes through the client's own transport (ADR-0008: the socket in production,
    /// TCP for an explicit URL or an old daemon) rather than a hardcoded loopback URL,
    /// so "reachable" is judged on the path the app will actually use. `/health` is
    /// the open route, so this needs no API key — which matters: a missing key must
    /// report "locked out" rather than masquerading as "no daemon", and those are
    /// different repairs.
    public func probe(_ client: APIClientProtocol, attempts: Int = 3) async -> Bool {
        // Retry a few times: the sampler leaves gaps by design (every reinstall and
        // every laptop wake restarts the daemon), so a single miss must not latch
        // "nothing is watching". A healthy daemon answers on the first attempt in
        // milliseconds; only a real outage exhausts the retries.
        for attempt in 1...max(1, attempts) {
            do {
                // `health()` is false on a 503 — the store is unusable, the process is
                // there but cannot serve — which is treated as NOT reachable so the UI
                // surfaces a real problem.
                if try await client.health() { return true }
                logger.error("daemon probe: /health did not answer ok (attempt \(attempt)/\(attempts))")
            } catch {
                logger.error("daemon probe failed (attempt \(attempt)/\(attempts)): \(error.localizedDescription)")
            }
            if attempt < attempts { try? await Task.sleep(nanoseconds: 400_000_000) }
        }
        return false
    }
}
