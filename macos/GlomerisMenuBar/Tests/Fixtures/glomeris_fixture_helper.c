/*
 * glomeris_fixture_helper.c
 *
 * Tiny compiled test fixture for GlomerisClientTests: an ordinary Mach-O
 * executable (not a shell script) that prints its second argument to
 * stdout, its third argument to stderr, and exits with the status given
 * by its first argument.
 *
 *   usage: glomeris_fixture_helper <exit-code> [stdout] [stderr]
 *
 * A compiled binary is used instead of a `#!/bin/sh` script so the test
 * suite spawns exactly one fresh executable via `Process.executableURL`,
 * reused (never rewritten) across every fixture-based test — mirroring
 * how GlomerisClient spawns the real `glomeris` binary.
 */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc > 2) {
        fputs(argv[2], stdout);
    }
    if (argc > 3) {
        fputs(argv[3], stderr);
    }
    return argc > 1 ? atoi(argv[1]) : 0;
}
