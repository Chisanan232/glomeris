#!/usr/bin/env bash
#
# check-single-instance-lifecycle.sh — HORO-1453
#
# Proves the invariant the single-instance rule exists for, against a real built
# app on a real Mac: after any launch, by any route, from any bundle path, at any
# version, exactly ONE process of the app under test is running — and it is the
# one that was launched last.
#
# This is deliberately NOT a CI check. It launches and quits GUI applications and
# reads the accessibility tree, neither of which a headless runner can do. It is
# the deterministic runbook check for this ticket's acceptance criteria, meant to
# be run locally against a Release build before a DogFood install.
# SingleInstanceGuardTests.swift is the part that runs in CI: it asserts the same
# convergence property against the pure rule.
#
# Usage:
#   scripts/check-single-instance-lifecycle.sh <path-to-.app> [iterations]
#
# Build the app under test with its own bundle identifier so the run cannot
# disturb an unrelated Glomeris you are already using:
#
#   xcodebuild -project macos/GlomerisMenuBar/GlomerisMenuBar.xcodeproj \
#     -scheme GlomerisMenuBar -configuration Release \
#     -derivedDataPath /tmp/lifecycle-dd \
#     PRODUCT_BUNDLE_IDENTIFIER=dev.glomeris.GlomerisMenuBarLifecycleRig \
#     CODE_SIGNING_ALLOWED=NO build
#
# Safety. Every process this script ends is proven first to (a) carry the bundle
# identifier of the app under test and (b) live inside the disposable rig
# directory this script created. It refuses to start if any process with that
# identifier is running from anywhere else, because superseding it is the app's
# decision to make in front of a user, not this script's. Nothing outside the rig
# directory is written to or removed, and no privilege is required.
#
set -euo pipefail

APP_UNDER_TEST=${1:-}
ITERATIONS=${2:-5}

if [[ -z $APP_UNDER_TEST || ! -d $APP_UNDER_TEST ]]; then
    echo "usage: $0 <path-to-.app> [iterations]" >&2
    exit 64
fi

PLIST="$APP_UNDER_TEST/Contents/Info.plist"
BUNDLE_ID=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")
EXECUTABLE=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")
BASE_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$PLIST")

# Resolved to its physical path on purpose. TMPDIR sits under /var, which is a
# symlink to /private/var, and a launched process reports the physical path — so a
# rig recorded as /var/... would match none of its own processes, and the check
# would report zero instances while several were running.
RIG=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/glomeris-lifecycle.XXXXXX")" && pwd -P)
cleanup() {
    for pid in $(rig_pids); do kill "$pid" 2>/dev/null || true; done
    sleep 1
    for pid in $(rig_pids); do kill -9 "$pid" 2>/dev/null || true; done
    rm -rf "$RIG"
}

# Every process running this executable name, as "pid<TAB>executable path". Read
# from ps rather than from NSRunningApplication so the script does not depend on
# the very framework behaviour it is checking.
processes_by_name() {
    ps -Ao pid=,comm= | awk -v exe="/Contents/MacOS/$EXECUTABLE" '
        index($0, exe) { pid = $1; sub(/^[ ]*[0-9]+[ ]+/, ""); print pid "\t" $0 }
    '
}

identifier_of() {
    local bundle=${1%"/Contents/MacOS/$EXECUTABLE"}
    # The existence test is not redundant: given a missing file PlistBuddy prints
    # "File Doesn't Exist, Will Create:" on stdout and exits successfully, which
    # would otherwise be read back as the identifier. A bundle can genuinely be
    # gone while its process is still running.
    if [[ ! -f "$bundle/Contents/Info.plist" ]]; then
        echo "(bundle no longer on disk)"
        return 0
    fi
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
        "$bundle/Contents/Info.plist" 2>/dev/null || echo "(unreadable)"
}

# Instances of the app UNDER TEST — same bundle identifier, which is what the
# rule itself keys on. Filtering by executable name alone would fold in every
# other build of this app on the machine, including ones deliberately given a
# different identifier so that they are none of this run's business.
all_instances() {
    local pid exe
    while IFS=$'\t' read -r pid exe; do
        [[ -n $pid ]] || continue
        if [[ $(identifier_of "$exe") == "$BUNDLE_ID" ]]; then
            printf '%s\t%s\n' "$pid" "$exe"
        fi
    done < <(processes_by_name)
    # Explicit, because finding nothing is a normal answer and `set -o pipefail`
    # would otherwise carry a falsy last test out of here as a failure.
    return 0
}

# Same executable, different identifier: reported as context so the operator can
# see what else is on the machine, never touched and never counted.
other_builds() {
    local pid exe id
    while IFS=$'\t' read -r pid exe; do
        [[ -n $pid ]] || continue
        id=$(identifier_of "$exe")
        if [[ $id != "$BUNDLE_ID" ]]; then
            printf '  %s\t%s\t%s\n' "$pid" "$id" "$exe"
        fi
    done < <(processes_by_name)
    return 0
}

# The subset living inside this run's disposable rig. Only these may be signalled.
rig_pids() {
    all_instances | awk -v rig="$RIG/" 'index($2, rig) == 1 { print $1 }'
    return 0
}

rig_count() { rig_pids | wc -l | tr -d ' '; }

# The bundle each rig process was launched from, and the version of that bundle.
survivor_bundle() {
    local exe
    exe=$(all_instances | awk -v rig="$RIG/" 'index($2, rig) == 1 { print $2; exit }')
    [[ -n $exe ]] || return 1
    # $EXECUTABLE quoted separately so it is stripped as a literal. Unquoted it
    # is a glob pattern, and CFBundleExecutable is read from a plist rather than
    # written here, so a `?` or `*` in it would silently strip the tail of a
    # bundle whose executable is a different name.
    printf '%s' "${exe%/Contents/MacOS/"$EXECUTABLE"}"
}

survivor_version() {
    local bundle
    bundle=$(survivor_bundle) || return 1
    /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$bundle/Contents/Info.plist"
}

# Menu-bar items the rig's processes own, read from the accessibility tree, and
# counted per process identifier rather than per process name — an unrelated
# Glomeris build carries the same process name, and counting by name would fold
# its item into this number.
#
# The wait is not padding. A status item appears in the accessibility tree several
# seconds AFTER its process starts: measured on this app, `menu bar 2` did not
# exist 5s in and did exist a little later. Sampling once immediately reports zero
# menu bars for a perfectly healthy instance, which reads as "the icon is missing"
# and is simply wrong.
#
# Prints the total, or "unavailable: <reason>" when the accessibility tree cannot
# be read at all — which is an environment fact about permissions, not a defect,
# and is reported rather than asserted.
menu_bar_items() {
    local total=0 pid count waited
    for pid in $(rig_pids); do
        waited=0
        while true; do
            if count=$(osascript -e "tell application \"System Events\" to count of menu bar items of menu bar 2 of (first process whose unix id is $pid)" 2>&1); then
                break
            fi
            case $count in
                *"assistive access"*|*"-1743"*)
                    echo "unavailable: this process has no Accessibility permission"
                    return 0
                    ;;
            esac
            if (( waited >= 40 )); then
                echo "unavailable: $(printf '%s' "$count" | tr '\n' ' ')"
                return 0
            fi
            sleep 0.5
            waited=$(( waited + 1 ))
        done
        total=$(( total + count ))
    done
    echo "$total"
}

fail() { echo "FAIL: $*" >&2; exit 1; }

# The invariant is convergence, so this waits for it rather than sampling once:
# a superseded instance is asked to quit and takes a moment to go. The wait is
# bounded well inside the guard's own grace period plus its forced escalation, so
# an instance that never goes still fails the check.
expect_count() {
    local want=$1 label=$2 got waited=0
    got=$(rig_count)
    while [[ $got != "$want" && $waited -lt 120 ]]; do
        sleep 0.25
        waited=$(( waited + 1 ))
        got=$(rig_count)
    done
    if [[ $got != "$want" ]]; then
        echo "  processes now:" >&2
        all_instances >&2
        fail "$label: expected $want process(es) of $BUNDLE_ID in the rig, found $got"
    fi
    # Converged is not enough; it has to stay converged. Without this hold, a
    # count that is briefly right before a duplicate finishes launching would
    # satisfy the poll above and the check would pass vacuously.
    sleep 1
    local held
    held=$(rig_count)
    [[ $held == "$want" ]] ||
        fail "$label: converged on $want and then moved to $held a second later"
    printf '    ok: %s process(es), still %s a second later — %s\n' "$got" "$held" "$label"
}

expect_survivor() {
    local want_bundle=$1 want_version=$2 got_bundle got_version
    got_bundle=$(survivor_bundle) || fail "no surviving instance to identify"
    got_version=$(survivor_version)
    [[ $got_bundle == "$want_bundle" ]] ||
        fail "the survivor is $got_bundle, expected $want_bundle"
    [[ $got_version == "$want_version" ]] ||
        fail "the survivor reports version $got_version, expected $want_version"
    echo "    ok: the survivor is the expected bundle, version $got_version"
}

settle() { sleep 2; }

# --- Preconditions -----------------------------------------------------------

echo "app under test : $APP_UNDER_TEST"
echo "bundle id      : $BUNDLE_ID"
echo "version        : $BASE_VERSION"
echo "rig            : $RIG"
echo

others=$(other_builds)
if [[ -n $others ]]; then
    echo "other builds of this app are running; they carry a different bundle"
    echo "identifier, so they are outside this run and are left alone:"
    echo "$others"
    echo
fi

foreign=$(all_instances | awk -v rig="$RIG/" 'index($2, rig) != 1 { print "  " $1 "\t" $2 }')
if [[ -n $foreign ]]; then
    rm -rf "$RIG"
    echo "REFUSING TO RUN: an instance of $BUNDLE_ID is already running from outside the rig:" >&2
    echo "$foreign" >&2
    echo >&2
    echo "Quit it yourself first, or build the app under test with its own" >&2
    echo "PRODUCT_BUNDLE_IDENTIFIER (see the header of this script). This script does" >&2
    echo "not terminate instances it did not launch." >&2
    exit 3
fi
trap cleanup EXIT

expect_count 0 "nothing of this bundle identifier is running to begin with"
echo

# --- The cycle ---------------------------------------------------------------

for (( i = 1; i <= ITERATIONS; i++ )); do
    echo "iteration $i of $ITERATIONS"

    # An install to a path that did not exist before — the DogFood pattern that
    # produced the reported bug, where each cycle lands beside the last one
    # rather than over it. Later iterations also carry a different version, which
    # is the in-place-update case.
    install="$RIG/install-$i/GlomerisMenuBar.app"
    mkdir -p "$(dirname "$install")"
    cp -R "$APP_UNDER_TEST" "$install"
    version="$BASE_VERSION+lifecycle.$i"
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
        "$install/Contents/Info.plist"

    # Route 1: the ordinary launch.
    open "$install"
    settle
    expect_count 1 "a fresh install from a new path launches once"
    expect_survivor "$install" "$version"

    # Route 2: LaunchServices activates what is already running, so this must add
    # nothing even without the guard. Checked so a regression here is visible too.
    open "$install"
    settle
    expect_count 1 "re-opening the same bundle does not add a process"

    # Route 3: an explicitly new instance of the same bundle. Before the fix this
    # left two processes and two icons.
    open -n "$install"
    settle
    expect_count 1 "open -n yields to the instance that already owns the menu bar"
    expect_survivor "$install" "$version"

    # Route 4: the executable invoked directly, which never consults
    # LaunchServices at all.
    "$install/Contents/MacOS/$EXECUTABLE" >/dev/null 2>&1 &
    settle
    expect_count 1 "a directly invoked instance yields as well"
    expect_survivor "$install" "$version"

    items=$(menu_bar_items)
    echo "    menu-bar items (accessibility tree): $items"
    case $items in
        unavailable:*) : ;;
        1) : ;;
        *) fail "one surviving process must own exactly one menu-bar item, not $items" ;;
    esac
    echo
done

# --- Quit, and come back -----------------------------------------------------

echo "quit and relaunch"
survivor=$(survivor_bundle)
survivor_version_now=$(survivor_version)
for pid in $(rig_pids); do kill "$pid"; done
sleep 2
expect_count 0 "quitting leaves nothing behind"

open "$survivor"
settle
expect_count 1 "relaunching after a quit gives exactly one instance again"
expect_survivor "$survivor" "$survivor_version_now"
echo

echo "PASS: $ITERATIONS install/update/relaunch cycles over 4 launch routes each," \
    "one instance throughout."
