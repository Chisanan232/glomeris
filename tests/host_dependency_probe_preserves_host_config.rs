//! HORO-1825 §4.3/§8: the executable-dependency probe reads a developer's
//! real hook/daemon/LaunchAgent config read-only. This asserts it
//! empirically — byte-for-byte content AND mtime unchanged on every config
//! file the probe examines — complementing
//! `scripts/check-host-dependency-probe-is-read-only.sh`'s static,
//! shape-based guard.
//!
//! Every fixture here lives under a throwaway temp directory, never the
//! developer's real `$HOME` — see `HostDependencyRoots`'s own doc comment
//! for why reading the real `~/.claude/settings.json` in a test would
//! reproduce the HORO-1822 incident's privacy failure in CI.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use glomeris::evidence::correlate::{
    HostDependencyProbe, HostDependencyRoots, LiveHostDependencyProbe,
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-host-dep-preservation-{prefix}-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct Snapshot {
    bytes: Vec<u8>,
    mtime_secs: i64,
    mtime_nanos: i64,
}

fn snapshot(path: &std::path::Path) -> Snapshot {
    let bytes = fs::read(path).expect("read fixture before probe");
    let meta = fs::metadata(path).expect("stat fixture before probe");
    Snapshot {
        bytes,
        mtime_secs: meta.mtime(),
        mtime_nanos: meta.mtime_nsec(),
    }
}

fn assert_unchanged(path: &std::path::Path, before: &Snapshot) {
    let after = snapshot(path);
    assert_eq!(
        after.bytes,
        before.bytes,
        "probe must never modify the content of {}",
        path.display()
    );
    assert_eq!(
        (after.mtime_secs, after.mtime_nanos),
        (before.mtime_secs, before.mtime_nanos),
        "probe must never touch the mtime of {}",
        path.display()
    );
}

/// Preservation across every recognized JSON config source plus a
/// LaunchAgent plist, probed against a resource that one of them
/// genuinely references — the path most likely to tempt a "helpful"
/// rewrite (e.g. normalizing the plist) is the one that found something.
#[test]
fn probe_preserves_every_recognized_host_config_file_byte_for_byte() {
    let home = unique_temp_dir("home");
    let resource = unique_temp_dir("target");
    let hook_bin = resource.join("debug").join("hook");
    fs::create_dir_all(hook_bin.parent().unwrap()).unwrap();
    fs::write(&hook_bin, b"#!/bin/sh\necho hi\n").unwrap();

    fs::create_dir_all(home.join(".claude")).unwrap();
    let settings_path = home.join(".claude").join("settings.json");
    fs::write(
        &settings_path,
        format!(
            r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
            hook_bin.display()
        ),
    )
    .unwrap();

    let local_path = home.join(".claude").join("settings.local.json");
    fs::write(&local_path, "{}").unwrap();

    fs::create_dir_all(home.join(".codex")).unwrap();
    let codex_hooks_path = home.join(".codex").join("hooks.json");
    fs::write(&codex_hooks_path, "{}").unwrap();
    let codex_toml_path = home.join(".codex").join("config.toml");
    fs::write(&codex_toml_path, "notify = []\n").unwrap();

    fs::create_dir_all(home.join("Library").join("LaunchAgents")).unwrap();
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join("com.example.agent.plist");
    fs::write(
        &plist_path,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.example.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/true</string>
    </array>
</dict>
</plist>
"#,
    )
    .unwrap();

    let all_fixtures = [
        settings_path.clone(),
        local_path.clone(),
        codex_hooks_path.clone(),
        codex_toml_path.clone(),
        plist_path.clone(),
    ];
    let before: Vec<Snapshot> = all_fixtures.iter().map(|p| snapshot(p)).collect();

    let roots = HostDependencyRoots {
        home: home.clone(),
        path_dirs: Vec::new(),
        plutil_bin: PathBuf::from("/usr/bin/plutil"),
        lsof_bin: PathBuf::from("lsof"),
        managed_settings: None,
    };
    let probe = LiveHostDependencyProbe::new(roots);
    let report = probe
        .probe(&resource, Duration::from_secs(5))
        .observed()
        .cloned()
        .expect("probe should observe against a real resource directory");

    // Sanity: the probe actually found the reference it was given, so this
    // test is exercising the "found something" path, not a vacuous no-op.
    assert!(
        !report.references_inside.is_empty(),
        "fixture must exercise a genuine reference, or this test proves nothing"
    );

    for (path, snap) in all_fixtures.iter().zip(before.iter()) {
        assert_unchanged(path, snap);
    }

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&resource).ok();
}

/// Negative control: an absent `plutil` must still leave every file on disk
/// untouched (fail-closed, not fail-destructive).
#[test]
fn absent_plutil_still_preserves_every_fixture_file() {
    let home = unique_temp_dir("home-noplutil");
    let resource = unique_temp_dir("target-noplutil");
    fs::create_dir_all(&resource).unwrap();
    fs::create_dir_all(home.join("Library").join("LaunchAgents")).unwrap();
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join("com.example.other.plist");
    fs::write(&plist_path, b"not a real plist").unwrap();

    let before = snapshot(&plist_path);

    let roots = HostDependencyRoots {
        home: home.clone(),
        path_dirs: Vec::new(),
        plutil_bin: PathBuf::from("/definitely/not/a/real/plutil"),
        lsof_bin: PathBuf::from("lsof"),
        managed_settings: None,
    };
    let probe = LiveHostDependencyProbe::new(roots);
    let _ = probe.probe(&resource, Duration::from_secs(5));

    assert_unchanged(&plist_path, &before);

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&resource).ok();
}
