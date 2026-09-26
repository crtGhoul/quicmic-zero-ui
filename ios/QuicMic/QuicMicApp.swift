import SwiftUI

// QuicMic native iOS client: turns the iPhone into a low-latency wireless
// microphone (and PC-speaker monitor) for the QuicMic server over LAN.
// Entry point; all state lives in QuicMicController.
@main
struct QuicMicApp: App {
    @StateObject private var controller = QuicMicController()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(controller)
        }
    }
}
