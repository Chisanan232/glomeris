//
//  GlomerisClient.swift
//  GlomerisMenuBar
//
//  Generic spawn/decode plumbing over the Rust `glomeris` CLI (HORO-1060).
//  See the standing project rule in GlomerisMenuBarApp.swift: this file is
//  plumbing only — no policy/evidence/action/execution logic. Every
//  subcommand's specific request/response shape is a later ticket's job;
//  this type only knows how to spawn the binary, capture stdout/stderr,
//  decode stdout as a single JSON document, and decode stderr as
//  newline-delimited JSON progress lines.
//
//  Process execution is `Process.executableURL` + `arguments` ONLY — never
//  a shell interpreter, never a string-interpolated command line, never
//  the deprecated launch-path API. GlomerisClientTests mechanically greps
//  this file's source for shell-execution strings to enforce that.
//

import Foundation

/// The decoded result of one `glomeris` invocation: the single JSON
/// document from stdout, plus zero or more progress lines decoded from
/// stderr NDJSON. HORO-1052/A9 (not yet merged on the Rust side) is what
/// will make subcommands actually emit `--progress-json` lines — until
/// then, `progressLines` is simply empty for every invocation.
struct GlomerisClientResult<Output: Decodable, Progress: Decodable> {
    let output: Output
    let progressLines: [Progress]
}

/// Result of invoking `glomeris` with no exit-code interpretation at all
/// — every byte of stdout/stderr and the raw exit code, verbatim.
///
/// `run(_:outputType:progressType:onProgress:)` below assumes exactly one
/// JSON shape appears on stdout only when the process exits `0`, which is
/// true for `detect`/`explain`/`status`/etc. but not for `execute`
/// (HORO-1065): `execute` prints a distinct, still-meaningful JSON body
/// (`ExecuteReport` or `ExecuteRefusalReport`) to stdout on MOST of its
/// non-zero exit codes too (see `book/src/cli_reference.md`'s `execute`
/// section) — throwing that body away, as `run` does, would lose exactly
/// the structured detail a UI needs to render a specific message per
/// outcome. `runRaw` hands back everything uninterpreted so a subcommand-
/// aware caller (the view layer, which already knows its own outcome
/// shapes) can decode and branch on it directly — this type itself
/// performs no decoding and no exit-code-specific judgment, staying pure
/// plumbing.
struct GlomerisRawResult<Progress: Decodable> {
    let exitCode: Int32
    let stdout: Data
    let stderr: Data
    let progressLines: [Progress]
}

/// Typed exit-code errors, matching (not 1:1) the exit-code conventions
/// documented in `book/src/cli_reference.md`'s "Exit codes" section:
/// 0 success, 1 execution/runtime failure, 2 usage error.
enum GlomerisClientError: Error, Equatable {
    /// The process could not be spawned at all (e.g. the binary is
    /// missing or not executable), or it exited `1` — the CLI's
    /// convention for a runtime/execution failure. The associated string
    /// is `Process.run()`'s error description, or the process's captured
    /// stderr, respectively.
    case executionFailed(String)
    /// The CLI exited `2` — its convention for a usage error (unknown
    /// argument, missing flag value, missing required argument, etc.).
    /// The associated string is the process's captured stderr.
    case usage(String)
    /// The CLI exited with some other non-zero code this client doesn't
    /// have a named case for.
    case unexpectedExitCode(Int32)
    /// The process exited `0` but stdout did not decode as the requested
    /// `Output` type.
    case outputDecodingFailed(String)
    /// No `glomeris` binary was found in any of the searched locations, so
    /// nothing was spawned (HORO-1295). Distinct from `executionFailed`
    /// because the remedy is different and knowable: a binary that exists
    /// but fails to launch is a broken install, whereas this is no install
    /// at all, and the associated names tell the user where to put one.
    case executableNotFound(searched: [String])
    /// The calling `Task` was cancelled, so the child was sent `SIGTERM` and
    /// its output discarded (HORO-1308).
    ///
    /// A typed case rather than Swift's own `CancellationError` on purpose:
    /// every popover section already funnels its failures through
    /// `SectionFetchErrors.shortMessage`, which knows how to turn a
    /// `GlomerisClientError` into one sentence of user-facing copy and would
    /// otherwise render this one as `String(describing:)` boilerplate. Being
    /// in the same enum is also what lets that funnel return *no message at
    /// all* for a cancellation — a scan the user stopped, or one abandoned
    /// because they closed the popover, is not a failure to report.
    case cancelled
}

/// One-shot carrier for a child process's exit status, handing it from
/// Foundation's `terminationHandler` callback to an awaiting `async` caller
/// without either side blocking a thread (HORO-1304).
///
/// This exists because the two events can happen in either order. The
/// handler must be installed *before* `Process.run()`, since a handler
/// attached after the child has already exited is not guaranteed to run — but
/// the continuation to resume only exists later, once someone awaits. So the
/// status may arrive before there is anybody to give it to, or somebody may
/// be waiting before it arrives. Storing the status and the waiters under one
/// lock makes both orderings correct, and means the fast-child case (the
/// common one: `glomeris status --json` exits in milliseconds) needs no
/// suspension at all.
///
/// Internal rather than private only so its ordering invariants can be
/// asserted on directly from the test target; it is an implementation detail
/// of `GlomerisClient.runRaw` and nothing else should use it.
final class ExitStatusRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var status: Int32?
    private var waiters: [CheckedContinuation<Int32, Never>] = []

    /// Records the child's exit status and wakes anyone waiting. Extra calls
    /// are ignored: resuming a `CheckedContinuation` twice traps, so a
    /// duplicate termination callback must degrade to "first status wins"
    /// rather than crash the app.
    func complete(_ status: Int32) {
        lock.lock()
        guard self.status == nil else {
            lock.unlock()
            return
        }
        self.status = status
        let waiting = waiters
        waiters.removeAll()
        lock.unlock()

        // Resumed outside the lock: a continuation can run its caller
        // synchronously, and that caller must never re-enter this lock.
        for waiter in waiting {
            waiter.resume(returning: status)
        }
    }

    /// The child's exit status, suspending only if it has not arrived yet.
    /// The status is retained rather than consumed, so this is safe to call
    /// more than once.
    func wait() async -> Int32 {
        await withCheckedContinuation { continuation in
            lock.lock()
            if let status {
                lock.unlock()
                continuation.resume(returning: status)
                return
            }
            waiters.append(continuation)
            lock.unlock()
        }
    }
}

/// Holds the spawned child so a cancellation arriving from another thread can
/// signal it, without either side having to know which happened first
/// (HORO-1308).
///
/// Two orderings have to be correct. Ordinarily the child is adopted first and
/// a later cancellation signals it. But `withTaskCancellationHandler` invokes
/// its handler immediately if the task is *already* cancelled, which can
/// happen while `Process.run()` is still in progress — and a terminate request
/// that arrived then must not be dropped on the floor, or the popover closing
/// mid-scan would leave an orphaned `glomeris` running to completion. So a
/// request that beats the child is remembered and applied at adoption.
///
/// `Process.terminate()` raises an Objective-C exception — uncatchable from
/// Swift, so it would take the app down — if the process was never launched.
/// Holding the reference only from the moment `run()` has returned
/// successfully is what makes that unreachable; there is no launched check to
/// get wrong because an unlaunched process is never visible here.
///
/// Internal rather than private only so the orderings can be asserted on
/// directly from the test target, same as `ExitStatusRelay`.
final class ProcessTerminationGate: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var terminationRequested = false

    /// Call once, immediately after `Process.run()` returns without throwing.
    func adopt(_ process: Process) {
        lock.lock()
        self.process = process
        let signalNow = terminationRequested
        lock.unlock()

        // Outside the lock: `terminate()` is a Foundation call and must not
        // run with our lock held.
        if signalNow {
            process.terminate()
        }
    }

    /// Safe from any thread, at any time, any number of times — including
    /// before `adopt` and after the child has already exited.
    func requestTermination() {
        lock.lock()
        terminationRequested = true
        let target = process
        lock.unlock()

        target?.terminate()
    }

    /// Whether a termination was ever asked for. Read after the fact to tell
    /// "the child exited on its own" from "we killed it".
    var wasTerminationRequested: Bool {
        lock.lock()
        defer { lock.unlock() }
        return terminationRequested
    }
}

/// Spawns the `glomeris` binary and decodes its output.
struct GlomerisClient {
    /// A caller-chosen binary that bypasses resolution entirely, or `nil` to
    /// resolve through `locator` on every invocation.
    private let pinnedExecutableURL: URL?
    private let locator: GlomerisExecutableLocator
    let environment: [String: String]?

    /// Always spawns exactly `executableURL`, never resolving anything. The
    /// tests use this to pin a fixture binary, and a pin is honoured even
    /// when it does not exist — so a test pinning a missing path still gets
    /// the spawn failure it is asserting on, rather than silently finding
    /// whatever `glomeris` the host happens to have installed.
    init(executableURL: URL, environment: [String: String]? = nil) {
        pinnedExecutableURL = executableURL
        locator = GlomerisExecutableLocator()
        self.environment = environment
    }

    /// Resolves the binary through `locator` (HORO-1295). This is what the
    /// views use, and replaces the previous hardcoded
    /// `/usr/local/bin/glomeris` default that made the app unusable after a
    /// documented `brew install` on Apple Silicon.
    init(
        environment: [String: String]? = nil,
        locator: GlomerisExecutableLocator = GlomerisExecutableLocator()
    ) {
        pinnedExecutableURL = nil
        self.locator = locator
        self.environment = environment
    }

    /// The binary to spawn for one invocation.
    ///
    /// Resolved per invocation rather than once when the client is built, so
    /// installing the CLI while the app is already running takes effect at
    /// the next poll (10s) instead of requiring a restart — the app is an
    /// `LSUIElement` that a user leaves running for days, and "quit and
    /// relaunch it" is not a remedy anyone should have to be told.
    private func resolveExecutableURL() throws -> URL {
        if let pinnedExecutableURL {
            return pinnedExecutableURL
        }
        guard let located = locator.locate() else {
            throw GlomerisClientError.executableNotFound(searched: locator.searchedLocations)
        }
        return located.url
    }

    /// Runs `glomeris` with `arguments`, decodes stdout as `Output`, and
    /// decodes stderr as zero or more NDJSON `Progress` lines.
    ///
    /// When `onProgress` is `nil` (the default, and every call site prior
    /// to HORO-1063), stderr is drained in bulk after the process exits,
    /// same as always. When `onProgress` is supplied, stderr is instead
    /// read incrementally while the process is still running, and
    /// `onProgress` is invoked once per decoded NDJSON line as it arrives
    /// — this is what lets a caller (HORO-1063's Refresh flow) show live
    /// per-detector progress instead of a single "done" update after the
    /// whole scan finishes. Either way, `GlomerisClientResult.progressLines`
    /// ends up holding every decoded line, so existing callers that never
    /// pass `onProgress` see no behavior change.
    func run<Output: Decodable, Progress: Decodable>(
        _ arguments: [String],
        outputType: Output.Type = Output.self,
        progressType: Progress.Type = Progress.self,
        onProgress: (@Sendable (Progress) -> Void)? = nil
    ) async throws -> GlomerisClientResult<Output, Progress> {
        let raw = try await runRaw(arguments, progressType: Progress.self, onProgress: onProgress)

        switch raw.exitCode {
        case 0:
            break
        case 1:
            throw GlomerisClientError.executionFailed(Self.trimmedText(raw.stderr))
        case 2:
            throw GlomerisClientError.usage(Self.trimmedText(raw.stderr))
        case let other:
            throw GlomerisClientError.unexpectedExitCode(other)
        }

        let output: Output
        do {
            output = try JSONDecoder().decode(Output.self, from: raw.stdout)
        } catch {
            throw GlomerisClientError.outputDecodingFailed(String(describing: error))
        }

        return GlomerisClientResult(output: output, progressLines: raw.progressLines)
    }

    /// Runs `glomeris` and hands back its raw exit code, stdout, stderr,
    /// and decoded progress lines with no exit-code-specific
    /// interpretation — see `GlomerisRawResult`'s doc comment for why
    /// this exists alongside `run` above. Throws if no binary was found
    /// to spawn, if the one found could not be spawned at all, or if the
    /// calling task was cancelled; every other outcome, including any
    /// non-zero exit code, is returned rather than thrown.
    ///
    /// ## Cancellation (HORO-1308)
    ///
    /// Cancelling the calling task sends the child `SIGTERM` and throws
    /// `GlomerisClientError.cancelled`. This is what lets a user stop an
    /// `llm-plan` run they started — a call that talks to a remote provider
    /// and can sit there for tens of seconds with nothing to show — instead of
    /// being stuck watching a spinner they cannot dismiss. It also stops the
    /// existing polling sections leaving an orphaned child behind every time
    /// the popover closes mid-fetch.
    ///
    /// **Only ever offer cancellation for a read-only or advisory subcommand.**
    /// `status`, `daemon status`, `detect`, `explain`, `history`,
    /// `action-history` and `llm-plan` observe and advise; interrupting one
    /// loses nothing but the answer. `execute` is the exception and must never
    /// be reachable from a cancellable task: `SIGTERM` partway through a
    /// deletion would leave the filesystem in a state neither this app nor the
    /// audit log could describe, and the result would be reported as
    /// "cancelled" whether or not the removal had already happened. That is
    /// why `CandidateDetailView` runs `performClean()` only from button
    /// actions in detached `Task {}` blocks and never from a `.task {}`
    /// modifier, which SwiftUI cancels on disappear —
    /// `GlomerisClientTests.testNoCancellableTaskModifierCanReachExecute`
    /// pins that mechanically.
    func runRaw<Progress: Decodable>(
        _ arguments: [String],
        progressType: Progress.Type = Progress.self,
        onProgress: (@Sendable (Progress) -> Void)? = nil
    ) async throws -> GlomerisRawResult<Progress> {
        let terminationGate = ProcessTerminationGate()
        return try await withTaskCancellationHandler {
            try await spawnAndDrain(
                arguments,
                progressType: Progress.self,
                onProgress: onProgress,
                terminationGate: terminationGate
            )
        } onCancel: {
            // Runs on whichever thread cancelled us, possibly before the child
            // has even been adopted — see `ProcessTerminationGate`.
            terminationGate.requestTermination()
        }
    }

    /// `runRaw`'s body, split out only so the cancellation handler above wraps
    /// one expression instead of forty lines.
    private func spawnAndDrain<Progress: Decodable>(
        _ arguments: [String],
        progressType: Progress.Type,
        onProgress: (@Sendable (Progress) -> Void)?,
        terminationGate: ProcessTerminationGate
    ) async throws -> GlomerisRawResult<Progress> {
        let process = Process()
        process.executableURL = try resolveExecutableURL()
        process.arguments = arguments
        if let environment {
            process.environment = environment
        }

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        // Installed before `run()`, which is mandatory: Foundation will not
        // call a handler attached to an already-terminated process, and these
        // children routinely finish in milliseconds. `ExitStatusRelay`
        // absorbs the resulting ordering ambiguity (HORO-1304).
        let exitStatus = ExitStatusRelay()
        process.terminationHandler = { finished in
            exitStatus.complete(finished.terminationStatus)
        }

        do {
            try process.run()
        } catch {
            throw GlomerisClientError.executionFailed(error.localizedDescription)
        }
        // Only now, and only on the success path: the gate must never hold a
        // process that was not launched.
        terminationGate.adopt(process)

        let decoder = JSONDecoder()

        // Drain both pipes concurrently with the process running, so a
        // chatty subcommand can never deadlock on a full pipe buffer
        // while we wait for it to exit.
        async let stdoutData = Self.readAll(stdoutPipe.fileHandleForReading)
        async let stderrOutcome: (Data, [Progress]) = {
            if let onProgress {
                return await Self.readAllWithLiveProgress(
                    stderrPipe.fileHandleForReading,
                    progressType: Progress.self,
                    decoder: decoder,
                    onProgress: onProgress
                )
            } else {
                let data = await Self.readAll(stderrPipe.fileHandleForReading)
                return (data, Self.parseNDJSON(data, as: Progress.self, decoder: decoder))
            }
        }()
        let (outData, (errData, liveProgressLines)) = await (stdoutData, stderrOutcome)

        // Never `process.waitUntilExit()`. That call blocks the calling
        // thread, and the calling thread here belongs to the Swift
        // concurrency cooperative pool — where it deadlocked outright, long
        // after the child had exited and been reaped. Measured on the host
        // this was fixed on, that was about one run in six of
        // `GlomerisClientTests`, i.e. of roughly 27 invocations. A popover
        // fetch that lost the race never returned, so the section sat on
        // "loading…" forever with no error and no timeout (HORO-1304).
        // Awaiting the relay suspends instead of blocking, so the thread
        // stays available and the wait always ends.
        let exitCode = await exitStatus.wait()

        // A terminated child's exit code is the signal that killed it and its
        // stdout is whatever happened to have been flushed — a truncated JSON
        // document at best. Throwing rather than returning that keeps a partial
        // body from ever reaching a decoder.
        //
        // Gated on the gate's own flag rather than `Task.isCancelled`, which
        // here would mean the same thing, because the flag is the thing the
        // tests can set and observe directly without needing a cancelled task.
        if terminationGate.wasTerminationRequested {
            throw GlomerisClientError.cancelled
        }

        return GlomerisRawResult(
            exitCode: exitCode,
            stdout: outData,
            stderr: errData,
            progressLines: liveProgressLines
        )
    }

    private static func readAll(_ handle: FileHandle) async -> Data {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: handle.readDataToEndOfFile())
            }
        }
    }

    private static func trimmedText(_ data: Data) -> String {
        (String(data: data, encoding: .utf8) ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Reads stderr byte-by-byte while the process is still running,
    /// decoding and emitting each complete NDJSON line as soon as its
    /// trailing newline arrives, instead of waiting for the pipe to close.
    /// A line that's empty or doesn't decode as `Progress` is skipped, same
    /// tolerance as `parseNDJSON` below. Returns the full raw bytes read
    /// (needed for the non-zero-exit-code error paths above) plus every
    /// decoded line, in arrival order.
    private static func readAllWithLiveProgress<Progress: Decodable>(
        _ handle: FileHandle,
        progressType: Progress.Type,
        decoder: JSONDecoder,
        onProgress: @Sendable (Progress) -> Void
    ) async -> (Data, [Progress]) {
        var fullData = Data()
        var lineBuffer = Data()
        var progressLines: [Progress] = []

        do {
            for try await byte in handle.bytes {
                fullData.append(byte)
                if byte == UInt8(ascii: "\n") {
                    if !lineBuffer.isEmpty, let event = try? decoder.decode(Progress.self, from: lineBuffer) {
                        progressLines.append(event)
                        onProgress(event)
                    }
                    lineBuffer.removeAll(keepingCapacity: true)
                } else {
                    lineBuffer.append(byte)
                }
            }
        } catch {
            // Reading stderr failed partway through (e.g. the handle was
            // closed unexpectedly). Fall through and decode whatever was
            // captured so far, same tolerance as a clean EOF.
        }
        // A final line with no trailing newline (or the whole stream, if
        // it never contained one) is still decoded rather than dropped.
        if !lineBuffer.isEmpty, let event = try? decoder.decode(Progress.self, from: lineBuffer) {
            progressLines.append(event)
            onProgress(event)
        }

        return (fullData, progressLines)
    }

    /// Parses stderr as newline-delimited JSON progress lines. A line
    /// that's empty or doesn't decode as `Progress` is skipped rather
    /// than treated as an error: without HORO-1052/A9's
    /// `--progress-json` support, stderr today is plain diagnostic text
    /// (or nothing at all), and that must keep working unchanged once
    /// progress lines actually start appearing.
    private static func parseNDJSON<Progress: Decodable>(
        _ data: Data,
        as type: Progress.Type,
        decoder: JSONDecoder
    ) -> [Progress] {
        guard let text = String(data: data, encoding: .utf8) else { return [] }
        return text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line in
                line.data(using: .utf8).flatMap { try? decoder.decode(Progress.self, from: $0) }
            }
    }
}
