//! Native task entitlements are durable protection, not cleanup authorization.
use super::*;
use crate::evidence::{ResourceId, ResourceLocator};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CargoTarget,
    Worktree,
    DockerBuilder,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: Kind,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub registration: Option<PathBuf>,
    pub common_dir: Option<PathBuf>,
    pub registration_identity: Option<(u64, u64)>,
    pub common_identity: Option<(u64, u64)>,
    pub builder: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Acquired,
    Waiting,
    Suspended,
    Closing,
    Released,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entitlement {
    id: String,
    host: String,
    session: String,
    owner: Identity,
    token: String,
    epoch: u64,
    state: State,
    lease_until: u64,
    pending: Vec<String>,
    children: Vec<Child>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Child {
    launch_id: String,
    identity: Identity,
    token: String,
    epoch: u64,
    released: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRecord {
    schema: u32,
    pub id: String,
    pub generation: String,
    revision: u64,
    scope: Scope,
    last_meaningful_use: u64,
    claims: Vec<Entitlement>,
    receipts: Vec<Replay>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Register {
        request_id: String,
        kind: Kind,
        path: PathBuf,
        builder: Option<String>,
    },
    Acquire {
        request_id: String,
        id: String,
        generation: String,
        revision: u64,
        host: String,
        session: String,
        owner_pid: u32,
    },
    Update {
        request_id: String,
        id: String,
        generation: String,
        revision: u64,
        claim: String,
        epoch: u64,
        token: String,
        update: Update,
    },
    ChildRelease {
        request_id: String,
        id: String,
        generation: String,
        revision: u64,
        claim: String,
        launch_id: String,
        token: String,
        expected_child_epoch: u64,
    },
    ChildResume {
        request_id: String,
        id: String,
        generation: String,
        revision: u64,
        claim: String,
        launch_id: String,
        token: String,
        expected_child_epoch: u64,
        owner_pid: u32,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Update {
    Renew,
    Wait,
    Suspend,
    PrepareChild { launch_id: String },
    ConfirmChild { launch_id: String, pid: u32 },
    CancelUnlaunched { launch_id: String },
    Resume { owner_pid: u32 },
    Release,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replay {
    request: Request,
    reply: serde_json::Value,
}

fn request_id(req: &Request) -> &str {
    match req {
        Request::Register { request_id, .. }
        | Request::Acquire { request_id, .. }
        | Request::Update { request_id, .. } => request_id,
        Request::ChildRelease { request_id, .. } | Request::ChildResume { request_id, .. } => {
            request_id
        }
    }
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn canonical(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(error("absolute native scope required"));
    }
    for parent in path.ancestors() {
        let m = fs::symlink_metadata(parent)?;
        if m.file_type().is_symlink() || (m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0) {
            return Err(error("redirected or writable native ancestry"));
        }
    }
    let path = path.canonicalize()?;
    if fs::symlink_metadata(&path)?.uid() != unsafe { nix::libc::getuid() } {
        return Err(error("native scope belongs to another user"));
    }
    Ok(path)
}
fn git_path(path: &Path, arg: &str) -> io::Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["--no-optional-locks", "-C"]).arg(path).args([
        "rev-parse",
        "--path-format=absolute",
        arg,
    ]);
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    match run_with_timeout(command, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() => canonical(Path::new(
            String::from_utf8(o.stdout).map_err(error)?.trim(),
        )),
        _ => Err(error("native Git registration unavailable")),
    }
}
fn observe(kind: &Kind, path: &Path, builder: &Option<String>) -> io::Result<Scope> {
    let path = canonical(path)?;
    let m = fs::symlink_metadata(&path)?;
    if *kind == Kind::DockerBuilder {
        use std::os::unix::fs::FileTypeExt;
        if !m.file_type().is_socket() || !builder.as_deref().is_some_and(text) {
            return Err(error("exact local Docker socket and builder required"));
        }
    } else if !m.is_dir() || builder.is_some() {
        return Err(error("native directory scope required"));
    }
    let (registration, common_dir) = if *kind == Kind::Worktree {
        if git_path(&path, "--show-toplevel")? != path {
            return Err(error("exact registered worktree root required"));
        }
        (
            Some(git_path(&path, "--absolute-git-dir")?),
            Some(git_path(&path, "--git-common-dir")?),
        )
    } else {
        (None, None)
    };
    Ok(Scope {
        kind: kind.clone(),
        path,
        dev: m.dev(),
        ino: m.ino(),
        registration_identity: registration
            .as_ref()
            .map(fs::metadata)
            .transpose()?
            .map(|m| (m.dev(), m.ino())),
        common_identity: common_dir
            .as_ref()
            .map(fs::metadata)
            .transpose()?
            .map(|m| (m.dev(), m.ino())),
        registration,
        common_dir,
        builder: builder.clone(),
    })
}

impl Store {
    fn native_lock(&self, exclusive: bool) -> io::Result<Flock<File>> {
        self.init()?;
        private_dir(&self.root.join("records/native"))?;
        let path = self.root.join("barriers/native-scopes.lock");
        if path.exists() {
            secure(&path, false)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)?;
        Flock::lock(
            file,
            if exclusive {
                FlockArg::LockExclusiveNonblock
            } else {
                FlockArg::LockSharedNonblock
            },
        )
        .map_err(|(_, e)| io::Error::from(e))
    }
    pub fn read_native(&self, id: &str) -> io::Result<NativeRecord> {
        let path = self.native_path(id)?;
        secure(&path, false)?;
        if fs::metadata(&path)?.len() > 2 * 1024 * 1024 {
            return Err(error("native record exceeds bound"));
        }
        let record: NativeRecord = serde_json::from_reader(File::open(path)?).map_err(error)?;
        if record.schema != 2
            || record.id != id
            || !valid_id(&record.generation)
            || record.claims.len() > 64
            || record.receipts.len() > 256
        {
            return Err(error("native custody record invalid"));
        }
        Ok(record)
    }
    fn native_records(&self) -> io::Result<Vec<NativeRecord>> {
        let mut records = vec![];
        let root = self.root.join("records/native");
        if !root.exists() {
            return Ok(records);
        }
        secure(&root, true)?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("tmp-"))
            {
                // Interrupted atomic writes are not published resource records.
                continue;
            }
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                return Err(error("unknown native record entry"));
            }
            let id = entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
                .filter(|s| valid_id(s))
                .ok_or_else(|| error("unknown native record filename"))?;
            if records.len() >= 256 {
                return Err(error("native custody inventory bound exceeded"));
            }
            records.push(self.read_native(&id)?);
        }
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }
    fn native_path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(error("invalid native scope ID"));
        }
        Ok(self.root.join("records/native").join(format!("{id}.json")))
    }
    pub fn native_inspect(&self) -> io::Result<Vec<serde_json::Value>> {
        self.native_records()?
            .iter()
            .map(|r| self.native_view(r))
            .collect()
    }
    pub(super) fn native_view(&self, r: &NativeRecord) -> io::Result<serde_json::Value> {
        let claims: Vec<_> = r.claims.iter().map(|c| serde_json::json!({
            "claim":c.id,"host":c.host,"session":c.session,"owner":c.owner,"epoch":c.epoch,
            "state":c.state,"lease_until":c.lease_until,"pending_children":c.pending.len(),
            "children":c.children.iter().map(|child|serde_json::json!({"launch_id":child.launch_id,
                "identity":child.identity,"released":child.released})).collect::<Vec<_>>(),
            "owner_live":process(c.owner.pid).ok().flatten().map(|i|i==c.owner)
        })).collect();
        Ok(
            serde_json::json!({"id":r.id,"generation":r.generation,"revision":r.revision,
            "scope":r.scope,"claims":claims,"last_meaningful_use":r.last_meaningful_use,
            "identity_provenance":"process identity observed; host/session/builder labels caller-declared",
            "canonical_host_lineage":"not_observed","docker_daemon_identity":"not_observed",
            "eligible":false,"coverage":"incomplete","cleanup_authority":false,
            "blockers":["native future-consumer coverage requires verified handover", "native reconstructability not attested"]}),
        )
    }
    pub fn native_request(&self, req: Request) -> io::Result<serde_json::Value> {
        if !valid_id(request_id(&req)) {
            return Err(error("unique 32-hex request ID required"));
        }
        let _global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
        let _barrier = self.native_lock(true)?;
        let mut records = match &req {
            Request::Register { .. } => self.native_records()?,
            Request::Acquire { id, .. }
            | Request::Update { id, .. }
            | Request::ChildRelease { id, .. }
            | Request::ChildResume { id, .. } => vec![self.read_native(id)?],
        };
        for r in &records {
            if let Some(old) = r
                .receipts
                .iter()
                .find(|entry| request_id(&entry.request) == request_id(&req))
            {
                if old.request != req {
                    return Err(error("request ID reused with different operation"));
                }
                if observe(&r.scope.kind, &r.scope.path, &r.scope.builder)? != r.scope {
                    return Err(error("replayed native scope identity changed"));
                }
                if let Some(cap) = old.reply.get("capability") {
                    let current = r
                        .claims
                        .iter()
                        .find(|c| Some(c.id.as_str()) == cap.get("claim").and_then(|v| v.as_str()));
                    if !current.is_some_and(|c| {
                        c.state != State::Released
                            && Some(c.epoch) == cap.get("epoch").and_then(|v| v.as_u64())
                            && Some(c.token.as_str()) == cap.get("token").and_then(|v| v.as_str())
                    }) {
                        return Err(error("replayed native capability was superseded"));
                    }
                    let owner = &current.unwrap().owner;
                    if caller(owner.pid)? != *owner {
                        return Err(error("replayed claimant identity changed"));
                    }
                } else if let Request::ChildRelease {
                    claim,
                    launch_id,
                    token,
                    expected_child_epoch,
                    ..
                } = &req
                {
                    let child = r
                        .claims
                        .iter()
                        .find(|c| c.id == *claim)
                        .and_then(|c| c.children.iter().find(|ch| ch.launch_id == *launch_id))
                        .ok_or_else(|| error("replayed child missing"))?;
                    if child.token != *token
                        || child.epoch != *expected_child_epoch
                        || caller(child.identity.pid)? != child.identity
                    {
                        return Err(error("replayed child authority changed"));
                    }
                } else if let Request::ChildResume {
                    claim, launch_id, ..
                } = &req
                {
                    let child = r
                        .claims
                        .iter()
                        .find(|c| c.id == *claim)
                        .and_then(|c| c.children.iter().find(|ch| ch.launch_id == *launch_id))
                        .ok_or_else(|| error("replayed child missing"))?;
                    let cap = old
                        .reply
                        .get("child_capability")
                        .ok_or_else(|| error("replayed child capability missing"))?;
                    if child.released
                        || cap.get("child_token").and_then(|v| v.as_str())
                            != Some(child.token.as_str())
                        || cap.get("child_epoch").and_then(|v| v.as_u64()) != Some(child.epoch)
                        || caller(child.identity.pid)? != child.identity
                    {
                        return Err(error("replayed child handover superseded"));
                    }
                } else if let Request::Update {
                    claim,
                    epoch,
                    token,
                    update,
                    ..
                } = &req
                {
                    let c = r
                        .claims
                        .iter()
                        .find(|c| c.id == *claim)
                        .ok_or_else(|| error("replayed claim missing"))?;
                    if c.epoch != *epoch || c.token != *token || caller(c.owner.pid)? != c.owner {
                        return Err(error("replayed claim owner or capability changed"));
                    }
                    if c.state == State::Released && !matches!(update, Update::Release) {
                        return Err(error("replayed claim released"));
                    }
                }
                return Ok(old.reply.clone());
            }
        }
        let mut record = match &req {
            Request::Register {
                kind,
                path,
                builder,
                ..
            } => {
                let scope = observe(kind, path, builder)?;
                // Older producer-only cores must not mistake native access for closed coverage.
                if scope.path.starts_with(&self.root) || self.root.starts_with(&scope.path) {
                    return Err(error(
                        "native scopes must stay outside Glomeris's protected state namespace",
                    ));
                }
                if let Some(found) = records.iter().position(|r| r.scope == scope) {
                    records.remove(found)
                } else {
                    if records.len() >= 256 {
                        return Err(error(
                            "native admission bound reached; existing owners preserved",
                        ));
                    }
                    NativeRecord {
                        schema: 2,
                        id: random()?,
                        generation: random()?,
                        revision: 0,
                        scope,
                        last_meaningful_use: now(),
                        claims: vec![],
                        receipts: vec![],
                    }
                }
            }
            Request::Acquire {
                id,
                generation,
                revision,
                ..
            }
            | Request::Update {
                id,
                generation,
                revision,
                ..
            }
            | Request::ChildRelease {
                id,
                generation,
                revision,
                ..
            }
            | Request::ChildResume {
                id,
                generation,
                revision,
                ..
            } => {
                let record = self.read_native(id)?;
                if record.generation != *generation || record.revision != *revision {
                    return Err(error("stale native generation or revision"));
                }
                if observe(
                    &record.scope.kind,
                    &record.scope.path,
                    &record.scope.builder,
                )? != record.scope
                {
                    return Err(error("native scope identity changed"));
                }
                record
            }
        };
        let bounded_transition = matches!(
            &req,
            Request::Update {
                update: Update::Renew
                    | Update::Resume { .. }
                    | Update::ConfirmChild { .. }
                    | Update::Release
                    | Update::CancelUnlaunched { .. },
                ..
            }
        ) || matches!(
            &req,
            Request::ChildRelease { .. } | Request::ChildResume { .. }
        );
        if !bounded_transition && record.receipts.len() >= 64 {
            return Err(error(
                "native admission capacity exhausted; closure capacity reserved",
            ));
        }
        let mut secret = None;
        match &req {
            Request::Register { .. } => {}
            Request::ChildRelease {
                claim,
                launch_id,
                token,
                expected_child_epoch,
                ..
            } => {
                let c = record
                    .claims
                    .iter_mut()
                    .find(|c| c.id == *claim)
                    .ok_or_else(|| error("parent claim missing"))?;
                let child = c
                    .children
                    .iter_mut()
                    .find(|ch| ch.launch_id == *launch_id)
                    .ok_or_else(|| error("native child missing"))?;
                if child.token != *token
                    || child.epoch != *expected_child_epoch
                    || caller(child.identity.pid)? != child.identity
                {
                    return Err(error("bound child authority required"));
                }
                if child.released {
                    return Ok(
                        serde_json::json!({"status":"already_released","id":record.id,
                        "generation":record.generation,"revision":record.revision,"cleanup_authority":false}),
                    );
                }
                child.released = true;
                if c.state == State::Closing
                    && c.pending.is_empty()
                    && c.children.iter().all(|ch| ch.released)
                {
                    c.state = State::Released;
                }
                record.last_meaningful_use = now();
            }
            Request::ChildResume {
                claim,
                launch_id,
                token,
                expected_child_epoch,
                owner_pid,
                ..
            } => {
                let c = record
                    .claims
                    .iter_mut()
                    .find(|c| c.id == *claim)
                    .ok_or_else(|| error("parent claim missing"))?;
                if c.state == State::Released {
                    return Err(error("terminal parent cannot reacquire child"));
                }
                let child = c
                    .children
                    .iter_mut()
                    .find(|ch| ch.launch_id == *launch_id)
                    .ok_or_else(|| error("child entitlement missing"))?;
                if child.released || child.token != *token || child.epoch != *expected_child_epoch {
                    return Err(error("child handover capability stale or released"));
                }
                child.identity = caller(*owner_pid)?;
                child.epoch = child
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| error("child epoch exhausted"))?;
                child.token = random()?;
                secret = Some(
                    serde_json::json!({"child_launch_id":launch_id,"child_token":child.token,"child_epoch":child.epoch}),
                );
            }
            Request::Acquire {
                host,
                session,
                owner_pid,
                ..
            } => {
                if !matches!(host.as_str(), "claude" | "codex")
                    || !text(session)
                    || record.claims.len() >= 32
                {
                    return Err(error("invalid or exhausted native owner scope"));
                }
                let owner = caller(*owner_pid)?;
                let token = random()?;
                let id = random()?;
                record.claims.push(Entitlement {
                    id: id.clone(),
                    host: host.clone(),
                    session: session.clone(),
                    owner,
                    token: token.clone(),
                    epoch: 1,
                    state: State::Acquired,
                    lease_until: now() + 60,
                    pending: vec![],
                    children: vec![],
                });
                record.last_meaningful_use = now();
                secret = Some(serde_json::json!({"claim":id,"epoch":1,"token":token}));
            }
            Request::Update {
                claim,
                epoch,
                token,
                update,
                ..
            } => {
                let child_count: usize = record
                    .claims
                    .iter()
                    .map(|c| c.children.len() + c.pending.len())
                    .sum();
                let c = record
                    .claims
                    .iter_mut()
                    .find(|c| c.id == *claim)
                    .ok_or_else(|| error("native claim missing"))?;
                if c.token != *token || c.epoch != *epoch || c.state == State::Released {
                    return Err(error("native claimant capability stale or released"));
                }
                if !matches!(update, Update::Resume { .. }) && caller(c.owner.pid)? != c.owner {
                    return Err(error(
                        "native update requires the current process-bound owner",
                    ));
                }
                match update {
                    Update::Renew => {
                        c.lease_until = now() + 60;
                    }
                    Update::Wait => c.state = State::Waiting,
                    Update::Suspend => c.state = State::Suspended,
                    Update::PrepareChild { launch_id } => {
                        if !valid_id(launch_id)
                            || c.pending.contains(launch_id)
                            || c.children.iter().any(|ch| ch.launch_id == *launch_id)
                            || child_count >= 32
                        {
                            return Err(error("invalid or duplicate native launch intent"));
                        }
                        c.pending.push(launch_id.clone());
                    }
                    Update::ConfirmChild { launch_id, pid } => {
                        let child = process(*pid)?
                            .ok_or_else(|| error("native child identity unavailable"))?;
                        if !c.pending.contains(launch_id) || c.children.len() >= 32 {
                            return Err(error(
                                "child was not acquired before launch or capacity exhausted",
                            ));
                        }
                        let child_token = random()?;
                        c.children.push(Child {
                            launch_id: launch_id.clone(),
                            identity: child,
                            token: child_token.clone(),
                            epoch: 1,
                            released: false,
                        });
                        c.pending.retain(|v| v != launch_id);
                        secret = Some(
                            serde_json::json!({"child_launch_id":launch_id,"child_token":child_token,"child_epoch":1}),
                        );
                    }
                    Update::CancelUnlaunched { launch_id } => {
                        if !c.pending.contains(launch_id) {
                            return Err(error("pending launch missing"));
                        }
                        c.pending.retain(|v| v != launch_id);
                        if c.state == State::Closing
                            && c.pending.is_empty()
                            && c.children.iter().all(|ch| ch.released)
                        {
                            c.state = State::Released;
                        }
                    }
                    Update::Resume { owner_pid } => {
                        c.owner = caller(*owner_pid)?;
                        c.epoch = c
                            .epoch
                            .checked_add(1)
                            .ok_or_else(|| error("claimant epoch exhausted"))?;
                        c.token = random()?;
                        c.state = State::Acquired;
                        c.lease_until = now() + 60;
                        secret =
                            Some(serde_json::json!({"claim":c.id,"epoch":c.epoch,"token":c.token}));
                    }
                    Update::Release => {
                        // Death is not an authoritative descendant release; keep its entitlement.
                        let next = if c.pending.is_empty()
                            && c.children.iter().all(|child| child.released)
                        {
                            State::Released
                        } else {
                            State::Closing
                        };
                        if c.state != next {
                            record.last_meaningful_use = now();
                        }
                        c.state = next;
                    }
                }
            }
        }
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| error("native revision exhausted"))?;
        let mut reply = serde_json::json!({"status":"acknowledged","id":record.id,"generation":record.generation,
            "revision":record.revision,"cleanup_authority":false});
        if let Some(capability) = secret {
            if capability.get("claim").is_some() {
                reply["capability"] = capability;
            } else {
                reply["child_capability"] = capability;
            }
        }
        // Superseded control receipts cannot be replayed against the new revision/epoch.
        if let Request::Update { claim, update, .. } = &req {
            record.receipts.retain(|old|!matches!(&old.request,Request::Update {claim:prior,update:prior_update,..}
                if prior==claim&&matches!((update,prior_update),(Update::Renew,Update::Renew)|(Update::Resume{..},Update::Resume{..})|(Update::Release,Update::Release))));
        }
        if let Request::ChildResume {
            claim, launch_id, ..
        } = &req
        {
            record.receipts.retain(|old| {
                !matches!(&old.request,Request::ChildResume{claim:prior,launch_id:prior_launch,..}
                if prior==claim&&prior_launch==launch_id)
            });
        }
        record.receipts.push(Replay {
            request: req,
            reply: reply.clone(),
        });
        if record.receipts.len() > 256
            || serde_json::to_vec(&record).map_err(error)?.len() > 2 * 1024 * 1024
        {
            return Err(error(
                "native durable state capacity exhausted; existing entitlements preserved",
            ));
        }
        durable(&self.native_path(&record.id)?, &record)?;
        Ok(reply)
    }
}
fn caller(pid: u32) -> io::Result<Identity> {
    if pid != std::process::id() && pid != unsafe { nix::libc::getppid() as u32 } {
        return Err(error("native acquisition must bind the caller before use"));
    }
    process(pid)?.ok_or_else(|| error("native caller identity unavailable"))
}

/// The shared barrier spans revalidation and mutation, including initially empty inventory.
pub(crate) fn execution_guard(resource: &ResourceId) -> io::Result<Flock<File>> {
    let store = Store::current()?;
    execution_guard_at(&store, resource)
}
fn execution_guard_at(store: &Store, resource: &ResourceId) -> io::Result<Flock<File>> {
    let guard = store.native_lock(false)?;
    for record in store.native_records()? {
        if record.scope.kind == Kind::DockerBuilder {
            let relevant = match &resource.locator {
                ResourceLocator::Path(p) => {
                    let resolved = p.canonicalize().unwrap_or_else(|_| p.clone());
                    resolved.starts_with(&record.scope.path)
                        || record.scope.path.starts_with(&resolved)
                }
                ResourceLocator::Tool { .. } => {
                    resource.kind.owning_tool() == crate::evidence::model::OwningTool::Docker
                }
            };
            if !relevant {
                continue;
            }
        }
        if observe(
            &record.scope.kind,
            &record.scope.path,
            &record.scope.builder,
        )? != record.scope
        {
            return Err(error(
                "native scope identity unresolved; retirement refused",
            ));
        }
        let overlaps = match &resource.locator {
            ResourceLocator::Path(p) => {
                // A resource that vanished between policy approval and
                // execution (the TOCTOU race `execute_inner` is designed to
                // catch via its own fingerprint check) must not abort here
                // with a generic I/O error that masks that more specific
                // diagnosis. Fall back to the un-canonicalized path, same as
                // the DockerBuilder branch above — `fs::metadata` below
                // already degrades to `false` on a missing path, so this
                // only changes behavior for a resource that no longer exists.
                let p = p.canonicalize().unwrap_or_else(|_| p.clone());
                p.starts_with(&record.scope.path)
                    || record.scope.path.starts_with(&p)
                    || fs::metadata(&p)
                        .is_ok_and(|m| m.dev() == record.scope.dev && m.ino() == record.scope.ino)
            }
            ResourceLocator::Tool { .. } => {
                resource.kind.owning_tool() == crate::evidence::model::OwningTool::Docker
                    && record.scope.kind == Kind::DockerBuilder
            }
        };
        if overlaps {
            return Err(error(
                "enrolled native scope has no verified retirement authority",
            ));
        }
    }
    Ok(guard)
}

pub(super) fn cli(store: &Store, args: &[String]) -> io::Result<()> {
    if args == ["inspect", "--json"] || args == ["reconcile", "--json"] {
        // Reconciliation never infers release from process death, expiry or host restart.
        return write_reply(&store.native_inspect()?);
    }
    if args != ["request", "--json"] {
        return Err(error(
            "native request --json (bounded stdin), inspect|reconcile --json",
        ));
    }
    let mut bytes = vec![];
    io::stdin().take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(error("native request exceeds 64KiB"));
    }
    let req: Request =
        serde_json::from_slice(&bytes).map_err(|_| error("invalid native custody request"))?;
    write_reply(&store.native_request(req)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::ResourceKind;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("native-barrier-{}", random().unwrap()));
            private_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn initially_empty_namespace_guard_excludes_native_admission() {
        let fixture = Fixture::new();
        let store = Store {
            root: fixture.0.join("store"),
        };
        let target = fixture.0.join("target");
        private_dir(&target).unwrap();
        let resource = ResourceId::new(ResourceKind::CargoTargetDir, ResourceLocator::Path(target));
        let guard = execution_guard_at(&store, &resource).unwrap();
        assert!(store.native_records().unwrap().is_empty());
        assert!(
            store.native_lock(true).is_err(),
            "registration must conflict even before the first scope exists"
        );
        drop(guard);
        assert!(store.native_lock(true).is_ok());
    }

    #[test]
    fn native_guard_protects_aliases_and_changed_filesystem_scope_identity() {
        let fixture = Fixture::new();
        let store = Store {
            root: fixture.0.join("store"),
        };
        drop(store.native_lock(true).unwrap());
        let target = fixture.0.join("target");
        private_dir(&target).unwrap();
        let scope = observe(&Kind::CargoTarget, &target, &None).unwrap();
        let id = random().unwrap();
        let r = NativeRecord {
            schema: 2,
            id: id.clone(),
            generation: random().unwrap(),
            revision: 0,
            scope,
            last_meaningful_use: now(),
            claims: vec![],
            receipts: vec![],
        };
        durable(&store.native_path(&id).unwrap(), &r).unwrap();
        let alias = target.join("../target");
        assert!(execution_guard_at(
            &store,
            &ResourceId::new(ResourceKind::CargoTargetDir, ResourceLocator::Path(alias))
        )
        .is_err());
        let renamed = fixture.0.join("renamed");
        fs::rename(&target, &renamed).unwrap();
        assert!(execution_guard_at(
            &store,
            &ResourceId::new(ResourceKind::CargoTargetDir, ResourceLocator::Path(renamed))
        )
        .is_err());
    }

    // HORO-1913: a non-empty native store (a real enrolled, non-overlapping
    // scope — exactly what any host that has actually used native
    // retirement will have) must not turn a vanished resource into a
    // generic I/O failure here. The vanished-resource case belongs to
    // `execute_inner`'s fingerprint check (`ResourceIdentityChanged`), not
    // to this guard's `canonicalize()?` on a path that no longer exists.
    #[test]
    fn native_guard_tolerates_a_vanished_resource_even_with_an_unrelated_enrolled_scope() {
        let fixture = Fixture::new();
        let store = Store {
            root: fixture.0.join("store"),
        };
        drop(store.native_lock(true).unwrap());
        let enrolled_target = fixture.0.join("enrolled-target");
        private_dir(&enrolled_target).unwrap();
        let scope = observe(&Kind::CargoTarget, &enrolled_target, &None).unwrap();
        let r = NativeRecord {
            schema: 2,
            id: random().unwrap(),
            generation: random().unwrap(),
            revision: 0,
            scope,
            last_meaningful_use: now(),
            claims: vec![],
            receipts: vec![],
        };
        durable(&store.native_path(&r.id).unwrap(), &r).unwrap();

        let vanished = fixture.0.join("node_modules");
        private_dir(&vanished).unwrap();
        fs::remove_dir_all(&vanished).unwrap();

        assert!(
            execution_guard_at(
                &store,
                &ResourceId::new(ResourceKind::NodeModules, ResourceLocator::Path(vanished))
            )
            .is_ok(),
            "a vanished, non-overlapping resource must not fail this guard with a generic I/O error"
        );
    }
}
