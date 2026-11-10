import AppKit
import SwiftUI

@main
struct StrataBarApp: App {
    @State private var model = Model()

    var body: some Scene {
        MenuBarExtra {
            Panel(model: model)
                .frame(width: Panel.width)
        } label: {
            MenuLabel(model: model)
                .task {
                    if let path = ProcessInfo.processInfo.environment["STRATA_SNAPSHOT"] {
                        await snapshot(to: path)
                    }
                    await model.poll()
                }
        }
        .menuBarExtraStyle(.window)
    }

    /// STRATA_SNAPSHOT=<png>: renders the panel once, for the README and for checking the
    /// layout without clicking the menu bar, then quits.
    private func snapshot(to path: String) async {
        await model.refresh()
        // An AppKit view, since ImageRenderer cannot draw buttons and menus.
        let view = NSHostingView(rootView: Panel(model: model).frame(width: Panel.width).background(.windowBackground))
        view.frame.size = view.fittingSize
        let window = NSWindow(contentRect: view.frame, styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        if let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
            view.cacheDisplay(in: view.bounds, to: rep)
            try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
        }
        NSApplication.shared.terminate(nil)
    }
}

/// C01's accent.
let ember = Color(red: 0.816, green: 0.392, blue: 0.227)

struct MenuLabel: View {
    let model: Model

    var body: some View {
        if model.status == nil {
            Image(systemName: "square.stack.3d.up.slash")
                .accessibilityLabel("Strata is not running")
        } else if !model.clashing.isEmpty {
            Image(systemName: "exclamationmark.triangle")
                .accessibilityLabel("Strata: agents' changes clash")
            Text("\(model.agentCount)")
        } else {
            Image(systemName: "square.stack.3d.up")
                .accessibilityLabel("Strata")
            if model.agentCount > 0 {
                Text("\(model.agentCount)")
            }
        }
    }
}

struct Panel: View {
    static let width: CGFloat = 380
    let model: Model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Header(model: model)
            Divider()
            VStack(alignment: .leading, spacing: 18) {
                if let status = model.status {
                    if status.repos.isEmpty {
                        Empty(icon: "folder.badge.plus", title: "No repos yet", detail: "Run `strata init` in a TypeScript repo.")
                    }
                    ForEach(status.repos) { repo in
                        RepoSection(repo: repo, checker: status.checker.first { $0.repo == repo.root }, model: model)
                    }
                } else {
                    Problem(model: model)
                }
            }
            .padding(14)
            if let message = model.message {
                Toast(message: message, failed: model.failed)
            }
            Divider()
            Footer(model: model)
        }
    }
}

struct Header: View {
    let model: Model

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "square.stack.3d.up.fill")
                .font(.title2)
                .foregroundStyle(ember)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text("Strata").font(.headline)
                Text(subtitle).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if model.status != nil {
                Pill(text: "\(model.agentCount) agent\(model.agentCount == 1 ? "" : "s")", color: model.clashing.isEmpty ? .green : .orange)
            } else {
                Pill(text: "Not running", color: .secondary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
    }

    private var subtitle: String {
        guard let s = model.status else { return "Parallel agents, one repo" }
        let up = max(0, Int(Date().timeIntervalSince1970) - s.startedAt)
        let uptime = up >= 86400 ? "\(up / 86400)d \(up % 86400 / 3600)h" : "\(up / 3600)h \(up % 3600 / 60)m"
        return "v\(s.version) · up \(uptime)"
    }
}

struct Pill: View {
    let text: String
    let color: Color

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text).font(.caption.weight(.medium))
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(.quaternary.opacity(0.6), in: Capsule())
    }
}

struct RepoSection: View {
    let repo: Repo
    let checker: Checker?
    let model: Model

    private var conflicts: [Conflict] { checker?.conflicts ?? [] }
    private var clashingAgents: Set<String> { Set(conflicts.filter { $0.skipped == nil }.flatMap(\.agents)) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(URL(fileURLWithPath: repo.root).lastPathComponent).font(.subheadline.weight(.semibold))
                Label(repo.baseBranch, systemImage: "arrow.triangle.branch")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .labelStyle(.titleAndIcon)
                Spacer()
            }
            ForEach(conflicts.filter { $0.skipped == nil }) { ClashCard(conflict: $0) }
            if repo.agents.isEmpty {
                Empty(icon: "person.2.badge.plus", title: "No agents", detail: "`strata new <name>` gives one its own worktree.")
            } else {
                VStack(spacing: 2) {
                    ForEach(repo.agents) { agent in
                        AgentRow(agent: agent, root: repo.root, clashing: clashingAgents.contains(agent.name), model: model)
                    }
                }
                .padding(4)
                .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            if let checker {
                CheckerLine(checker: checker)
            }
        }
    }
}

/// Two agents whose changes break each other: what breaks, and where.
struct ClashCard: View {
    let conflict: Conflict
    private static let shown = 3

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Text("\(conflict.title) clash").font(.callout.weight(.semibold))
                Spacer()
                Text("neither merged").font(.caption2).foregroundStyle(.secondary)
            }
            Text(conflict.summary).font(.caption).foregroundStyle(.secondary)
            ForEach(conflict.clashes.prefix(Self.shown)) { c in
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: "\(c.place)  TS\(c.code)").font(.caption.monospaced())
                    Text(c.message).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
                .help("\(c.file)\n\(c.message)")
            }
            ForEach(conflict.unmerged, id: \.self) { f in
                Label(URL(fileURLWithPath: f).lastPathComponent, systemImage: "arrow.triangle.merge")
                    .font(.caption.monospaced())
                    .help("\(f): both changed it, and git cannot merge the two versions")
            }
            if conflict.clashes.count > Self.shown {
                Text("and \(conflict.clashes.count - Self.shown) more in `strata status`").font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(.orange.opacity(0.35)))
        .accessibilityElement(children: .combine)
    }
}

struct AgentRow: View {
    let agent: Agent
    let root: String
    let clashing: Bool
    let model: Model
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityLabel(agent.state)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(agent.name).font(.callout.weight(.medium))
                    if agent.branch != agent.name {
                        Text(agent.branch).font(.caption).foregroundStyle(.secondary)
                    }
                    if clashing {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                            .accessibilityLabel("clashes with another agent")
                    }
                }
                Text(details)
                    .font(.caption)
                    .foregroundStyle(agent.budget == "over" ? .red : agent.budget == "warning" ? .orange : .secondary)
            }
            Spacer(minLength: 8)
            if let url = agent.url, let link = URL(string: url) {
                Button("Open \(url)", systemImage: "safari") { NSWorkspace.shared.open(link) }
                    .help(url)
            }
            Button(agent.devRunning ? "Stop dev server" : "Start dev server", systemImage: agent.devRunning ? "stop.circle" : "play.circle") {
                Task { await model.act([agent.devRunning ? "stop" : "dev", agent.name], in: root) }
            }
            .help(agent.devRunning ? "Stop dev server" : "Start dev server")
            .opacity(hovering || agent.devRunning ? 1 : 0)
            Menu {
                Button("Open in Terminal", systemImage: "terminal") { openInTerminal() }
                Button("Show in Finder", systemImage: "folder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: agent.path)]) }
                Button("Copy Path", systemImage: "doc.on.doc") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(agent.path, forType: .string)
                }
                Divider()
                Button("Remove…", systemImage: "trash", role: .destructive) { Task { await remove() } }
            } label: {
                Image(systemName: "ellipsis").accessibilityLabel("Actions for \(agent.name)")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(hovering ? Color.primary.opacity(0.07) : .clear, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    private var color: Color {
        switch agent.state {
        case "ready": .green
        case "orphaned": .red
        default: .orange
        }
    }

    private var details: String {
        var parts: [String] = []
        if agent.memoryBytes > 0 {
            parts.append(size(agent.memoryBytes) + (agent.budget == "over" ? " over budget" : agent.budget == "warning" ? " near budget" : ""))
        }
        if let tier = agent.seedTier { parts.append("deps \(tier)") }
        if let port = agent.port { parts.append(":\(port)") }
        if agent.state != "ready" { parts.append(agent.state) }
        return parts.joined(separator: " · ")
    }

    private func openInTerminal() {
        guard let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else { return }
        NSWorkspace.shared.open([URL(fileURLWithPath: agent.path)], withApplicationAt: terminal, configuration: NSWorkspace.OpenConfiguration())
    }

    /// Strata refuses to remove uncommitted work; only a second, explicit yes passes --force.
    private func remove() async {
        guard confirm("Remove \(agent.name)?", "Stops its processes and deletes its worktree. Strata refuses if there are uncommitted changes.", "Remove") else { return }
        let r = await model.act(["rm", agent.name], in: root)
        if r.code != 0, r.err.contains("uncommitted changes"),
           confirm("\(agent.name) has uncommitted changes", "\(r.err)\n\nDiscard them and remove the worktree?", "Discard and Remove") {
            await model.act(["rm", agent.name, "--force"], in: root)
        }
    }
}

struct CheckerLine: View {
    let checker: Checker

    var body: some View {
        let skipped = (checker.conflicts ?? []).filter { $0.skipped != nil }
        VStack(alignment: .leading, spacing: 3) {
            Label {
                Text(text)
            } icon: {
                Image(systemName: "checkmark.seal.fill").foregroundStyle(.green)
            }
            ForEach(skipped) { p in
                Label("\(p.title): \(p.summary)", systemImage: "minus.circle")
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    private var text: String {
        // A repo without the shared checker still has git's merges checked between agents.
        var parts = checker.typescript.isEmpty
            ? ["No shared checker"]
            : ["Shared checker", "TypeScript \(checker.typescript)", size(checker.memoryBytes)]
        if let n = checker.pairsChecked, n > 0 { parts.append("\(n) pair\(n == 1 ? "" : "s") checked") }
        return parts.joined(separator: " · ")
    }
}

struct Problem: View {
    let model: Model

    var body: some View {
        VStack(spacing: 10) {
            Empty(icon: Strata.binary == nil ? "arrow.down.circle" : "pause.circle",
                  title: Strata.binary == nil ? "Strata is not installed" : "Strata is not running",
                  detail: model.problem.flatMap { $0 == "Strata is not running" ? nil : String($0.trimmingPrefix("strata: ")) })
            if Strata.binary == nil {
                Link("How to install Strata", destination: URL(string: "https://github.com/C01-Dev/strata#install")!)
            } else if model.problem != nil {
                Button("Start Strata") { Task { await model.act(["daemon", "start"]) } }
                    .buttonStyle(.borderedProminent)
                    .tint(ember)
            }
        }
        .frame(maxWidth: .infinity)
    }
}

struct Empty: View {
    let icon: String
    let title: String
    let detail: String?

    var body: some View {
        VStack(spacing: 4) {
            Image(systemName: icon).font(.title2).foregroundStyle(.secondary).padding(.bottom, 2)
            Text(title).font(.callout.weight(.medium))
            if let detail {
                Text(.init(detail)).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
    }
}

/// The last action's outcome.
struct Toast: View {
    let message: String
    let failed: Bool

    var body: some View {
        Label(message, systemImage: failed ? "xmark.octagon.fill" : "checkmark.circle.fill")
            .font(.caption)
            .foregroundStyle(failed ? .red : .secondary)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 14)
            .padding(.bottom, 10)
    }
}

struct Footer: View {
    let model: Model

    var body: some View {
        HStack(spacing: 14) {
            Button("Status in Terminal", systemImage: "terminal") { openWatch() }
            Link(destination: URL(string: "https://github.com/C01-Dev/strata#readme")!) {
                Label("Docs", systemImage: "book")
            }
            Spacer()
            Button("Quit") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        }
        .buttonStyle(.borderless)
        .font(.callout)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    /// Terminal opens `.command` files, so this needs no Automation permission.
    private func openWatch() {
        guard let bin = Strata.binary else { return }
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("strata-status.command")
        let script = "#!/bin/sh\nexec '\(bin.path)' status --watch\n"
        do {
            try script.write(to: file, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: file.path)
            NSWorkspace.shared.open(file)
        } catch {
            model.message = error.localizedDescription
            model.failed = true
        }
    }
}

@MainActor
func confirm(_ title: String, _ text: String, _ action: String) -> Bool {
    let alert = NSAlert()
    alert.messageText = title
    alert.informativeText = text
    alert.alertStyle = .warning
    alert.addButton(withTitle: action).hasDestructiveAction = true
    alert.addButton(withTitle: "Cancel")
    NSApplication.shared.activate()
    return alert.runModal() == .alertFirstButtonReturn
}
