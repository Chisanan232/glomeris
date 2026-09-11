//! Path/pattern-based `PROTECTED` matcher (HORO-950).
//!
//! This is a deliberately conservative, MVP-scope, NON-exhaustive
//! denylist — a starting allowlist of patterns to extend, not a claim of
//! completeness. It performs no filesystem I/O (no `canonicalize`, no
//! `read_link`, no `metadata`): it only inspects the path/locator text
//! already carried by a [`ResourceId`], which keeps
//! [`crate::policy::engine::classify`] pure.

use std::path::{Component, Path};

use crate::evidence::{ResourceId, ResourceKind, ResourceLocator};

use super::class::ReasonCode;

/// Returns `Some(reason)` if `resource` matches one of the conservative
/// protected categories below, `None` otherwise. Called first, before any
/// evidence-freshness logic, from `classify` — a Protected classification
/// never depends on evidence freshness/completeness at all.
pub(super) fn protected_reason(resource: &ResourceId) -> Option<ReasonCode> {
    // Docker persistent-volume conservatism: detectors do not yet
    // distinguish a persistent Docker volume from disposable image cache.
    // Since that ambiguity cannot be resolved from the evidence available
    // today, every `DockerImageCache` resource is treated as a possible
    // persistent volume and classified Protected unconditionally. Revisit
    // once a detector can positively identify build/image cache vs. a
    // named volume.
    if resource.kind == ResourceKind::DockerImageCache {
        return Some(ReasonCode::ProtectedPersistentVolume);
    }

    let ResourceLocator::Path(path) = &resource.locator else {
        return None;
    };

    if is_credential_material(path) {
        return Some(ReasonCode::ProtectedCredentialMaterial);
    }
    if is_git_internals(path) {
        return Some(ReasonCode::ProtectedGitInternals);
    }
    if is_infra_state(path) {
        return Some(ReasonCode::ProtectedInfraState);
    }
    if is_system_path(path) {
        return Some(ReasonCode::ProtectedSystemPath);
    }
    if is_unsafe_mount_or_symlink_target(path) {
        return Some(ReasonCode::ProtectedUnsafeMountOrSymlink);
    }

    None
}

fn component_strs(path: &Path) -> impl Iterator<Item = &str> {
    path.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    })
}

/// SSH/GPG/credential material. MVP-conservative, not exhaustive: `~/.ssh`,
/// `~/.gnupg`, `*.pem`/`*.key` files, and anything with "credentials" in
/// its name.
fn is_credential_material(path: &Path) -> bool {
    if component_strs(path).any(|c| c == ".ssh" || c == ".gnupg") {
        return true;
    }
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".pem") || lower.ends_with(".key") || lower.contains("credentials")
}

/// Git internals: any path whose components include a `.git` directory
/// itself — not merely "inside a git-tracked project" (that's
/// `GitWorktreeDirty`, a different, evidence-driven reason).
fn is_git_internals(path: &Path) -> bool {
    component_strs(path).any(|c| c == ".git")
}

/// Terraform/OpenTofu state: `*.tfstate`, `*.tfstate.backup`, or a
/// `.terraform/` directory.
fn is_infra_state(path: &Path) -> bool {
    if component_strs(path).any(|c| c == ".terraform") {
        return true;
    }
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    file_name.ends_with(".tfstate") || file_name.ends_with(".tfstate.backup")
}

/// System-critical paths. Conservative denylist, MVP-scope, not
/// exhaustive: `/System`, `/usr` (excluding `/usr/local`), `/bin`,
/// `/sbin`, `/private/var/db`.
fn is_system_path(path: &Path) -> bool {
    if path.starts_with("/usr/local") {
        return false;
    }
    path.starts_with("/System")
        || path.starts_with("/usr")
        || path.starts_with("/bin")
        || path.starts_with("/sbin")
        || path.starts_with("/private/var/db")
}

/// Unsafe mount/symlink targets: conservative, MVP-scope denylist of
/// macOS mount points a resource could point into — external/network
/// volumes whose contents this tool has no business deleting, and never
/// resolved via I/O (that would violate `classify`'s purity).
fn is_unsafe_mount_or_symlink_target(path: &Path) -> bool {
    path.starts_with("/Volumes")
        || path.starts_with("/dev")
        || path.starts_with("/Network")
        || path.starts_with("/net")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::evidence::OwningTool;

    use super::*;

    fn path_resource(kind: ResourceKind, path: &str) -> ResourceId {
        ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path)))
    }

    #[test]
    fn docker_image_cache_is_always_protected_persistent_volume() {
        let resource = ResourceId::new(
            ResourceKind::DockerImageCache,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: "sha256:abc".to_string(),
            },
        );
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedPersistentVolume)
        );
    }

    #[test]
    fn docker_build_cache_is_not_unconditionally_protected() {
        let resource = ResourceId::new(
            ResourceKind::DockerBuildCache,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: "build_cache".to_string(),
            },
        );
        assert_eq!(protected_reason(&resource), None);
    }

    #[test]
    fn ssh_dir_is_protected_credential_material() {
        let resource = path_resource(ResourceKind::CargoTargetDir, "/Users/x/.ssh/id_ed25519");
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedCredentialMaterial)
        );
    }

    #[test]
    fn pem_file_is_protected_credential_material() {
        let resource = path_resource(ResourceKind::CargoTargetDir, "/Users/x/certs/site.pem");
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedCredentialMaterial)
        );
    }

    #[test]
    fn dot_git_component_is_protected_git_internals() {
        let resource = path_resource(
            ResourceKind::CargoTargetDir,
            "/Users/x/proj/.git/objects/pack",
        );
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedGitInternals)
        );
    }

    #[test]
    fn tfstate_file_is_protected_infra_state() {
        let resource = path_resource(
            ResourceKind::CargoTargetDir,
            "/Users/x/infra/terraform.tfstate",
        );
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedInfraState)
        );
    }

    #[test]
    fn dot_terraform_dir_is_protected_infra_state() {
        let resource = path_resource(
            ResourceKind::CargoTargetDir,
            "/Users/x/infra/.terraform/providers",
        );
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedInfraState)
        );
    }

    #[test]
    fn usr_is_protected_system_path_but_usr_local_is_not() {
        let usr = path_resource(ResourceKind::CargoTargetDir, "/usr/lib/foo");
        assert_eq!(
            protected_reason(&usr),
            Some(ReasonCode::ProtectedSystemPath)
        );

        let usr_local = path_resource(ResourceKind::CargoTargetDir, "/usr/local/bin/foo");
        assert_eq!(protected_reason(&usr_local), None);
    }

    #[test]
    fn volumes_mount_is_protected_unsafe_mount() {
        let resource = path_resource(ResourceKind::CargoTargetDir, "/Volumes/External/target");
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedUnsafeMountOrSymlink)
        );
    }

    #[test]
    fn ordinary_project_path_is_not_protected() {
        let resource = path_resource(ResourceKind::CargoTargetDir, "/Users/x/proj/target");
        assert_eq!(protected_reason(&resource), None);
    }
}
