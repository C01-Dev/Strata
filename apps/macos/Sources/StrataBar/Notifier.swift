import Foundation
import UserNotifications

/// Tells the user when a pair of agents starts clashing. Asks for permission at the first
/// clash rather than at launch, so the request comes with a reason.
@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    static let shared = Notifier()

    func post(_ c: Conflict) async {
        // `swift run` has no app bundle, and UNUserNotificationCenter traps without one.
        guard Bundle.main.bundleIdentifier != nil else { return }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else { return }
        let content = UNMutableNotificationContent()
        content.title = "\(c.title) clash"
        content.body = c.detail
        content.sound = .default
        // The same id replaces an older notification for this pair.
        try? await center.add(UNNotificationRequest(identifier: "clash-\(c.id)", content: content, trigger: nil))
    }

    /// Shows the banner even while the panel is open, when the app counts as active.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }
}
