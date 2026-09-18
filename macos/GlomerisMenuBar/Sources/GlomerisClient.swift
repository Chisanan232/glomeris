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
}

/// Spawns the `glomeris` binary and decodes its output.
struct GlomerisClient {
    let executableURL: URL
    let environment: [String: String]?

    init(executableURL: URL = GlomerisClient.defaultExecutableURL, environment: [String: String]? = nil) {
        self.executableURL = executableURL
        self.environment = environment
    }

    /// TODO(HORO-????): finding the actual embedded/installed `glomeris`
    /// binary (bundled resource vs. Homebrew vs. `PATH` resolution via
    /// `/usr/bin/env glomeris` semantics) is explicit future scope, not
    /// this ticket's. This is a placeholder path only — callers who need
    /// a specific binary should pass `executableURL` explicitly to the
    /// initializer rather than rely on this default.
    static var defaultExecutableURL: URL {
        URL(fileURLWithPath: "/usr/local/bin/glomeris")
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
        let process = Process()
        process.executableURL = executableURL
        process.arguments = arguments
        if let environment {
            process.environment = environment
        }

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        do {
            try process.run()
        } catch {
            throw GlomerisClientError.executionFailed(error.localizedDescription)
        }

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

        process.waitUntilExit()

        switch process.terminationStatus {
        case 0:
            break
        case 1:
            throw GlomerisClientError.executionFailed(Self.trimmedText(errData))
        case 2:
            throw GlomerisClientError.usage(Self.trimmedText(errData))
        case let other:
            throw GlomerisClientError.unexpectedExitCode(other)
        }

        let output: Output
        do {
            output = try decoder.decode(Output.self, from: outData)
        } catch {
            throw GlomerisClientError.outputDecodingFailed(String(describing: error))
        }

        return GlomerisClientResult(output: output, progressLines: liveProgressLines)
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
