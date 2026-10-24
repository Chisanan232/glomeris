//! Compile committed library source under a fixed standalone Cargo configuration.
//! Original package features/profiles/workspaces are not build inputs; native targets are never adopted.
use super::*;
use std::process::Stdio;

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MANIFEST: &[u8] = b"[package]\nname='glomeris_managed_library'\nversion='0.0.0'\nedition='2021'\n[lib]\npath='src/lib.rs'\n";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub identity: Artifact,
    pub directory: bool,
    pub digest: Option<String>,
    pub links: u64,
    pub mode: u32,
    pub ctime: i64,
    pub ctime_nsec: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoProof {
    pub schema: u32,
    pub contract: String,
    pub inputs: BTreeMap<PathBuf, TreeEntry>,
    pub tree: BTreeMap<String, TreeEntry>,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    pub cargo_digest: String,
    pub rustc_digest: String,
}

fn identity(m: &fs::Metadata) -> Artifact {
    Artifact {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        mtime: m.mtime(),
        mtime_nsec: m.mtime_nsec(),
    }
}
fn open(path: &Path) -> io::Result<File> {
    let parent = access::directory(path.parent().ok_or_else(|| error("parent missing"))?)?;
    access::open_at(
        &parent,
        path.file_name().ok_or_else(|| error("name missing"))?,
        nix::libc::O_RDONLY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
    )
}
fn regular(path: &Path) -> io::Result<File> {
    let f = open(path)?;
    if !f.metadata()?.is_file() {
        return Err(error("regular producer input required"));
    }
    Ok(f)
}
fn digest_file(file: &File, deadline: std::time::Instant) -> io::Result<String> {
    use std::io::{Seek, SeekFrom};
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX_BYTES {
        return Err(error("digest input outside budget"));
    }
    let mut input = file.try_clone()?;
    input.seek(SeekFrom::Start(0))?;
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = Command::new("/usr/bin/shasum");
        c.args(["-a", "256"]);
        c
    };
    #[cfg(not(target_os = "macos"))]
    let mut c = Command::new("/usr/bin/sha256sum");
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::from(input));
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(error("Cargo inspection time budget exceeded"));
    }
    let out = match run_with_timeout(c, remaining.min(Duration::from_secs(10))) {
        CommandOutcome::Completed(o) if o.status.success() => o.stdout,
        _ => return Err(error("bounded SHA-256 unavailable")),
    };
    let value = String::from_utf8(out)
        .map_err(error)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();
    if value.len() != 64
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
        || identity(&before) != identity(&file.metadata()?)
    {
        return Err(error("digest identity changed"));
    }
    Ok(value)
}
fn digest(path: &Path) -> io::Result<String> {
    let file = regular(path)?;
    digest_file(&file, std::time::Instant::now() + Duration::from_secs(10))
}
fn opened_entry(file: &File, deadline: std::time::Instant) -> io::Result<TreeEntry> {
    let m = file.metadata()?;
    if (!m.is_file() && !m.is_dir())
        || m.uid() != unsafe { nix::libc::getuid() }
        || (m.is_file() && m.mode() & 0o111 != 0)
    {
        return Err(error("unsupported Cargo output identity"));
    }
    let e = TreeEntry {
        identity: identity(&m),
        directory: m.is_dir(),
        digest: if m.is_file() {
            Some(digest_file(file, deadline)?)
        } else {
            None
        },
        links: m.nlink(),
        mode: m.mode(),
        ctime: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    };
    if identity(&file.metadata()?) != e.identity {
        return Err(error("Cargo entry changed while inspecting"));
    }
    Ok(e)
}
fn entry(path: &Path) -> io::Result<TreeEntry> {
    opened_entry(
        &open(path)?,
        std::time::Instant::now() + Duration::from_secs(10),
    )
}
fn cloexec_dup(file: &File) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn names(file: &File) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStrExt;
    struct Directory(*mut nix::libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                nix::libc::closedir(self.0);
            }
        }
    }
    let fd = cloexec_dup(file)?.into_raw_fd();
    let dir = unsafe { nix::libc::fdopendir(fd) };
    if dir.is_null() {
        unsafe {
            nix::libc::close(fd);
        }
        return Err(io::Error::last_os_error());
    }
    let dir = Directory(dir);
    unsafe {
        nix::libc::rewinddir(dir.0);
    }
    let mut out = Vec::new();
    loop {
        #[cfg(target_os = "macos")]
        let errno = unsafe { nix::libc::__error() };
        #[cfg(not(target_os = "macos"))]
        let errno = unsafe { nix::libc::__errno_location() };
        unsafe {
            *errno = 0;
        }
        let next = unsafe { nix::libc::readdir(dir.0) };
        if next.is_null() {
            let code = unsafe { *errno };
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
            break;
        }
        let bytes = unsafe { std::ffi::CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if out.len() >= 10000 {
            return Err(error("Cargo directory entry budget exceeded"));
        }
        out.push(std::ffi::OsStr::from_bytes(bytes).to_os_string());
    }
    Ok(out)
}
fn relative(root: &File, path: &Path) -> io::Result<File> {
    let mut file = root.try_clone()?;
    for part in path.components() {
        let std::path::Component::Normal(name) = part else {
            return Err(error("invalid relative Cargo path"));
        };
        file = access::open_at(
            &file,
            name,
            nix::libc::O_RDONLY | nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW,
            0,
        )?;
    }
    Ok(file)
}
pub(super) fn manifest(r: &Record) -> io::Result<BTreeMap<String, TreeEntry>> {
    let path = Store::current()?.path(r);
    manifest_tree(&path, r.dev, r.ino, || Ok(()))
}
fn manifest_tree(
    path: &Path,
    dev: u64,
    ino: u64,
    after_root: impl FnOnce() -> io::Result<()>,
) -> io::Result<BTreeMap<String, TreeEntry>> {
    let root = access::directory(path)?;
    if root.metadata()?.dev() != dev || root.metadata()?.ino() != ino {
        return Err(error("Cargo generation root changed"));
    }
    let root_before = opened_entry(&root, std::time::Instant::now() + Duration::from_secs(1))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut pending = vec![(PathBuf::new(), root.try_clone()?)];
    let mut tree = BTreeMap::new();
    let mut bytes = 0u64;
    let mut links: BTreeMap<(u64, u64), (u64, u64)> = BTreeMap::new();
    let mut after_root = Some(after_root);
    while let Some((prefix, dir)) = pending.pop() {
        let names = names(&dir)?;
        if prefix.as_os_str().is_empty() {
            after_root.take().unwrap()()?;
        }
        for name in names {
            if std::time::Instant::now() >= deadline {
                return Err(error("Cargo inspection time budget exceeded"));
            }
            let p = prefix.join(&name);
            let file = access::open_at(
                &dir,
                &name,
                nix::libc::O_RDONLY | nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW,
                0,
            )?;
            let e = opened_entry(&file, deadline)?;
            if e.identity.dev != dev || tree.len() >= 10000 || name == ".git" {
                return Err(error("Cargo generation boundary exceeded"));
            }
            if e.directory {
                pending.push((p.clone(), file));
            } else {
                bytes = bytes
                    .checked_add(e.identity.len)
                    .ok_or_else(|| error("Cargo byte overflow"))?;
                if bytes > MAX_BYTES {
                    return Err(error("Cargo generation budget exceeded"));
                }
                let count = links
                    .entry((e.identity.dev, e.identity.ino))
                    .or_insert((e.links, 0));
                if count.0 != e.links {
                    return Err(error("Cargo link identity changed"));
                }
                count.1 += 1;
            }
            tree.insert(
                p.to_str()
                    .ok_or_else(|| error("non-UTF8 Cargo output"))?
                    .to_owned(),
                e,
            );
        }
    }
    if links.values().any(|(expected, seen)| expected != seen) {
        return Err(error("Cargo output has an external hard-link consumer"));
    }
    for (name, e) in &tree {
        if std::time::Instant::now() >= deadline {
            return Err(error("Cargo final inspection budget exceeded"));
        }
        let m = relative(&root, Path::new(name))?.metadata()?;
        if identity(&m) != e.identity
            || m.nlink() != e.links
            || m.mode() != e.mode
            || m.ctime() != e.ctime
            || m.ctime_nsec() != e.ctime_nsec
        {
            return Err(error("Cargo traversal identity changed"));
        }
    }
    let current_root = access::directory(path)?;
    if opened_entry(&root, deadline)? != root_before
        || opened_entry(&current_root, deadline)? != root_before
    {
        return Err(error("Cargo generation ancestry changed"));
    }
    Ok(tree)
}
fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = regular(path)?;
    let before = file.metadata()?;
    if before.len() > limit {
        return Err(error("producer input budget exceeded"));
    }
    let mut bytes = Vec::new();
    file.try_clone()?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit || identity(&before) != identity(&file.metadata()?) {
        return Err(error("producer input changed while reading"));
    }
    Ok(bytes)
}
pub(super) fn verify(r: &Record) -> io::Result<()> {
    let proof = r
        .cargo_proof
        .as_ref()
        .ok_or_else(|| error("Cargo producer proof missing"))?;
    let generic = r
        .producer
        .as_ref()
        .ok_or_else(|| error("closed Cargo consumer proof missing"))?;
    let store = Store::current()?;
    let inputs = store
        .root
        .join("inputs")
        .join(format!("{}-{}", r.id, r.generation));
    if proof.schema != 1
        || proof.contract != "standalone-source-v1"
        || !generic.closed_consumers
        || generic.input != inputs.join("src/lib.rs")
        || proof.inputs.len() != 3
    {
        return Err(error("Cargo input/consumer closure incomplete"));
    }
    for name in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        let path = inputs.join(name);
        secure(&path, false)?;
        if proof.inputs.get(&path) != Some(&entry(&path)?) {
            return Err(error("Cargo reconstruction input changed"));
        }
    }
    if read_bounded(&inputs.join("Cargo.toml"), 4096)? != MANIFEST {
        return Err(error("unsupported Cargo reconstruction contract"));
    }
    let tree = manifest(r)?;
    if tree != proof.tree
        || r.sealed.as_ref() != Some(&generic.outputs)
        || generic.input_metadata != artifact_identity(&generic.input)?
        || generic.outputs
            != tree
                .into_iter()
                .filter_map(|(p, e)| {
                    if e.directory {
                        None
                    } else {
                        Some((p, e.identity))
                    }
                })
                .collect()
    {
        return Err(error("Cargo output generation changed"));
    }
    Ok(())
}
fn git_bytes(r: &Record, args: &[&str]) -> io::Result<Vec<u8>> {
    let mut c = source_git(&r.source_root);
    c.args(args);
    match run_with_timeout(c, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() && o.stdout.len() <= 65536 => {
            Ok(o.stdout)
        }
        _ => Err(error("bounded committed Cargo input unavailable")),
    }
}
fn tool(name: &str) -> io::Result<PathBuf> {
    let mut c = Command::new("rustup");
    c.args(["which", name]).env("RUSTUP_AUTO_INSTALL", "0");
    let p = match run_with_timeout(c, Duration::from_secs(5)) {
        CommandOutcome::Completed(o) if o.status.success() => {
            PathBuf::from(String::from_utf8(o.stdout).map_err(error)?.trim())
        }
        _ => return Err(error("installed pinned toolchain unavailable")),
    };
    let p = fs::canonicalize(p)?;
    if !p.is_absolute() || !fs::metadata(&p)?.is_file() {
        return Err(error("invalid installed toolchain"));
    }
    Ok(p)
}
fn controlled(c: &mut Command, workspace: &Path, home: &Path, rustc: &Path) {
    c.env_clear()
        .env("HOME", home)
        .env("CARGO_HOME", home)
        .env("RUSTC", rustc)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_CODEGEN_UNITS", "1")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", rustc.parent().unwrap().display()),
        )
        .current_dir(workspace);
}
// Successful exit is insufficient: Cargo can spawn rustc. The private process group
// must be empty before any generation is sealed. Failure leaves recovery state protected.
fn run(c: &mut Command, logs: &Path) -> io::Result<Vec<u8>> {
    run_budget(c, logs, Duration::from_secs(60))
}
fn leader_exited(child: &std::process::Child) -> io::Result<bool> {
    // WNOWAIT keeps the owned child unreaped: its PID/PGID cannot be recycled
    // while we inspect or stop descendants. try_wait() would release that fence.
    let mut info: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            child.id() as nix::libc::id_t,
            &mut info,
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "macos")]
    let pid = info.si_pid;
    #[cfg(not(target_os = "macos"))]
    let pid = unsafe { info.si_pid() };
    Ok(pid == child.id() as i32)
}
fn group_running(pgid: u32, exclude: Option<u32>) -> io::Result<bool> {
    let mut c = Command::new("/bin/ps");
    c.args(["-axo", "pid=,pgid=,stat="]);
    let out = match run_with_timeout(c, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() => o.stdout,
        _ => return Err(error("owned producer group unavailable")),
    };
    for line in std::str::from_utf8(&out).map_err(error)?.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(error("producer group observation incomplete"));
        }
        let pid = parts[0].parse::<u32>().map_err(error)?;
        let group = parts[1].parse::<u32>().map_err(error)?;
        if group == pgid && Some(pid) != exclude && !parts[2].starts_with('Z') {
            return Ok(true);
        }
    }
    Ok(false)
}
fn terminate_owned_group(child: &std::process::Child) -> io::Result<()> {
    let pgid = child.id();
    // This non-reaping check is an ownership fence, not evidence of healthy work.
    // ECHILD/observation failure must never degrade to signalling a numeric PGID.
    leader_exited(child)?;
    if pgid == unsafe { nix::libc::getpgrp() } as u32 {
        return Err(error("refuse caller process group"));
    }
    if !group_running(pgid, None)? {
        return Ok(());
    }
    for (signal, grace) in [
        (nix::libc::SIGTERM, Duration::from_millis(500)),
        (nix::libc::SIGKILL, Duration::from_secs(1)),
    ] {
        leader_exited(child)?;
        if unsafe { nix::libc::kill(-(pgid as i32), signal) } != 0
            && io::Error::last_os_error().raw_os_error() != Some(nix::libc::ESRCH)
        {
            return Err(io::Error::last_os_error());
        }
        let until = std::time::Instant::now() + grace;
        while std::time::Instant::now() < until {
            if !group_running(pgid, None)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Err(error(
        "owned producer termination unconfirmed; recovery state preserved",
    ))
}
fn run_budget(c: &mut Command, logs: &Path, budget: Duration) -> io::Result<Vec<u8>> {
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(logs.with_extension("stdout"))?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(logs.with_extension("stderr"))?;
    c.stdout(stdout).stderr(stderr).stdin(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            if nix::libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let limit = nix::libc::rlimit {
                rlim_cur: 32 * 1024 * 1024,
                rlim_max: 32 * 1024 * 1024,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = c.spawn()?;
    let pgid = child.id();
    let start = std::time::Instant::now();
    loop {
        if leader_exited(&child)? {
            if group_running(pgid, Some(pgid))? {
                terminate_owned_group(&child)?;
                let _ = child.wait();
                return Err(error(
                    "Cargo producer left descendants; generation remains unsealed",
                ));
            }
            let status = child.wait()?;
            if !status.success() {
                return Err(error("Cargo producer failed; generation remains unsealed"));
            }
            break;
        }
        if start.elapsed() > budget {
            terminate_owned_group(&child)?;
            let _ = child.wait();
            return Err(error(
                "Cargo budget expired; incomplete recovery state preserved",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = read_bounded(&logs.with_extension("stdout"), 1024 * 1024)?;
    Ok(output)
}
fn write_input(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    access::directory(path.parent().ok_or_else(|| error("input parent missing"))?)?.sync_all()
}
pub(super) fn produce(store: &Store, id: &str, generation: &str) -> io::Result<()> {
    let _global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let mut r = store.read(id)?;
    store.identity(&r)?;
    if r.class != "managed_cargo_library"
        || r.generation != generation
        || r.production_started
        || r.retirement_started
        || r.reclaimed
        || r.producer.is_some()
        || !r.claims.is_empty()
        || fs::read_dir(store.path(&r))?.next().is_some()
    {
        return Err(error("Cargo producer requires a new unexposed generation"));
    }
    let _barrier = store.barrier(id, true)?;
    source_revision(&r.source_root, Some(&r.source_head))?;
    let listing = git_bytes(&r, &["ls-tree", "-r", "--full-tree", &r.source_head])?;
    for line in std::str::from_utf8(&listing).map_err(error)?.lines() {
        let (meta, path) = line
            .split_once('\t')
            .ok_or_else(|| error("invalid committed tree"))?;
        if !meta.starts_with("100644 blob ")
            || (path.ends_with(".rs") && path != "src/lib.rs")
            || path.starts_with(".cargo/")
            || path == "rust-toolchain"
            || path == "rust-toolchain.toml"
        {
            return Err(error("Cargo source outside standalone library capability"));
        }
    }
    let manifest_bytes = MANIFEST;
    let source = git_bytes(&r, &["show", &format!("{}:src/lib.rs", r.source_head)])?;
    let text = std::str::from_utf8(&source).map_err(error)?;
    for token in [
        "asm",
        "include",
        "env!",
        "option_env",
        "macro_rules",
        "concat",
        "#![feature",
        "extern",
        "#",
    ] {
        if text.contains(token) {
            return Err(error("Cargo source has unsupported hidden inputs"));
        }
    }
    // Persist intent before launching any producer. Crash recovery never upgrades this
    // intent to closure merely because the parent process disappeared.
    r.production_started = true;
    store.save(&r)?;
    #[cfg(test)]
    super::crash_tests::checkpoint("cargo_intent");
    let inputs = store
        .root
        .join("inputs")
        .join(format!("{}-{}", id, generation));
    private_dir(&inputs)?;
    private_dir(&inputs.join("src"))?;
    write_input(&inputs.join("Cargo.toml"), manifest_bytes)?;
    write_input(&inputs.join("src/lib.rs"), &source)?;
    let base = fs::canonicalize(std::env::temp_dir())?;
    for parent in base.ancestors() {
        if parent.join(".cargo/config").exists()
            || parent.join(".cargo/config.toml").exists()
            || parent.join("Cargo.toml").exists()
        {
            return Err(error("ambient Cargo workspace/configuration protected"));
        }
    }
    let workspace = base.join(format!("glomeris-cargo-{}", random()?));
    private_dir(&workspace)?;
    private_dir(&workspace.join("src"))?;
    durable(
        &inputs.join("workspace.json"),
        &serde_json::json!({"schema":1,"workspace":workspace,"id":id,"generation":generation}),
    )?;
    write_input(&workspace.join("Cargo.toml"), manifest_bytes)?;
    write_input(&workspace.join("src/lib.rs"), &source)?;
    let home = inputs.join("cargo-home");
    private_dir(&home)?;
    let cargo = tool("cargo")?;
    let rustc = tool("rustc")?;
    let cargo_digest = digest(&cargo)?;
    let rustc_digest = digest(&rustc)?;
    let mut c = Command::new(&cargo);
    controlled(&mut c, &workspace, &home, &rustc);
    c.arg("read-manifest");
    let meta: serde_json::Value =
        serde_json::from_slice(&run(&mut c, &inputs.join("read-manifest"))?).map_err(error)?;
    let targets = meta["targets"]
        .as_array()
        .ok_or_else(|| error("Cargo targets absent"))?;
    if !meta["dependencies"]
        .as_array()
        .is_some_and(|d| d.is_empty())
        || targets.len() != 1
        || targets[0]["kind"] != serde_json::json!(["lib"])
        || targets[0]["crate_types"] != serde_json::json!(["lib"])
        || targets[0]["src_path"] != workspace.join("src/lib.rs").to_string_lossy().as_ref()
        || meta["links"].as_str().is_some()
    {
        return Err(error(
            "dependencies, scripts, custom targets or external sources are not supported",
        ));
    }
    let mut c = Command::new(&cargo);
    controlled(&mut c, &workspace, &home, &rustc);
    c.args(["metadata", "--offline", "--format-version", "1"]);
    let meta: serde_json::Value =
        serde_json::from_slice(&run(&mut c, &inputs.join("metadata"))?).map_err(error)?;
    if meta["workspace_root"] != workspace.to_string_lossy().as_ref()
        || !meta["packages"].as_array().is_some_and(|a| a.len() == 1)
    {
        return Err(error("external Cargo workspace unsupported"));
    }
    let lock = read_bounded(&workspace.join("Cargo.lock"), 65536)
        .map_err(|e| error(format!("retained Cargo lock missing: {e}")))?;
    if lock.len() > 65536 {
        return Err(error("Cargo lock budget exceeded"));
    }
    write_input(&inputs.join("Cargo.lock"), &lock)?;
    let mut c = Command::new(&cargo);
    controlled(&mut c, &workspace, &home, &rustc);
    c.args([
        "build",
        "--offline",
        "--locked",
        "--lib",
        "--jobs",
        "1",
        "--message-format=json",
        "--target-dir",
    ])
    .arg(store.path(&r));
    let build = run(&mut c, &inputs.join("build"))?;
    #[cfg(test)]
    super::crash_tests::checkpoint("cargo_built");
    let mut library = None;
    for line in build.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let value: serde_json::Value = serde_json::from_slice(line).map_err(error)?;
        if value["reason"] == "compiler-artifact" {
            for name in value["filenames"]
                .as_array()
                .ok_or_else(|| error("Cargo filenames absent"))?
            {
                let path = PathBuf::from(
                    name.as_str()
                        .ok_or_else(|| error("invalid Cargo artifact"))?,
                );
                if path.extension().is_some_and(|e| e == "rlib") {
                    if !path.starts_with(store.path(&r)) {
                        return Err(error("external Cargo artifact"));
                    }
                    library = Some(path);
                }
            }
        }
    }
    // Cargo dep-info must name only the retained standalone source. An unseen
    // file/environment dependency invalidates closure instead of expanding authority.
    let target = store.path(&r);
    let built_tree = manifest(&r)?;
    let mut dep_count = 0;
    for (name, observed) in &built_tree {
        let p = target.join(name);
        if !observed.directory && p.extension().is_some_and(|e| e == "d") {
            dep_count += 1;
            let data = String::from_utf8(read_bounded(&p, 1024 * 1024)?)
                .map_err(error)?
                .replace("\\ ", " ");
            let source_name = workspace.join("src/lib.rs").to_string_lossy().into_owned();
            for line in data.lines().filter(|l| !l.is_empty()) {
                let (left, right) = line
                    .split_once(": ")
                    .or_else(|| line.strip_suffix(':').map(|l| (l, "")))
                    .ok_or_else(|| error("unsupported Cargo dependency record"))?;
                if !(left.starts_with(target.to_string_lossy().as_ref())
                    || left == source_name
                    || left == "src/lib.rs")
                    || (!right.is_empty() && right != source_name && right != "src/lib.rs")
                {
                    return Err(error(format!(
                        "external Cargo compiler input detected: {line:?}"
                    )));
                }
            }
        }
    }
    if dep_count == 0 {
        return Err(error("Cargo dependency evidence absent"));
    }
    let library = library.ok_or_else(|| error("Cargo library artifact missing"))?;
    // Export admission remains single-file and inode-independent, even when Cargo
    // internally hard-links its root/deps artifacts.
    let from = regular(&library)?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(store.path(&r).join("artifact.rlib"))?;
    if io::copy(&mut from.take(32 * 1024 * 1024 + 1), &mut out)? > 32 * 1024 * 1024 {
        return Err(error("Cargo library export budget exceeded"));
    }
    out.sync_all()?;
    if digest(&cargo)? != cargo_digest || digest(&rustc)? != rustc_digest {
        return Err(error("toolchain changed during Cargo production"));
    }
    let tree = manifest(&r)?;
    let mut retained = BTreeMap::new();
    for name in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        let path = inputs.join(name);
        retained.insert(path.clone(), entry(&path)?);
    }
    let outputs = tree
        .iter()
        .filter_map(|(p, e)| {
            if e.directory {
                None
            } else {
                Some((p.clone(), e.identity.clone()))
            }
        })
        .collect();
    r.cargo_proof = Some(CargoProof {
        schema: 1,
        contract: "standalone-source-v1".into(),
        inputs: retained,
        tree,
        cargo,
        rustc,
        cargo_digest,
        rustc_digest,
    });
    r.producer = Some(ProducerProof {
        input: inputs.join("src/lib.rs"),
        input_metadata: artifact_identity(&inputs.join("src/lib.rs"))?,
        compiler_version: "fixed standalone Cargo library extraction v1 (original package configuration not applied)".into(),
        outputs,
        closed_consumers: true,
    });
    for directory in [
        inputs.join("src"),
        inputs.clone(),
        inputs.parent().unwrap().to_owned(),
        store.root.clone(),
    ] {
        access::directory(&directory)?.sync_all()?;
    }
    r.sealed = r.producer.as_ref().map(|p| p.outputs.clone());
    r.last_meaningful_use = now();
    verify(&r)?;
    store.save(&r)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("gl-cargo-proof-{}", random().unwrap()));
            private_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn substituted_fifo_is_nonblocking_and_not_hashed_as_a_regular_file() {
        use std::os::unix::ffi::OsStrExt;
        let s = Scratch::new();
        let path = s.0.join("input");
        fs::write(&path, "original").unwrap();
        let _precheck = fs::symlink_metadata(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(read_bounded(&path, 1024).is_err());
        assert!(digest(&path).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn substituted_directory_does_not_follow_external_unique_data() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new();
        let root = s.0.join("root");
        let outside = s.0.join("outside");
        private_dir(&root).unwrap();
        private_dir(&root.join("sub")).unwrap();
        private_dir(&outside).unwrap();
        fs::write(outside.join("unique"), "keep").unwrap();
        let root_fd = access::directory(&root).unwrap();
        let original = relative(&root_fd, Path::new("sub")).unwrap();
        fs::rename(root.join("sub"), root.join("previous")).unwrap();
        symlink(&outside, root.join("sub")).unwrap();
        assert!(relative(&root_fd, Path::new("sub/unique")).is_err());
        assert!(names(&original).unwrap().is_empty());
        assert_eq!(fs::read_to_string(outside.join("unique")).unwrap(), "keep");
    }
    #[test]
    fn bounded_producer_failure_terminates_only_its_private_group() {
        let s = Scratch::new();
        let mut unrelated = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & echo $!; exit 1"]);
        let logs = s.0.join("failed");
        assert!(run_budget(&mut command, &logs, Duration::from_secs(2)).is_err());
        let pid: u32 = fs::read_to_string(logs.with_extension("stdout"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while process(pid).unwrap().is_some() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            process(pid).unwrap().is_none(),
            "owned orphan descendant must end"
        );
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "unrelated process is preserved"
        );
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
    #[test]
    fn producer_timeout_does_not_return_with_an_active_owned_child() {
        let s = Scratch::new();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & echo $!; wait"]);
        let logs = s.0.join("timeout");
        assert!(run_budget(&mut command, &logs, Duration::from_millis(100)).is_err());
        let pid: u32 = fs::read_to_string(logs.with_extension("stdout"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while process(pid).unwrap().is_some() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(process(pid).unwrap().is_none());
    }

    #[test]
    fn late_root_entry_invalidates_complete_manifest() {
        let s = Scratch::new();
        let m = fs::metadata(&s.0).unwrap();
        let sentinel = s.0.join("late-unique");
        let result = manifest_tree(&s.0, m.dev(), m.ino(), || {
            fs::write(&sentinel, "preserve late data")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "preserve late data");
    }

    #[test]
    fn enumeration_descriptor_is_atomically_close_on_exec() {
        use std::os::fd::AsRawFd;
        let s = Scratch::new();
        let dir = access::directory(&s.0).unwrap();
        let duplicate = cloexec_dup(&dir).unwrap();
        let fd = duplicate.as_raw_fd();
        assert_ne!(
            unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) } & nix::libc::FD_CLOEXEC,
            0
        );
        let status = Command::new("/bin/sh")
            .args(["-c", &format!("test ! -d /dev/fd/{fd}")])
            .status()
            .unwrap();
        assert!(status.success());
        assert!(names(&dir).unwrap().is_empty());
    }

    #[test]
    fn exited_leader_stays_unreaped_until_group_ownership_is_no_longer_needed() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "exit 0"]);
        unsafe {
            c.pre_exec(|| {
                if nix::libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = c.spawn().unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !leader_exited(&child).unwrap() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        // The zombie is still our unreaped child, reserving the numeric group id.
        // Darwin getpgid() returns ESRCH for a zombie despite WNOWAIT retaining
        // the waitable child. Repeated raw waitid, not getpgid, proves the fence.
        assert!(leader_exited(&child).unwrap());
        assert!(!group_running(child.id(), Some(child.id())).unwrap());
        assert!(child.wait().unwrap().success());
        assert!(
            leader_exited(&child).is_err(),
            "a reaped leader cannot authorize signalling"
        );
    }
}
