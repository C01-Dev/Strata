import Foundation

/// The parts of `strata status --json` the menu shows.
struct Status: Decodable {
    var version: String
    var startedAt: Int
    var repos: [Repo]
    var checker: [Checker]
}

struct Repo: Decodable, Identifiable {
    var id: Int
    var root: String
    var baseBranch: String
    var agents: [Agent]
}

struct Agent: Decodable, Identifiable {
    var id: Int
    var name: String
    var path: String
    var branch: String
    var state: String
    var seedTier: String?
    var port: Int?
    var url: String?
    var memoryBytes: UInt64
    var budget: String
    var processes: [Proc]

    var devRunning: Bool { processes.contains { $0.kind == "dev" && $0.state == "running" } }
}

struct Proc: Decodable {
    var kind: String
    var state: String
}

struct Checker: Decodable {
    var typescript: String
    var memoryBytes: UInt64
    var agents: [CheckerAgent]
}

struct CheckerAgent: Decodable {}

struct Output: Sendable {
    var code: Int32
    var out: Data
    var err: String
}

/// Runs the `strata` CLI, the app's only way into the daemon.
enum Strata {
    /// Apps started from Finder get a bare PATH, so look where the installers put it.
    static let binary: URL? = {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let env = ProcessInfo.processInfo.environment["STRATA_BIN"]
        return [env, "\(home)/.cargo/bin/strata", "/opt/homebrew/bin/strata", "/usr/local/bin/strata", "\(home)/.local/bin/strata"]
            .compactMap { $0 }
            .first { FileManager.default.isExecutableFile(atPath: $0) }
            .map { URL(fileURLWithPath: $0) }
    }()

    static func run(_ args: [String], in dir: String? = nil) async -> Output {
        await Task.detached { runNow(args, in: dir) }.value
    }

    private static func runNow(_ args: [String], in dir: String?) -> Output {
        guard let binary else { return Output(code: -1, out: Data(), err: "strata is not installed") }
        let p = Process()
        p.executableURL = binary
        p.arguments = args
        if let dir { p.currentDirectoryURL = URL(fileURLWithPath: dir) }
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        p.environment = env
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        do { try p.run() } catch { return Output(code: -1, out: Data(), err: error.localizedDescription) }
        // ponytail: stdout is read to the end before stderr; fine for the short commands run here.
        let o = out.fileHandleForReading.readDataToEndOfFile()
        let e = err.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return Output(code: p.terminationStatus, out: o, err: String(decoding: e, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines))
    }
}

@MainActor @Observable
final class Model {
    var status: Status?
    /// Why there is no status: not installed, or the daemon is not running.
    var problem: String?
    /// The last action's outcome.
    var message: String?
    var failed = false

    var agentCount: Int { status?.repos.reduce(0) { $0 + $1.agents.count } ?? 0 }

    func poll() async {
        while !Task.isCancelled {
            await refresh()
            try? await Task.sleep(for: .seconds(2))
        }
    }

    func refresh() async {
        let r = await Strata.run(["status", "--json"])
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        if r.code == 0, let s = try? decoder.decode(Status.self, from: r.out) {
            status = s
            problem = nil
        } else {
            status = nil
            problem = r.err.isEmpty ? "Strata is not running" : r.err
        }
    }

    /// Runs a command, shows its first line of output (or its error), then refreshes.
    @discardableResult
    func act(_ args: [String], in dir: String? = nil) async -> Output {
        message = "strata \(args.joined(separator: " "))…"
        failed = false
        let r = await Strata.run(args, in: dir)
        failed = r.code != 0
        message = failed ? r.err : String(decoding: r.out, as: UTF8.self).split(separator: "\n").first.map(String.init)
        await refresh()
        return r
    }
}

func size(_ bytes: UInt64) -> String {
    bytes >= 1 << 30 ? String(format: "%.1f GB", Double(bytes) / Double(1 << 30)) : "\(bytes >> 20) MB"
}
