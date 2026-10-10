//! HORO-1824 end-to-end proof: discovery widening surfaces a shared/
//! Redirected Cargo target as its own resource, and — because the
//! HORO-1825 executable-dependency probe runs for every `Path`-locator
//! resource regardless of how it was discovered — a hook binary
//! installed inside that newly-surfaced shared target is caught as
//! PROTECTED, exactly as it would be for any other resource. This is the
//! guard-before-widening interaction the ticket requires: discovery
//! widening does not need to re-implement the PROTECTED check, but it
//! does need to be PROVEN that surfacing the resource is sufficient for
//! the existing guard to see it at all.
//!
//! Two disposable fixture project roots, each with a project-local
//! `.cargo/config.toml` redirecting `target-dir` to the SAME physical
//! directory outside both workspace roots — the hermetic equivalent of
//! two worktrees sharing `~/.cargo/shared-target` via the real machine's
//! global config (see `~/.claude/CLAUDE.md`'s own documented incident).
//! Never touches the developer's real `$HOME` or real `~/.cargo`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use glomeris::detectors::{DetectorId, DetectorRegistry, DetectorStatus, DiscoveryContext};
use glomeris::evidence::correlate::{
    DefaultEvidenceCollector, EvidenceCollector, GitCliProbe, HostDependencyRoots,
    LiveHostDependencyProbe, LsofOpenFileProbe, LsofProcessCwdProbe, ProbeBudget,
    SystemToolLivenessProbe,
};
use glomeris::evidence::{ResourceKind, ResourceLocator, TargetScope};
use glomeris::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};

fn unique_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-cargo-widen-{prefix}-{}-{}-{n}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_manifest(root: &Path, name: &str) {
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "").unwrap();
}

fn point_at_shared_target(root: &Path, shared: &Path) {
    fs::create_dir_all(root.join(".cargo")).unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        format!(
            "[build]\ntarget-dir = \"{}\"\n",
            shared.to_string_lossy().replace('\\', "\\\\")
        ),
    )
    .unwrap();
}

#[test]
fn a_shared_target_surfaced_by_widening_is_protected_when_a_hook_lives_inside_it() {
    let root_a = unique_dir("root-a");
    let root_b = unique_dir("root-b");
    write_manifest(&root_a, "worker-a");
    write_manifest(&root_b, "worker-b");

    let shared = unique_dir("shared-target");
    point_at_shared_target(&root_a, &shared);
    point_at_shared_target(&root_b, &shared);
    // Discovery only probes a configured root's own `<root>/target` dir
    // if one exists on disk (a leftover/orphan from before the redirect,
    // or simply because something created it) — widening only ever
    // widens FROM an already-discovered root.
    fs::create_dir_all(root_a.join("target")).unwrap();
    fs::create_dir_all(root_b.join("target")).unwrap();

    // Install a "hook binary" inside the shared target, and a fixture
    // `~/.claude/settings.json` that references it — never the
    // developer's real `$HOME`.
    let fixture_home = unique_dir("fixture-home");
    let hook_bin = shared.join("release").join("hook");
    fs::create_dir_all(hook_bin.parent().unwrap()).unwrap();
    fs::write(&hook_bin, b"#!/bin/sh\n").unwrap();
    fs::create_dir_all(fixture_home.join(".claude")).unwrap();
    fs::write(
        fixture_home.join(".claude").join("settings.json"),
        format!(
            r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
            hook_bin.display()
        ),
    )
    .unwrap();

    // Discovery, with BOTH roots configured — this is what triggers
    // dedup: both roots independently resolve to the SAME physical
    // shared directory.
    let ctx = DiscoveryContext::new(&fixture_home)
        .with_known_project_roots(vec![root_a.clone(), root_b.clone()]);
    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);
    let (_, status) = results
        .into_iter()
        .find(|(id, _)| *id == DetectorId("cargo_target_dir"))
        .expect("cargo_target_dir detector must be registered");

    let evidence = match status {
        DetectorStatus::Found(ev) => ev,
        other => panic!("expected Found, got {other:?}"),
    };

    // Three resources: each root's own (now-Orphan) `<root>/target`
    // locator, plus ONE deduplicated widened resource for the shared
    // directory itself.
    let shared_canonical = shared.canonicalize().unwrap();
    let widened = evidence
        .iter()
        .find(|e| matches!(&e.resource.locator, ResourceLocator::Path(p) if p == &shared_canonical))
        .expect("the shared target must be surfaced by discovery widening");

    match &widened.cargo_target_scope {
        Some(outcome) => match outcome.observed() {
            Some(scope) => assert_eq!(
                scope.scope,
                TargetScope::Redirected,
                "the widened shared target is outside both workspace roots"
            ),
            None => panic!("expected an Observed scope on the widened resource"),
        },
        None => panic!("the widened resource must carry a cargo_target_scope"),
    }
    assert_eq!(widened.resource.kind, ResourceKind::CargoTargetDir);

    // Correlate with the REAL executable-dependency probe (HORO-1825),
    // pointed at the fixture home — never the real one — and confirm the
    // widened resource is caught.
    let host_dependency_roots = HostDependencyRoots {
        home: fixture_home.clone(),
        path_dirs: Vec::new(),
        plutil_bin: PathBuf::from("/usr/bin/plutil"),
        lsof_bin: PathBuf::from("lsof"),
        managed_settings: None,
    };
    let collector = DefaultEvidenceCollector::new(
        Box::new(LsofOpenFileProbe),
        Box::new(LsofProcessCwdProbe),
        Box::new(GitCliProbe),
        Box::new(SystemToolLivenessProbe),
        Some(Box::new(LiveHostDependencyProbe::new(
            host_dependency_roots,
        ))),
    );
    let correlation = collector.collect(
        &widened.resource,
        ProbeBudget {
            timeout: Duration::from_secs(5),
        },
    );

    let mut evidence = widened.clone();
    glomeris::evidence::correlate::merge_into(&mut evidence, correlation);

    let decision = classify(&evidence, &PolicyConfig::default(), SystemTime::now());

    assert_eq!(
        decision.class,
        PolicyClass::Protected,
        "a hook binary inside the widened shared target must still be caught, \
         reasons: {:?}",
        decision.reasons
    );
    assert_eq!(
        decision.reasons,
        vec![ReasonCode::ProtectedExecutableDependency],
        "step 2b (installed executable dependency) must fire before step 6's \
         SharedBuildCacheBlastRadius — a stronger, unconditional refusal, matching \
         step 2's own precedent for a stale positive"
    );

    fs::remove_dir_all(&root_a).ok();
    fs::remove_dir_all(&root_b).ok();
    fs::remove_dir_all(&shared).ok();
    fs::remove_dir_all(&fixture_home).ok();
}
