import AppKit
import SwiftUI

@main
struct StrataBarApp: App {
    @State private var model = Model()

    var body: some Scene {
        MenuBarExtra {
            Panel(model: model)
                .frame(width: 360)
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
        let view = NSHostingView(rootView: Panel(model: model).frame(width: 360).background(.background))
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

struct MenuLabel: View {
    let model: Model

    var body: some View {
        if model.status == nil {
            Image(systemName: "square.stack.3d.up.slash")
                .accessibilityLabel("Strata is not running")
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
    let model: Model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let status = model.status {
                ForEach(status.repos) { RepoSection(repo: $0, model: model) }
                if status.repos.isEmpty {
                    Text("No repos yet. Run `strata init` in one.")
                        .foregroundStyle(.secondary)
                }
                ForEach(Array(status.checker.enumerated()), id: \.offset) { _, c in
                    Label("Checker: TypeScript \(c.typescript), \(c.agents.count) agents, \(size(c.memoryBytes))", systemImage: "checkmark.seal")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else {
                problem
            }
            if let message = model.message {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(model.failed ? .red : .secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Divider()
            HStack {
                Button("Status in Terminal", systemImage: "terminal") { openWatch() }
                Spacer()
                Button("Quit") { NSApplication.shared.terminate(nil) }
                    .keyboardShortcut("q")
            }
            .buttonStyle(.borderless)
        }
        .padding(12)
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("Strata").font(.headline)
            Spacer()
            if let s = model.status {
                let up = max(0, Int(Date().timeIntervalSince1970) - s.startedAt)
                Text("v\(s.version), up \(up / 3600)h \(up % 3600 / 60)m")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var problem: some View {
        Text(model.problem ?? "Loading…")
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        if Strata.binary == nil {
            Link("How to install Strata", destination: URL(string: "https://github.com/C01-Dev/strata#install")!)
        } else if model.problem != nil {
            Button("Start Strata") { Task { await model.act(["daemon", "start"]) } }
        }
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

struct RepoSection: View {
    let repo: Repo
    let model: Model

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(URL(fileURLWithPath: repo.root).lastPathComponent).font(.subheadline.bold())
                Text(repo.baseBranch).font(.caption).foregroundStyle(.secondary)
            }
            if repo.agents.isEmpty {
                Text("No agents. `strata new <name>` adds one.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ForEach(repo.agents) { AgentRow(agent: $0, root: repo.root, model: model) }
        }
    }
}

struct AgentRow: View {
    let agent: Agent
    let root: String
    let model: Model

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityLabel(agent.state)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 4) {
                    Text(agent.name).bold()
                    if agent.branch != agent.name {
                        Text(agent.branch).foregroundStyle(.secondary)
                    }
                }
                Text(details)
                    .font(.caption)
                    .foregroundStyle(agent.budget == "over" ? .red : .secondary)
            }
            Spacer()
            if let url = agent.url, let link = URL(string: url) {
                Button("Open \(url)", systemImage: "safari") { NSWorkspace.shared.open(link) }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .help(url)
            }
            Menu {
                if agent.devRunning {
                    Button("Stop Dev Server") { Task { await model.act(["stop", agent.name], in: root) } }
                } else {
                    Button("Start Dev Server") { Task { await model.act(["dev", agent.name], in: root) } }
                }
                Divider()
                Button("Open in Terminal") { openInTerminal() }
                Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: agent.path)]) }
                Button("Copy Path") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(agent.path, forType: .string)
                }
                Divider()
                Button("Remove…", role: .destructive) { Task { await remove() } }
            } label: {
                Image(systemName: "ellipsis.circle").accessibilityLabel("Actions for \(agent.name)")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
        }
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
            parts.append(size(agent.memoryBytes) + (agent.budget == "over" ? " (over budget)" : agent.budget == "warning" ? " (near budget)" : ""))
        }
        if let tier = agent.seedTier { parts.append("deps: \(tier)") }
        if let port = agent.port { parts.append("port \(port)") }
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
