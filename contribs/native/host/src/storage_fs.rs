//! Directory descriptors, not a check-then-open pathname, carry authority.
use crate::HostPolicy;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use zio_core::error::EvalError;

fn io_error(operation: &str, error: std::io::Error) -> EvalError {
    let class = match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR | libc::EACCES | libc::EPERM) => "capability-denied",
        Some(libc::ENOENT) => "artifact-unavailable",
        _ => "backend-failed",
    };
    EvalError::custom(format!("{class}: {operation}: {error}"))
}
pub(super) fn os_error(operation: &str) -> EvalError {
    io_error(operation, std::io::Error::last_os_error())
}
pub(super) fn cstring(name: &OsStr) -> Result<CString, EvalError> {
    CString::new(name.as_bytes()).map_err(|_| EvalError::custom("invalid-input: path contains NUL"))
}
pub(super) fn open_at(parent: &File, name: &CStr, flags: i32) -> Result<File, EvalError> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        Err(os_error("open authorized path"))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn open_absolute_directory(path: &Path) -> Result<File, EvalError> {
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(os_error("open filesystem root"));
    }
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = open_at(
                    &directory,
                    &cstring(name)?,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                )?
            }
            _ => {
                return Err(EvalError::custom(
                    "invalid-input: capability root must be an absolute canonical directory",
                ));
            }
        }
    }
    Ok(directory)
}
struct Root {
    path: PathBuf,
    directory: Arc<File>,
    writable: bool,
}
pub(super) struct Authority {
    roots: Vec<Root>,
    cwd: PathBuf,
}
pub(super) struct Entry {
    pub directory: File,
    pub name: CString,
    pub path: PathBuf,
}
impl Entry {
    pub fn open(&self, flags: i32) -> Result<File, EvalError> {
        open_at(&self.directory, &self.name, flags)
    }
    pub fn metadata(&self) -> Result<Option<libc::stat>, EvalError> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOENT) {
                Ok(None)
            } else {
                Err(io_error("stat authorized path", error))
            };
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(EvalError::custom("capability-denied: symbolic-link target"));
        }
        Ok(Some(stat))
    }
    pub fn remove(&self, sync: bool) -> Result<bool, EvalError> {
        let Some(stat) = self.metadata()? else {
            return Ok(false);
        };
        let flags = if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            libc::AT_REMOVEDIR
        } else {
            0
        };
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), flags) } < 0 {
            return Err(os_error("remove authorized path"));
        }
        if sync {
            self.directory
                .sync_all()
                .map_err(|e| io_error("fsync removed entry directory", e))?;
        }
        Ok(true)
    }
}
impl Authority {
    pub fn capture(policy: &HostPolicy) -> Result<Self, EvalError> {
        let mut roots = Vec::new();
        for (paths, writable) in [(&policy.read_roots, false), (&policy.write_roots, true)] {
            for path in paths {
                roots.push(Root {
                    path: path.clone(),
                    directory: Arc::new(open_absolute_directory(path)?),
                    writable,
                });
            }
        }
        roots.sort_by_key(|r| std::cmp::Reverse((r.path.components().count(), r.writable)));
        Ok(Self {
            roots,
            cwd: std::env::current_dir().map_err(|e| io_error("capture cwd", e))?,
        })
    }
    pub fn absolute(&self, path: &Path) -> Result<PathBuf, EvalError> {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let mut result = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::RootDir | Component::Normal(_) => result.push(component.as_os_str()),
                Component::CurDir => {}
                _ => {
                    return Err(EvalError::custom(
                        "capability-denied: parent traversal is not allowed",
                    ));
                }
            }
        }
        Ok(result)
    }
    pub fn writable(&self, path: &Path) -> Result<bool, EvalError> {
        let path = self.absolute(path)?;
        Ok(self
            .roots
            .iter()
            .any(|r| r.writable && path.starts_with(&r.path)))
    }
    pub fn entry(
        &self,
        path: &Path,
        write: bool,
        create_parents: bool,
    ) -> Result<Entry, EvalError> {
        let path = self.absolute(path)?;
        let root = self
            .roots
            .iter()
            .find(|r| (!write || r.writable) && path.starts_with(&r.path))
            .ok_or_else(|| {
                EvalError::custom(format!(
                    "capability-denied: {} {}",
                    if write { "write" } else { "read" },
                    path.display()
                ))
            })?;
        let relative = path
            .strip_prefix(&root.path)
            .map_err(|_| EvalError::custom("capability-denied: foreign root"))?;
        let mut directory = root
            .directory
            .try_clone()
            .map_err(|e| io_error("duplicate capability directory", e))?;
        let components: Vec<_> = relative.components().collect();
        if components.is_empty() {
            return Ok(Entry {
                directory,
                name: CString::new(".").unwrap(),
                path,
            });
        }
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                return Err(EvalError::custom(
                    "capability-denied: invalid path component",
                ));
            };
            let name = cstring(name)?;
            if create_parents {
                if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } < 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    return Err(os_error("create authorized parent"));
                }
                directory
                    .sync_all()
                    .map_err(|e| io_error("fsync parent directory", e))?;
            }
            directory = open_at(&directory, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        let Component::Normal(name) = components[components.len() - 1] else {
            return Err(EvalError::custom("capability-denied: invalid filename"));
        };
        Ok(Entry {
            directory,
            name: cstring(name)?,
            path,
        })
    }
    pub fn read(&self, path: &Path) -> Result<Vec<u8>, EvalError> {
        let entry = self.entry(path, false, false)?;
        let mut file = entry.open(libc::O_RDONLY | libc::O_NONBLOCK)?;
        if !file
            .metadata()
            .map_err(|e| io_error("stat input", e))?
            .is_file()
        {
            return Err(EvalError::custom("invalid-input: expected regular file"));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io_error("read bytes", e))?;
        Ok(bytes)
    }
    pub fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), EvalError> {
        let entry = self.entry(path, true, true)?;
        if entry.name.to_bytes() == b"." {
            return Err(EvalError::custom(
                "invalid-input: cannot replace capability root",
            ));
        }
        if entry
            .metadata()?
            .is_some_and(|s| s.st_mode & libc::S_IFMT != libc::S_IFREG)
        {
            return Err(EvalError::custom(
                "invalid-input: atomic target must be a regular file",
            ));
        }
        let mut random = [0u8; 16];
        random_fill(&mut random)?;
        let name = CString::new(format!(".zio-atomic-{}", hex(&random))).unwrap();
        let result = (|| {
            let mut file = open_at(
                &entry.directory,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
            file.write_all(bytes)
                .map_err(|e| io_error("write temporary bytes", e))?;
            file.sync_all()
                .map_err(|e| io_error("fsync temporary bytes", e))?;
            if unsafe {
                libc::renameat(
                    entry.directory.as_raw_fd(),
                    name.as_ptr(),
                    entry.directory.as_raw_fd(),
                    entry.name.as_ptr(),
                )
            } < 0
            {
                return Err(os_error("commit atomic bytes"));
            }
            entry
                .directory
                .sync_all()
                .map_err(|e| io_error("fsync atomic directory", e))
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(entry.directory.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        result
    }
    pub fn mkdir(&self, path: &Path) -> Result<(), EvalError> {
        let entry = self.entry(path, true, true)?;
        if unsafe { libc::mkdirat(entry.directory.as_raw_fd(), entry.name.as_ptr(), 0o700) } < 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
        {
            return Err(os_error("mkdir"));
        }
        entry
            .open(libc::O_RDONLY | libc::O_DIRECTORY)?
            .sync_all()
            .map_err(|e| io_error("fsync created directory", e))?;
        entry
            .directory
            .sync_all()
            .map_err(|e| io_error("fsync mkdir parent", e))
    }
    pub fn list(&self, path: &Path) -> Result<Vec<String>, EvalError> {
        let entry = self.entry(path, false, false)?;
        let directory = entry.open(libc::O_RDONLY | libc::O_DIRECTORY)?;
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(os_error("duplicate listing directory"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(os_error("list directory"));
        }
        let mut names = Vec::new();
        let result = loop {
            unsafe {
                *libc::__errno_location() = 0;
            }
            let item = unsafe { libc::readdir(stream) };
            if item.is_null() {
                break if std::io::Error::last_os_error().raw_os_error() == Some(0) {
                    Ok(())
                } else {
                    Err(os_error("read directory entry"))
                };
            }
            let bytes = unsafe { CStr::from_ptr((*item).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            match std::str::from_utf8(bytes) {
                Ok(name) => names.push(name.to_owned()),
                Err(_) => {
                    break Err(EvalError::custom(
                        "invalid-input: directory entry is not UTF-8",
                    ));
                }
            }
        };
        unsafe {
            libc::closedir(stream);
        }
        result?;
        names.sort();
        Ok(names)
    }
    pub fn rename(&self, from: &Path, to: &Path) -> Result<(), EvalError> {
        let from = self.entry(from, true, false)?;
        let to = self.entry(to, true, false)?;
        if from.name.to_bytes() == b"." || to.name.to_bytes() == b"." {
            return Err(EvalError::custom(
                "capability-denied: cannot rename capability root",
            ));
        }
        from.metadata()?
            .ok_or_else(|| EvalError::custom("artifact-unavailable: rename source"))?;
        to.metadata()?;
        if unsafe {
            libc::renameat(
                from.directory.as_raw_fd(),
                from.name.as_ptr(),
                to.directory.as_raw_fd(),
                to.name.as_ptr(),
            )
        } < 0
        {
            return Err(os_error("rename"));
        }
        from.directory
            .sync_all()
            .map_err(|e| io_error("fsync rename source", e))?;
        to.directory
            .sync_all()
            .map_err(|e| io_error("fsync rename destination", e))
    }
    pub fn canonical(&self, path: &Path) -> Result<PathBuf, EvalError> {
        let entry = self.entry(path, false, false)?;
        let file = entry.open(libc::O_PATH)?;
        let resolved = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|e| io_error("canonical descriptor path", e))?;
        Ok(resolved)
    }
    pub fn temporary(&self) -> Result<Entry, EvalError> {
        let root =
            self.roots.iter().find(|r| r.writable).ok_or_else(|| {
                EvalError::custom("capability-denied: no writable temporary root")
            })?;
        let mut random = [0u8; 16];
        random_fill(&mut random)?;
        self.entry(
            &root.path.join(format!(".zio-sqlite-{}", hex(&random))),
            true,
            false,
        )
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 15) as usize] as char);
    }
    text
}
pub(super) fn random_fill(mut bytes: &mut [u8]) -> Result<(), EvalError> {
    while !bytes.is_empty() {
        let count = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(os_error("CSPRNG"));
        }
        if count == 0 {
            return Err(EvalError::custom(
                "backend-failed: CSPRNG returned no bytes",
            ));
        }
        bytes = &mut bytes[count as usize..];
    }
    Ok(())
}
