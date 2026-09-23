/*
 * glomeris_fixture_helper.c
 *
 * Tiny compiled test fixture for GlomerisClientTests: an ordinary Mach-O
 * executable (not a shell script) that prints its second argument to
 * stdout, its third argument to stderr, and exits with the status given
 * by its first argument.
 *
 *   usage: glomeris_fixture_helper <exit-code> [stdout] [stderr] [linger-ms]
 *
 * `linger-ms` (HORO-1308) makes the child outlive its own output: both
 * streams are flushed first, then it sleeps that many milliseconds before
 * exiting. That is what lets a cancellation test kill a child which has
 * ALREADY written a complete, valid JSON body — the adversarial case, since
 * returning that body on a cancelled call is exactly the mistake worth
 * pinning against. It also installs no SIGTERM handler, so `terminate()`
 * really does end it.
 *
 * A compiled binary is used instead of a `#!/bin/sh` script so the test
 * suite spawns exactly one fresh executable via `Process.executableURL`,
 * reused (never rewritten) across every fixture-based test — mirroring
 * how GlomerisClient spawns the real `glomeris` binary.
 *
 * HORO-1365 adds an environment override. The argv protocol above only works
 * when the TEST chooses the arguments; driving a view's own fetch function
 * (`runDetect`, `runLlmPlan`) means production code chooses them, so there is
 * no way to hand this fixture a report that way. `GLOMERIS_FIXTURE_STDOUT`
 * and `GLOMERIS_FIXTURE_EXIT` are read first when set, and reach the child
 * through `GlomerisClient(executableURL:environment:)`. Both are ignored when
 * unset, so every argv-driven test above is untouched.
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *env_stdout = getenv("GLOMERIS_FIXTURE_STDOUT");
    const char *env_exit = getenv("GLOMERIS_FIXTURE_EXIT");
    if (env_stdout != NULL) {
        fputs(env_stdout, stdout);
        fflush(stdout);
        return env_exit != NULL ? atoi(env_exit) : 0;
    }

    if (argc > 2) {
        fputs(argv[2], stdout);
    }
    if (argc > 3) {
        fputs(argv[3], stderr);
    }
    if (argc > 4) {
        /* Flushed before sleeping, not at exit: a pipe is block-buffered, so
         * without this the "child already produced its whole body" premise of
         * the cancellation tests would be false. */
        fflush(stdout);
        fflush(stderr);
        long linger_ms = atol(argv[4]);
        if (linger_ms > 0) {
            usleep((useconds_t)linger_ms * 1000);
        }
    }
    return argc > 1 ? atoi(argv[1]) : 0;
}
