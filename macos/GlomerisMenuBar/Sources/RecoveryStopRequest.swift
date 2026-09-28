//
//  RecoveryStopRequest.swift
//  GlomerisMenuBar
//
//  HORO-1509: "Stop after current action", which is a file rather than a signal.
//
//  ---------------------------------------------------------------------
//  Why a sentinel file and not a `SIGTERM`
//  ---------------------------------------------------------------------
//  `GlomerisClient.runRaw`'s header states the rule this type exists to keep: a
//  `SIGTERM` partway through a deletion leaves the filesystem in a state neither
//  this app nor the audit log could describe, and the result would be reported as
//  "cancelled" whether or not the removal had already happened. A recovery run is
//  the longest-running child the app starts, so it is exactly the surface on
//  which a Stop button looks reasonable — and killing it is exactly what must not
//  happen.
//
//  So the stop is cooperative and the loop owns it. This app creates a file; the
//  Rust loop checks for it between actions (`StopFile` in
//  `src/executor/recovery_loop.rs`) and finishes the action it is on before
//  stopping, then reports `stop_reason: "stopped_by_user"` like any other
//  termination. Nothing is interrupted, and no run handle has to exist for the
//  button to work — which is why `RecoveryState` still deliberately keeps none.
//
//  ---------------------------------------------------------------------
//  Why the path is unique per run
//  ---------------------------------------------------------------------
//  `free` refuses a `--stop-file` that already exists, before taking the
//  execution lock. That refusal is right: a sentinel left behind by an earlier
//  run would stop this one before it did anything and the report would
//  truthfully say the user stopped it, while the user had done nothing at all.
//  A fixed path would hit that refusal after any run the app failed to clean up
//  after — a crash, a force-quit — and recovery would then be broken until
//  somebody found the file. A fresh name per run cannot collide with a corpse.
//

import Foundation

/// The sentinel file one recovery run watches, and the two operations the card
/// performs on it.
///
/// A value type over a URL: it holds no state about whether the stop was
/// requested, because the filesystem holds that and the loop reads it there.
struct RecoveryStopRequest: Equatable {
    /// The path handed to `free --stop-file`, and the path
    /// ``requestStop(using:)`` creates.
    let url: URL

    /// A path for a run that is about to start.
    ///
    /// `id` and `directory` are injectable so a test can assert the shape of the
    /// name and work somewhere disposable, not because production has a second
    /// caller.
    static func forNewRun(
        id: UUID = UUID(),
        directory: URL = FileManager.default.temporaryDirectory
    ) -> RecoveryStopRequest {
        RecoveryStopRequest(
            url: directory.appendingPathComponent("glomeris-recovery-stop-\(id.uuidString).stop")
        )
    }

    /// Creates the sentinel, returning `nil` on success or one sentence
    /// describing why it could not be created.
    ///
    /// Writing an empty file rather than a marker byte: the loop tests for
    /// existence and reads nothing, and a payload would be a second thing that
    /// could be wrong about a request whose entire content is "yes".
    ///
    /// An already-created sentinel is success, not an error. The button can be
    /// pressed twice, and the second press is asking for something that is
    /// already true.
    func requestStop(using fileManager: FileManager = .default) -> String? {
        if fileManager.fileExists(atPath: url.path) { return nil }
        do {
            try Data().write(to: url)
            return nil
        } catch {
            // The path is deliberately not in the message. It is a temporary
            // directory under the user's home, it tells the reader nothing they
            // can act on, and this string is shown in the panel.
            return "Could not ask the run to stop: \(error.localizedDescription)"
        }
    }

    /// Removes the sentinel if it is there.
    ///
    /// Called when a run ends, however it ended, so the next run starts from a
    /// directory with no corpse in it. Failure is deliberately unreported: the
    /// next run generates a fresh name and cannot be blocked by a leftover, so
    /// the only cost of a failed cleanup is a zero-byte file in a temporary
    /// directory the system already reaps.
    func clear(using fileManager: FileManager = .default) {
        try? fileManager.removeItem(at: url)
    }
}
