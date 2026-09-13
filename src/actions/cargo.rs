//! `cargo.clean.target_dir`: run `cargo clean` against the manifest
//! sitting next to a detected `target/` directory.
//!
//! Deliberately passes an explicit `--target-dir` pointing at the exact
//! resource path being cleaned, rather than relying on `cargo clean`'s
//! ambient config resolution (`~/.cargo/config.toml`'s `[build]
//! target-dir`, a workspace-level override, or `CARGO_TARGET_DIR`). An
//! action that knows precisely which directory it intends to delete must
//! not let ambient environment/config decide that for it — deferring to
//! ambient config here could clean an entirely different (possibly
//! shared, possibly much larger) target directory than the one the
//! evidence and policy layers actually reasoned about.

use std::path::{Path, PathBuf};

use crate::evidence::model::{Evidence, EvidenceField, Recoverability, ResourceKind};
use crate::evidence::model::{ResourceId, ResourceLocator};

use super::{Action, ActionError, ActionId, ActionPlan, ActionStep, ToolBinary};

pub const ID: ActionId = ActionId("cargo.clean.target_dir");

pub struct CargoCleanTargetDir;

impl Action for CargoCleanTargetDir {
    fn id(&self) -> ActionId {
        ID
    }

    fn applies_to(&self) -> &'static [ResourceKind] {
        &[ResourceKind::CargoTargetDir]
    }

    fn required_evidence(&self) -> &'static [EvidenceField] {
        &[EvidenceField::ReclaimableBytes]
    }

    fn recoverability(&self) -> Recoverability {
        Recoverability::RegenerableByRebuild
    }

    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
        if ev.resource.kind != ResourceKind::CargoTargetDir {
            return Err(ActionError::ResourceMismatch);
        }

        let target_dir = target_dir_path(&ev.resource)?;
        let manifest_path = manifest_path_for(&ev.resource, target_dir)?;

        let steps = vec![ActionStep::RunTool {
            tool: ToolBinary::Cargo,
            args: vec![
                "clean".to_string(),
                "--manifest-path".to_string(),
                manifest_path.to_string_lossy().into_owned(),
                "--target-dir".to_string(),
                target_dir.to_string_lossy().into_owned(),
            ],
            scoped_path: Some(target_dir.clone()),
        }];

        let explain = format!(
            "Run `cargo clean --manifest-path {} --target-dir {}` to remove {}",
            manifest_path.display(),
            target_dir.display(),
            target_dir.display()
        );

        Ok(ActionPlan {
            action: ID,
            resource: ev.resource.clone(),
            steps,
            expected_reclaimed_bytes: ev.reclaimable_bytes.clone(),
            explain,
        })
    }
}

fn target_dir_path(resource: &ResourceId) -> Result<&PathBuf, ActionError> {
    match &resource.locator {
        ResourceLocator::Path(p) => Ok(p),
        ResourceLocator::Tool { .. } => Err(ActionError::Unsupported(
            "cargo target dir resource must be a Path locator".to_string(),
        )),
    }
}

/// Derives the Cargo manifest path for `resource`'s `target_dir`.
///
/// HORO-1017: when `resource.source_project_root` is known (populated by
/// [`crate::detectors::cargo::CargoDetector`]), the manifest is looked up
/// there — the actual project root the detector discovered this resource
/// under — rather than next to `target_dir`'s (possibly symlink-resolved)
/// parent. This matters when `target/` is a symlink to a physically
/// different location: deriving the manifest from the canonicalized
/// target path's parent could pick up an unrelated project's `Cargo.toml`
/// sitting next to that physical location instead of the real one.
///
/// Falls back to `target_dir`'s parent ONLY when no project root is known
/// (e.g. a caller other than the Cargo detector constructs evidence
/// without one) — preserving the original derivation for that case.
/// Either way, this fails closed with [`ActionError::Unsupported`] when
/// the resulting candidate directory has no `Cargo.toml`.
fn manifest_path_for(resource: &ResourceId, target_dir: &Path) -> Result<PathBuf, ActionError> {
    let project_root = match &resource.source_project_root {
        Some(root) => root.clone(),
        None => target_dir
            .parent()
            .ok_or_else(|| {
                ActionError::Unsupported(format!(
                    "target dir {} has no parent directory",
                    target_dir.display()
                ))
            })?
            .to_path_buf(),
    };
    let manifest_path = project_root.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Err(ActionError::Unsupported(format!(
            "no Cargo.toml found at {}",
            manifest_path.display()
        )));
    }
    Ok(manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::model::{
        NativeCleanup, ResourceFingerprint, ResourceKind as RK, ResourceLocator as RL,
    };
    use crate::evidence::probe::{ProbeOutcome, ProbeReason};
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn evidence_for(resource_path: PathBuf, kind: RK) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, RL::Path(resource_path)),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    fn evidence_for_with_root(
        resource_path: PathBuf,
        kind: RK,
        source_project_root: Option<PathBuf>,
    ) -> Evidence {
        let mut resource = ResourceId::new(kind, RL::Path(resource_path));
        if let Some(root) = source_project_root {
            resource = resource.with_source_project_root(root);
        }
        Evidence {
            resource,
            ..evidence_for(PathBuf::from("/unused"), kind)
        }
    }

    #[test]
    fn resource_mismatch_is_rejected() {
        let ev = evidence_for(PathBuf::from("/tmp/x/target"), RK::NodeModules);
        assert_eq!(
            CargoCleanTargetDir.plan(&ev).unwrap_err(),
            ActionError::ResourceMismatch
        );
    }

    #[test]
    fn missing_cargo_toml_is_unsupported() {
        let root = make_temp_dir("cargo-plan-no-manifest");
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let ev = evidence_for(target_dir, RK::CargoTargetDir);

        match CargoCleanTargetDir.plan(&ev) {
            Err(ActionError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn plan_renders_expected_run_tool_step() {
        let root = make_temp_dir("cargo-plan-ok");
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let ev = evidence_for(target_dir.clone(), RK::CargoTargetDir);

        let plan = CargoCleanTargetDir.plan(&ev).expect("plan should succeed");
        assert_eq!(plan.action, ID);
        assert_eq!(plan.steps.len(), 1);
        match &plan.steps[0] {
            ActionStep::RunTool {
                tool,
                args,
                scoped_path,
            } => {
                assert_eq!(*tool, ToolBinary::Cargo);
                assert_eq!(args[0], "clean");
                assert!(args.contains(&"--manifest-path".to_string()));
                assert!(args.contains(&"--target-dir".to_string()));
                assert!(args.contains(&target_dir.to_string_lossy().into_owned()));
                assert_eq!(scoped_path.as_deref(), Some(target_dir.as_path()));
            }
            other => panic!("expected RunTool, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// Real-execution test against a disposable tempdir fixture, per
    /// HORO-951's test requirements. This is safe: `root` is a
    /// process-unique directory under `std::env::temp_dir()`, never a
    /// real project. Includes a `CACHEDIR.TAG` marker because `cargo
    /// clean --target-dir <dir>` refuses to clean a directory that
    /// doesn't look like a real cargo target dir (a real safety feature
    /// of cargo itself, discovered while building this fixture).
    #[test]
    fn plan_step_actually_removes_target_dir_when_run() {
        let root = make_temp_dir("cargo-real-exec");
        let target_dir = root.join("target");
        fs::create_dir_all(target_dir.join("debug")).unwrap();
        fs::write(
            target_dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(target_dir.join("debug/build_output.bin"), vec![0u8; 4096]).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"glomeris-test-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let ev = evidence_for(target_dir.clone(), RK::CargoTargetDir);
        let plan = CargoCleanTargetDir.plan(&ev).expect("plan should succeed");
        let ActionStep::RunTool { tool, args, .. } = &plan.steps[0] else {
            panic!("expected a RunTool step");
        };

        let status = Command::new(tool.program())
            .args(args)
            .status()
            .expect("failed to spawn cargo clean");
        assert!(status.success(), "cargo clean exited with {status}");
        assert!(
            !target_dir.exists(),
            "expected target dir to be removed by cargo clean"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1017 regression: `target/` is a symlink to a physically
    /// different directory that sits right next to an UNRELATED
    /// ("decoy") project's `Cargo.toml`. Without `source_project_root`,
    /// `manifest_path_for` would derive the manifest from the
    /// canonicalized target dir's parent and pick up the decoy manifest
    /// instead of the real project's.
    #[test]
    fn manifest_path_uses_source_project_root_not_decoy_neighbor() {
        let real_root = make_temp_dir("cargo-real-project");
        fs::write(
            real_root.join("Cargo.toml"),
            "[package]\nname = \"real-project\"\n",
        )
        .unwrap();

        // The physical target dir lives in its own directory, sitting
        // right next to a decoy project's Cargo.toml.
        let physical_container = make_temp_dir("cargo-physical-container");
        let physical_target = physical_container.join("target");
        fs::create_dir_all(&physical_target).unwrap();
        fs::write(
            physical_container.join("Cargo.toml"),
            "[package]\nname = \"decoy-project\"\n",
        )
        .unwrap();

        // real_root/target -> physical_target (symlink).
        let symlinked_target = real_root.join("target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&physical_target, &symlinked_target).unwrap();

        // Evidence carries the CANONICALIZED (symlink-resolved) path as
        // the resource locator, exactly like the real detector does, but
        // also carries the real, pre-canonicalization project root.
        let canonical_target = symlinked_target.canonicalize().unwrap();
        let ev = evidence_for_with_root(
            canonical_target,
            RK::CargoTargetDir,
            Some(real_root.clone()),
        );

        let plan = CargoCleanTargetDir.plan(&ev).expect("plan should succeed");
        let ActionStep::RunTool { args, .. } = &plan.steps[0] else {
            panic!("expected a RunTool step");
        };
        let manifest_idx = args
            .iter()
            .position(|a| a == "--manifest-path")
            .expect("--manifest-path must be present");
        let manifest_path = &args[manifest_idx + 1];

        assert_eq!(
            manifest_path,
            &real_root.join("Cargo.toml").to_string_lossy().into_owned(),
            "manifest path must point at the REAL project's Cargo.toml, not the decoy's"
        );
        assert_ne!(
            manifest_path,
            &physical_container
                .join("Cargo.toml")
                .to_string_lossy()
                .into_owned(),
            "manifest path must NOT point at the decoy project's Cargo.toml"
        );

        fs::remove_dir_all(&real_root).ok();
        fs::remove_dir_all(&physical_container).ok();
    }

    /// Confirms the ordinary (non-symlinked) case is unchanged: no
    /// `source_project_root` known, `target/` lives directly under its
    /// own project root — falls back to `target_dir.parent()`, as before
    /// this fix.
    #[test]
    fn manifest_path_falls_back_to_target_parent_when_no_project_root_known() {
        let root = make_temp_dir("cargo-plan-ordinary");
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let ev = evidence_for_with_root(target_dir, RK::CargoTargetDir, None);
        let plan = CargoCleanTargetDir.plan(&ev).expect("plan should succeed");
        let ActionStep::RunTool { args, .. } = &plan.steps[0] else {
            panic!("expected a RunTool step");
        };
        let manifest_idx = args.iter().position(|a| a == "--manifest-path").unwrap();
        assert_eq!(
            args[manifest_idx + 1],
            root.join("Cargo.toml").to_string_lossy().into_owned()
        );

        fs::remove_dir_all(&root).ok();
    }
}
