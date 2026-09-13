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
    func run<Output: Decodable, Progress: Decodable>(
        _ arguments: [String],
        outputType: Output.Type = Output.self,
        progressType: Progress.Type = Progress.self
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

        // Drain both pipes concurrently with the process running, so a
        // chatty subcommand can never deadlock on a full pipe buffer
        // while we wait for it to exit.
        async let stdoutData = Self.readAll(stdoutPipe.fileHandleForReading)
        async let stderrData = Self.readAll(stderrPipe.fileHandleForReading)
        let (outData, errData) = await (stdoutData, stderrData)

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

        let decoder = JSONDecoder()
        let output: Output
        do {
            output = try decoder.decode(Output.self, from: outData)
        } catch {
            throw GlomerisClientError.outputDecodingFailed(String(describing: error))
        }

        let progressLines = Self.parseNDJSON(errData, as: Progress.self, decoder: decoder)
        return GlomerisClientResult(output: output, progressLines: progressLines)
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
