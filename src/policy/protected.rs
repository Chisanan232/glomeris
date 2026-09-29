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
    // Docker volumes are refused unconditionally, and HORO-1544 narrowed
    // this from the whole Docker category to the one kind that warrants it.
    // The previous rule read "every `DockerImageCache` resource is treated
    // as a possible persistent volume", because a detector could not tell
    // an image from a volume. It can now: images, containers and volumes
    // are separate kinds, so the refusal applies to volumes only.
    //
    // The refusal itself is NOT narrowed. It deliberately covers anonymous
    // volumes too, not just named ones. A `docker compose` service whose
    // volume declaration lost its name still has the developer's database
    // in it, Docker's own `--volumes` prune flag exists precisely because
    // Docker does not consider them disposable either, and no evidence
    // available here distinguishes "anonymous" from "unimportant".
    if resource.kind == ResourceKind::DockerVolume {
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

    fn docker_resource(kind: ResourceKind, id: &str) -> ResourceId {
        ResourceId::new(
            kind,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: id.to_string(),
            },
        )
    }

    /// A named volume holds the only copy of whatever a developer's local
    /// service wrote into it, so it is refused before any evidence is
    /// consulted.
    #[test]
    fn a_named_docker_volume_is_always_protected_persistent_volume() {
        let resource = docker_resource(ResourceKind::DockerVolume, "pgdata");
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedPersistentVolume)
        );
    }

    /// An anonymous volume is refused on exactly the same terms. Docker
    /// minting the name does not make the contents Docker's, and the only
    /// thing "anonymous" reliably says is that nobody wrote a name down.
    #[test]
    fn an_anonymous_docker_volume_is_also_protected_persistent_volume() {
        let resource = docker_resource(
            ResourceKind::DockerVolume,
            "9f2c1b0a7e5d4c3b2a1908f7e6d5c4b3a29180f7e6d5c4b3a29180f7e6d5c4b3",
        );
        assert_eq!(
            protected_reason(&resource),
            Some(ReasonCode::ProtectedPersistentVolume)
        );
    }

    /// The narrowing must be real in both directions: an image and a
    /// container are no longer swept up by the volume refusal, because they
    /// are now separately classified on their own evidence. If either of
    /// these started returning `Some(..)` again the split would have bought
    /// nothing.
    #[test]
    fn docker_images_and_containers_are_not_refused_as_volumes() {
        assert_eq!(
            protected_reason(&docker_resource(ResourceKind::DockerImage, "sha256:abc")),
            None
        );
        assert_eq!(
            protected_reason(&docker_resource(ResourceKind::DockerContainer, "abc123")),
            None
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
