//! Core-only artifact access does not hand source paths or descriptors to consumers.
use super::*;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Component;

fn open_at(parent: &File, name: &std::ffi::OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(error)?;
    // The parent descriptor anchors resolution even if its pathname is replaced.
    let fd = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | nix::libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(error("absolute export destination required"));
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                current = open_at(
                    &current,
                    name,
                    nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW,
                    0,
                )?;
                let metadata = current.metadata()?;
                if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
                    return Err(error("writable export ancestry refused"));
                }
            }
            _ => return Err(error("noncanonical export ancestry refused")),
        }
    }
    Ok(current)
}

pub(super) fn export(
    store: &Store,
    id: &str,
    generation: &str,
    artifact: &str,
    destination: &Path,
) -> io::Result<u64> {
    if !destination.is_absolute()
        || destination.starts_with(&store.root)
        || artifact.is_empty()
        || Path::new(artifact).components().count() != 1
        || !matches!(
            Path::new(artifact).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(error(
            "exact artifact and external absolute destination required",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| error("destination parent missing"))?;
    let parent_handle = directory(parent)?;
    if parent_handle.metadata()?.uid() != unsafe { nix::libc::getuid() } {
        return Err(error("export parent is not user-owned"));
    }
    let global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let barrier = store.barrier(id, false)?;
    let metadata = store.metadata_barrier(id)?;
    let mut record = store.read(id)?;
    if record.generation != generation || record.retirement_started || record.reclaimed {
        return Err(error("stale or retiring export generation"));
    }
    store.identity(&record)?;
    verify_producer(&record)?;
    if record.sealed.as_ref() != record.producer.as_ref().map(|p| &p.outputs)
        || artifacts(&store.path(&record), record.dev)? != record.producer.as_ref().unwrap().outputs
    {
        return Err(error("export requires unchanged sealed producer outputs"));
    }
    source_revision(&record.source_root, Some(&record.source_head))?;
    let expected = record
        .producer
        .as_ref()
        .unwrap()
        .outputs
        .get(artifact)
        .ok_or_else(|| error("artifact outside producer manifest"))?
        .clone();
    if expected.len > 32 * 1024 * 1024 || record.read_claims.len() >= 256 {
        return Err(error("bounded export capability exceeded"));
    }
    let source_directory = directory(&store.path(&record))?;
    let directory_metadata = source_directory.metadata()?;
    if directory_metadata.dev() != record.dev || directory_metadata.ino() != record.ino {
        return Err(error("export source directory replaced"));
    }
    let mut input = open_at(
        &source_directory,
        std::ffi::OsStr::new(artifact),
        nix::libc::O_RDONLY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
    )?;
    validate_open_artifact(&input, &expected)?;
    let token = random()?;
    record.read_claims.push(ReadClaim {
        token: token.clone(),
        holder: process(std::process::id())?
            .ok_or_else(|| error("core access identity unavailable"))?,
        artifact: artifact.into(),
        ended: false,
        exported: false,
    });
    record.last_meaningful_use = now();
    store.save(&record)?;
    drop(metadata);
    drop(global);
    #[cfg(test)]
    super::crash_tests::checkpoint("access");
    let mut output = open_at(
        &parent_handle,
        destination
            .file_name()
            .ok_or_else(|| error("destination filename missing"))?,
        nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL | nix::libc::O_NOFOLLOW,
        0o600,
    )?;
    let mut bytes = 0u64;
    let mut buffer = [0u8; 16384];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > expected.len {
            return Err(error(
                "source grew during export; partial destination preserved",
            ));
        }
        output.write_all(&buffer[..count])?;
        #[cfg(test)]
        super::crash_tests::checkpoint("partial_export");
    }
    if bytes != expected.len {
        return Err(error(
            "source allocation changed during export; partial destination preserved",
        ));
    }
    validate_open_artifact(&input, &expected)?;
    output.sync_all()?;
    parent_handle.sync_all()?;
    drop(input);
    drop(source_directory);
    drop(output);
    let _metadata = store.metadata_barrier(id)?;
    let mut fresh = store.read(id)?;
    if fresh.generation != generation {
        return Err(error("export completion generation changed"));
    }
    let claim = fresh
        .read_claims
        .iter_mut()
        .find(|c| c.token == token && !c.ended)
        .ok_or_else(|| error("core export claim changed"))?;
    claim.ended = true;
    claim.exported = true;
    fresh.last_meaningful_use = now();
    store.save(&fresh)?;
    drop(barrier);
    Ok(bytes)
}

pub(super) fn validate_open_artifact(file: &File, expected: &Artifact) -> io::Result<()> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.nlink() != 1
        || m.mode() & 0o111 != 0
        || m.dev() != expected.dev
        || m.ino() != expected.ino
        || m.len() != expected.len
        || m.mtime() != expected.mtime
        || m.mtime_nsec() != expected.mtime_nsec
    {
        Err(error("opened producer artifact identity changed"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_descriptors_do_not_escape_to_identity_probe_children() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("gl-access-{}", random().unwrap()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("owned.o"), b"public fixture").unwrap();
        let parent = directory(&root).unwrap();
        let input = open_at(
            &parent,
            std::ffi::OsStr::new("owned.o"),
            nix::libc::O_RDONLY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
        )
        .unwrap();
        for fd in [parent.as_raw_fd(), input.as_raw_fd()] {
            assert_ne!(
                unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) } & nix::libc::FD_CLOEXEC,
                0
            );
        }
        let metadata = input.metadata().unwrap();
        let output = Command::new("python3").env("PATH", env!("PATH")).args([
            "-c", "import os,sys\nwant=tuple(map(int,sys.argv[1:]))\nfor fd in range(256):\n try: s=os.fstat(fd)\n except OSError: continue\n assert (s.st_dev,s.st_ino)!=want, 'source descriptor escaped'",
            &metadata.dev().to_string(), &metadata.ino().to_string()]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        drop(input);
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn substituted_fifo_is_opened_nonblocking_then_refused() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("gl-fifo-{}", random().unwrap()));
        fs::create_dir(&root).unwrap();
        let path = root.join("artifact.o");
        fs::write(&path, b"public fixture").unwrap();
        let expected = artifact_identity(&path).unwrap();
        let precheck = fs::metadata(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let parent = directory(&root).unwrap();
        let input = open_at(
            &parent,
            std::ffi::OsStr::new("artifact.o"),
            nix::libc::O_RDONLY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
        )
        .unwrap();
        assert_ne!(
            unsafe { nix::libc::fcntl(input.as_raw_fd(), nix::libc::F_GETFL) }
                & nix::libc::O_NONBLOCK,
            0
        );
        assert!(validate_open_artifact(&input, &expected).is_err());
        // The shared header reader receives the previously verified regular
        // file metadata, then encounters the substituted FIFO at open time.
        assert!(artifact_header(&path, &precheck).is_err());
        assert!(artifacts(&root, expected.dev).is_err());
        drop(input);
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }
}
