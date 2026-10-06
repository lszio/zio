//! G00: the worker profile — denied by default, not a namespace label.
//!
//! A namespace only *enables* a boundary; it does not by itself remove
//! anything. What actually denies is the root the worker ends up in: the
//! profile is materialized into a minimal tree and entered with
//! `pivot_root`, so the worker's `/` contains exactly the read-only set
//! declared here plus one writable scratch. The store, the credentials,
//! the evaluator and the rest of the host filesystem are not in that
//! tree — they are not "protected by permissions", they are *absent*.
//!
//! Every missing boundary fails closed. There is no `--unsafe` mode: a
//! profile that cannot be completed is a refusal, not a degraded run.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::contracts::{Error, Result};

/// Where each declared entry is mounted inside the jail. The worker
/// script's *directory* comes along, because a file cannot be a bind
/// source and the worker imports its siblings.
/// Declared entries live under `/zio`, not under a runtime tree: `/lib`
/// is a symlink into `/usr/lib` on merged-`/usr` systems, so a bind onto
/// it replaces the directory and leaves no place to put a second mount.
pub const MOUNT_ROOT: &str = "/zio";
/// Sentinel source for a carried ancestor: an empty directory, created
/// in the staging tree and never bound from the host.
const MOUNT_CARRIED: &str = "";
pub const MOUNT_WORKER: &str = "/zio/worker";
pub const MOUNT_LIB: &str = "/zio/lib";
pub const MOUNT_STDLIB: &str = "/zio/stdlib";
pub const MOUNT_INPUT: &str = "/zio/input";

/// Top-level trees the runtime needs before the profile's own entries are
/// overlaid. Without a loader, a resolver, and `/proc`, CPython cannot
/// start at all — which would be a denial for the wrong reason.
const RUNTIME_TREES: &[&str] = &["/usr"];

/// On a merged-`/usr` system `/bin`, `/sbin`, `/lib` and `/lib64` are
/// symlinks into `/usr`. They are recreated as symlinks inside the jail
/// rather than bound: a bind onto a symlink path resolves to its target
/// and would land on top of the `/usr` mount the runtime already needs.
const USR_MERGE_LINKS: &[&str] = &["/bin", "/sbin", "/lib", "/lib64"];

/// Individual files inside a runtime tree the interpreter reads.
const RUNTIME_FILES: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/nsswitch.conf",
    "/etc/hosts",
    "/etc/resolv.conf",
    "/etc/localtime",
];

/// The runtime's device directory. It is bound wholesale with `--rbind`
/// because a char device is not a bind *target* a user namespace can
/// create (`mknod` is not permitted there), and torch's C extension needs
/// `/dev/urandom` to seed its RNG. `/dev` is a `devtmpfs` mount point, so
/// a plain bind of it does not work; the recursive bind does.

/// The read-only side of a profile: everything the worker may *see*.
///
/// Each entry appears in the worker's root read-only. Anything not listed
/// is not in that tree at all, so it cannot be read, written, or even
/// stat'ed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyRoot {
    /// The Python runtime prefix (`.venv`). Without it there is no worker.
    ///
    /// A venv's `bin/python` is normally a symlink into a separate
    /// interpreter installation, so the profile resolves it and mounts
    /// the real prefix as well — otherwise the jail has a dangling
    /// interpreter.
    pub python_prefix: PathBuf,
    /// Interpreter prefixes discovered by [`ReadOnlyRoot::with_resolved_interpreter`].
    /// Empty when the interpreter lives inside the declared prefix.
    pub interpreter_prefixes: Vec<PathBuf>,
    /// The worker script.
    pub worker_script: PathBuf,
    /// The frozen Zio libraries the worker may read.
    pub lib: PathBuf,
    /// The frozen Zio standard library.
    pub stdlib: PathBuf,
    /// Declared task inputs, one directory each.
    pub inputs: Vec<PathBuf>,
}

impl ReadOnlyRoot {
    /// Each declared entry with the jail-local path it is mounted at.
    ///
    /// Stable and explicit, so the host and the worker agree on where a
    /// declared input lives without either knowing the other's layout.
    pub fn entry_names(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
        if let Some(dir) = self.worker_script.parent() {
            out.push((PathBuf::from(MOUNT_WORKER), dir.to_path_buf()));
        }
        out.push((PathBuf::from(MOUNT_LIB), self.lib.clone()));
        out.push((PathBuf::from(MOUNT_STDLIB), self.stdlib.clone()));
        for (index, input) in self.inputs.iter().enumerate() {
            out.push((PathBuf::from(format!("{MOUNT_INPUT}{index}")), input.clone()));
        }
        out
    }

    /// Where the worker script is mounted inside the jail.
    pub fn declared_worker_script(&self) -> Option<PathBuf> {
        let declared = self.worker_script.parent()?;
        if !declared.exists() {
            return None;
        }
        let name = self
            .worker_script
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .unwrap_or_else(|| std::ffi::OsString::from("worker.py"));
        Some(PathBuf::from(MOUNT_WORKER).join(name))
    }

    /// Discover the interpreter the declared prefix actually execs.
    ///
    /// The symlink is read **literally**, not canonicalized: uv writes a
    /// link like `.../cpython-3.12-linux-x86_64-gnu/bin/python3.12` while
    /// the directory on disk is `cpython-3.12.14-...`. Following the link
    /// resolves the version and the link inside the jail would then dangle.
    pub fn with_resolved_interpreter(mut self) -> Self {
        let python = self.python_prefix.join("bin").join("python");
        let Ok(resolved) = std::fs::canonicalize(&python) else {
            return self;
        };
        // The interpreter installation is the directory holding `bin/`.
        let Some(prefix) = resolved.parent().and_then(Path::parent) else {
            return self;
        };
        // The literal link target wins when it exists on disk: it is what
        // `execve` will follow inside the jail.
        let literal = std::fs::read_link(&python)
            .ok()
            .and_then(|link| link.parent().and_then(Path::parent).map(Path::to_path_buf))
            .filter(|dir| dir.exists());
        if let Some(literal) = literal {
            if !self.interpreter_prefixes.iter().any(|p| p == &literal) {
                self.interpreter_prefixes.push(literal);
            }
        }
        if !prefix.starts_with(&self.python_prefix)
            && !self.interpreter_prefixes.iter().any(|p| p == prefix)
        {
            self.interpreter_prefixes.push(prefix.to_path_buf());
        }
        self
    }
}

/// A complete, enforceable worker boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationProfile {
    pub read_only: ReadOnlyRoot,
    /// The one writable directory, and the worker's cwd. A relative path
    /// is rejected: it would resolve against whatever the worker chose.
    pub scratch: PathBuf,
}

/// One entry materialized into the worker's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub source: PathBuf,
    /// Where it appears in the worker's root. Same as `source` unless
    /// remapped, which keeps the runtime where the interpreter expects it.
    pub target: PathBuf,
}

/// The concrete root, materialized for inspection and for the process
/// builder. A test can read this; the runtime builds exactly it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    /// The staging directory the root is assembled in.
    pub stage: PathBuf,
    pub read_only_bindings: Vec<Binding>,
    pub runtime_trees: Vec<PathBuf>,
    pub runtime_files: Vec<PathBuf>,
    pub scratch: PathBuf,
}

impl IsolationProfile {
    /// Resolve the interpreter the profile's venv actually execs.
    pub fn with_resolved_interpreter(mut self) -> Self {
        self.read_only = self.read_only.with_resolved_interpreter();
        self
    }

    /// Fail closed on any missing boundary. A profile that cannot be
    /// enforced must never be handed to a process.
    pub fn validate(&self) -> Result<()> {
        if !self.scratch.is_absolute() {
            return Err(Error::denied(format!(
                "worker scratch must be an absolute path, got {}",
                self.scratch.display()
            )));
        }
        if !self.scratch.is_dir() {
            return Err(Error::denied(format!(
                "worker scratch {} does not exist; a missing scratch is a missing boundary",
                self.scratch.display()
            )));
        }
        if self.scratch == Path::new("/") {
            return Err(Error::denied("worker scratch must not be the filesystem root"));
        }
        for (label, path) in [
            ("python prefix", &self.read_only.python_prefix),
            ("worker script", &self.read_only.worker_script),
            ("lib", &self.read_only.lib),
            ("stdlib", &self.read_only.stdlib),
        ] {
            if !path.exists() {
                return Err(Error::denied(format!(
                    "read-only {label} {} does not exist; a missing read-only mount is a missing boundary",
                    path.display()
                )));
            }
        }
        for prefix in &self.read_only.interpreter_prefixes {
            if !prefix.is_dir() {
                return Err(Error::denied(format!(
                    "resolved interpreter prefix {} does not exist",
                    prefix.display()
                )));
            }
        }
        for input in &self.read_only.inputs {
            if !input.is_dir() {
                return Err(Error::denied(format!(
                    "read-only input {} is not a directory",
                    input.display()
                )));
            }
            if *input == self.scratch {
                return Err(Error::denied(
                    "the scratch cannot also be a read-only input mount",
                ));
            }
        }
        Ok(())
    }

    /// The root this profile builds: what the worker will see, mapped to
    /// the host path it comes from.
    ///
    /// The target is the path *inside the jail*, which for everything but
    /// the Python runtime is a stable local name — the host's layout is
    /// not something the worker needs to know, and carrying it would
    /// expose whatever else lives alongside the declared entries.
    pub fn to_mount_spec(&self) -> MountSpec {
        let mut bindings: Vec<Binding> = vec![
            Binding {
                target: self.read_only.python_prefix.clone(),
                source: self.read_only.python_prefix.clone(),
            },
        ];
        for prefix in &self.read_only.interpreter_prefixes {
            bindings.push(Binding { target: prefix.clone(), source: prefix.clone() });
        }
        for (target, source) in self.read_only.entry_names() {
            bindings.push(Binding { target, source });
        }
        MountSpec {
            stage: PathBuf::from("/tmp/.grove-jail"),
            read_only_bindings: bindings,
            runtime_trees: RUNTIME_TREES.iter().map(PathBuf::from).collect(),
            runtime_files: RUNTIME_FILES.iter().map(PathBuf::from).collect(),
            scratch: self.scratch.clone(),
        }
    }

    /// Run `program` inside this profile and return its output.
    ///
    /// The child gets: its own user/network/pid/mount namespaces, a root
    /// containing only the declared read-only set plus one writable
    /// scratch, a fresh `/proc`, no inherited descriptors beyond stdio,
    /// and a cleared capability bounding set with `no_new_privs`.
    pub fn run<I, S>(
        &self,
        program: &Path,
        args: I,
        extra_args: &[OsString],
    ) -> Result<std::process::Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.validate()?;
        let shell = isolation_shell(self, program, args, extra_args)?;
        let mut command = Command::new("unshare");
        command
            // user (map root), network, pid, mount namespaces + fork
            .args(["-Urnm", "--pid", "--fork", "--"])
            .arg("/bin/sh")
            .arg("-c")
            .arg(shell)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output().map_err(|e| {
            Error::new(
                crate::contracts::ErrorKind::BackendFailed,
                format!("spawn isolated process: {e}"),
            )
        })
    }
}

/// The shell program the worker's own spawn path runs. Exposed so the
/// worker and a probe assemble the identical profile.
pub fn worker_shell(
    profile: &IsolationProfile,
    python: &Path,
    worker_script: &Path,
) -> Result<String> {
    isolation_shell(
        profile,
        python,
        [worker_script.display().to_string()],
        &[],
    )
}

/// A single-quoted argv element, so a path with a quote in it cannot
/// break out of the shell script.
/// Add `target`'s ancestor chain to `ordered`, shallowest first, so a
/// parent bind cannot hide the child mount points created after it.
fn chain(target: &Path, source: &Path, ordered: &mut BTreeMap<PathBuf, PathBuf>) {
    // `/` is already the new root; binding it would fail and gain nothing.
    //
    // Ancestors are carried as **empty directories**, not as binds of
    // themselves. Binding `/home/lszio/Projects/zio` because the venv
    // lives under it would hand the worker the entire checkout — the
    // store, the evaluator, the credentials — which is exactly what the
    // profile exists to prevent. An empty directory is enough for a
    // mount point.
    let mut ancestors: Vec<PathBuf> = target
        .ancestors()
        .skip(1)
        .take_while(|dir| *dir != Path::new("/"))
        .map(Path::to_path_buf)
        .collect();
    ancestors.reverse();
    for dir in ancestors {
        ordered.entry(dir).or_insert_with(|| PathBuf::from(MOUNT_CARRIED));
    }
    ordered.insert(target.to_path_buf(), source.to_path_buf());
}

fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Assemble the root, pivot into it, drop capabilities, then exec.
fn isolation_shell<I, S>(
    profile: &IsolationProfile,
    program: &Path,
    args: I,
    extra: &[OsString],
) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{

    let spec = profile.to_mount_spec();
    // The staging tree must not sit inside any granted directory: a
    // scratch under /tmp is itself mounted into the jail, and a stage
    // beside it would make the host's view of that path reachable from
    // inside. `/tmp/.grove-jail-*` keeps the two apart.
    // The stage is unique per invocation, not per process: two attempts
    // from one host (concurrent runs, a retry) would otherwise share a
    // staging tree, and the second's `rm -rf` would pull the mount points
    // out from under the first.
    static STAGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = STAGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stage = format!("/tmp/.grove-jail-{}-{seq}", std::process::id());
    let new_root = format!("{stage}/new");
    let reset = format!("rm -rf {stage} 2>/dev/null; mkdir -p {new_root}/old ");

    // Every target is created in the staging tree first, then bound. The
    // order is: create the root as a mount point (pivot_root needs that),
    // overlay the runtime trees, then the profile's own read-only set.
    let mut setup = reset.clone();

    // Ancestors first. Binding `/home/lszio` *replaces* the directory it
    // lands on, so a later bind onto `/home/lszio/Projects/zio/.venv` would
    // find no mount point there. Every entry therefore contributes its
    // whole ancestor chain, mounted shallowest first.
    //
    // The carried ancestors are created **empty**: each one is created
    // with `mkdir` and immediately overwritten by its own bind. Nothing
    // of the host's is ever visible through them, so an ancestor like
    // `/tmp` does not hand over whatever else lives there.
    // Declared entries are bound at a *jail-local* path, not at their host
    // path. Binding at the host path would require carrying every
    // intermediate directory, and a carried directory that the host also
    // owns as a mount point (a tmpfs /tmp) would hand over its whole
    // contents. A jail-local path has no such ancestor: `/inputs` needs
    // only `/`, which the new root already is.
    //
    // The one exception is the Python runtime, whose interpreter, shared
    // libraries, and `pyvenv.cfg` are compiled against absolute host
    // paths; those must keep them or the interpreter cannot start.
    let mut ordered: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for tree in &spec.runtime_trees {
        if tree.exists() {
            chain(tree, tree, &mut ordered);
        }
    }
    for binding in &spec.read_only_bindings {
        chain(&binding.target, &binding.source, &mut ordered);
    }
    // Every target's directory chain is created up front, in lexicographic
    // order so a parent is always made before its child. The child is
    // created *again* right after its parent is bound, because binding a
    // parent replaces the directory it lands on and would take the
    // child's empty mount point with it.
    // Mount order must be by depth, not lexicographic: `/home/lszio/.local`
    // sorts before `/home/lszio/Projects`, so a lexicographic pass binds
    // `.local` first and then replaces `/home/lszio` when `Projects`
    // lands — losing the first mount. Parents always bind first.
    let by_depth: Vec<&PathBuf> = {
        let mut keys: Vec<&PathBuf> = ordered.keys().collect();
        keys.sort_by_key(|p| (p.components().count(), (*p).clone()));
        keys
    };
    for target in &by_depth {
        setup.push_str(&format!("mkdir -p {}{} || exit 90; ", shq(&new_root), target.display()));
    }
    setup.push_str(&format!("mount --bind {new_root} {new_root} || exit 91; "));
    // Merged-`/usr` links are created before any bind, so `/lib` and
    // friends resolve through the `/usr` mount that comes next. A bind
    // onto a symlink path would resolve to its target and land on top of
    // that mount.
    for link in USR_MERGE_LINKS {
        if let Ok(target) = std::fs::read_link(link) {
            setup.push_str(&format!(
                "ln -sfn {} {}{}; ",
                shq(&target.display().to_string()),
                shq(&new_root),
                link
            ));
        }
    }
    for target in &by_depth {
        setup.push_str(&format!("mkdir -p {}{}; ", shq(&new_root), target.display()));
        let Some(source) = ordered.get(*target) else { continue };
        if source.as_os_str().is_empty() {
            // A carried ancestor: the empty directory the mkdir made is
            // the whole point — it carries a mount without carrying the
            // host directory that holds it.
            continue;
        }
        // A declared entry that cannot be bound is a failed profile, not
        // a directory that quietly ends up empty: fail closed.
        setup.push_str(&format!(
            "mount --bind {} {}{} || exit 92; ",
            shq(&source.display().to_string()),
            shq(&new_root),
            target.display()
        ));
    }
    // Only the declared leaves are read-only; the ancestor directories
    // exist purely to carry them, and are empty otherwise.
    for file in &spec.runtime_files {
        if file.exists() {
            setup.push_str(&format!(
                "touch {}{} 2>/dev/null; mount --bind {} {}{} 2>/dev/null; ",
                shq(&new_root),
                file.display(),
                shq(&file.display().to_string()),
                shq(&new_root),
                file.display()
            ));
        }
    }
    // A minimal /dev, bound node by node: the host's /dev cannot be bound
    // wholesale (it is a devtmpfs mount point, not a plain directory).
    setup.push_str(&format!(
        "mkdir -p {new_root}/dev; mount --rbind /dev {new_root}/dev 2>/dev/null; "
    ));
    // A fresh /proc: the worker's view of processes is its own namespace.
    setup.push_str(&format!("mkdir -p {new_root}/proc; mount -t proc proc {new_root}/proc 2>/dev/null; "));
    // The one writable path. It is bind-mounted at a path *inside* the
    // jail, not at its own absolute location: a scratch under a carried
    // ancestor (/tmp) would otherwise drag that ancestor's whole contents
    // into the root, which is how a canary or a credential directory
    // sitting next to it becomes reachable.
    let scratch_mount = format!("{stage}/scratch");
    setup.push_str(&format!(
        "mkdir -p {new_root}/scratch {scratch_mount} && \
         mount --bind {} {new_root}/scratch || exit 93; ",
        shq(&profile.scratch.display().to_string()),
    ));
    // The whole root read-only, then the scratch reopened. Remounting the
    // root itself is what makes an undeclared path unwritable: per-entry
    // remounts would leave a directory the profile forgot still open, and
    // the process is uid 0 inside its user namespace, so file
    // permissions alone deny nothing.
    setup.push_str(&format!(
        "mount -o remount,bind,ro {new_root} || exit 95; \
         mount -o remount,bind,rw {new_root}/scratch || exit 96; "
    ));
    // Enter the new root. After this, every path above is unreachable.
    setup.push_str(&format!(
        "cd {new_root} && pivot_root . old || exit 94; cd /; umount -l /old 2>/dev/null; "
    ));

    // Inherited descriptors: keep stdio and nothing else, so a leaked
    // store lock or open database cannot be read back.
    let close_fds = "for fd in /proc/self/fd/*; do n=${fd##*/}; \
        case \"$n\" in 0|1|2) ;; *) eval \"exec $n>&-\" ;; esac; done; ";

    // The venv's `bin/python` is exec'd **as given**, not resolved: the
    // resolution would bypass `pyvenv.cfg` and run the base interpreter
    // with no `site-packages`, which is a subtly different Python than
    // the one the profile declared. The symlink target is mounted, so
    // `execve` can follow it.
    let argv: Vec<String> = std::iter::once(program.display().to_string())
        .chain(args.into_iter().map(|a| a.as_ref().to_string_lossy().into_owned()))
        .map(|a| shq(&a))
        .collect();
    let command_line = argv.join(" ");
    let extra_args = extra
        .iter()
        .map(|a| format!("{} ", shq(&a.to_string_lossy())))
        .collect::<String>();

    // Capability drop: bounding set and inheritable/ambient sets cleared,
    // no_new_privs set. `--clear-groups` is deliberately omitted — inside
    // a user namespace it needs CAP_SETGID, and including it would abort
    // the whole drop rather than the group clear. Capabilities are the
    // boundary that matters; supplementary groups are not granted
    // anything the bounding set still allows.
    let drop_caps = "setpriv --no-new-privs --bounding-set=-all \
        --inh-caps=-all --ambient-caps=-all -- ";

    Ok(format!(
        "{setup}\
         {close_fds}\
         cd /scratch; \
         HOME=/scratch TMPDIR=/scratch PYTHONDONTWRITEBYTECODE=1 PYTHONNOUSERSITE=1; \
         export HOME TMPDIR PYTHONDONTWRITEBYTECODE PYTHONNOUSERSITE; \
         {drop_caps}{extra_args}{command_line}"
    ))
}
