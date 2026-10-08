//! Constructed mount roots, never an unrestricted namespace-only spawn.
use super::process::ProcessConfig;
use crate::HostPolicy;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use zio_core::error::EvalError;

#[derive(Clone, Debug)]
pub struct ReadOnlyMount {
    pub source: PathBuf,
    pub target: PathBuf,
}

pub(crate) fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn quoted(path: &Path) -> Result<String, EvalError> {
    path.to_str()
        .map(quote)
        .ok_or_else(|| EvalError::custom("invalid-input: mount path must be UTF-8"))
}

pub(crate) fn command(
    config: &ProcessConfig,
    policy: &HostPolicy,
) -> Result<(Command, PathBuf), EvalError> {
    #[cfg(not(target_os = "linux"))]
    return Err(EvalError::custom(
        "capability-denied: process isolation requires Linux namespaces",
    ));
    let scratch = config.scratch.as_ref().ok_or_else(|| {
        EvalError::custom("capability-denied: isolated process requires explicit scratch")
    })?;
    let scratch = policy.write_path(scratch)?;
    if !absolute(&scratch)
        || !scratch.is_dir()
        || scratch == Path::new("/")
        || scratch == std::env::temp_dir()
    {
        return Err(EvalError::custom(
            "capability-denied: scratch must be an existing private absolute directory",
        ));
    }
    if config.mounts.is_empty() {
        return Err(EvalError::custom(
            "capability-denied: isolated process requires declared read-only mounts",
        ));
    }
    let checkout = std::env::current_dir().ok();
    let mut mounts = config.mounts.clone();
    mounts.sort_by_key(|mount| (mount.target.components().count(), mount.target.clone()));
    for (index, mount) in mounts.iter().enumerate() {
        if mount.target == Path::new("/")
            || ["/scratch", "/proc", "/dev", "/old"]
                .iter()
                .any(|reserved| mount.target.starts_with(reserved))
        {
            return Err(EvalError::custom(
                "capability-denied: mount target overlaps reserved jail path",
            ));
        }
        if !absolute(&mount.target) {
            return Err(EvalError::custom(
                "invalid-input: mount target must be normalized absolute path",
            ));
        }
        if mounts[..index]
            .iter()
            .any(|parent| mount.target.starts_with(&parent.target))
        {
            return Err(EvalError::custom(
                "capability-denied: read-only mount targets must not overlap",
            ));
        }
        if mount.source == Path::new("/")
            || !mount.source.is_dir()
            || scratch.starts_with(&mount.source)
            || mount.source.starts_with(&scratch)
        {
            return Err(EvalError::custom(
                "capability-denied: read-only source must be a separate declared directory",
            ));
        }
        if checkout
            .as_ref()
            .is_some_and(|cwd| cwd.starts_with(&mount.source))
            || mount.source.join(".git").exists()
            || mount.source.join("Cargo.toml").exists() && mount.source.join("apps").exists()
        {
            return Err(EvalError::custom(
                "capability-denied: checkout and its ancestors are not mount grants",
            ));
        }
    }
    if !absolute(&config.executable)
        || !mounts
            .iter()
            .any(|mount| config.executable.starts_with(&mount.target))
    {
        return Err(EvalError::custom(
            "capability-denied: executable must be within a declared jail mount",
        ));
    }
    if !absolute(&config.cwd)
        || !(config.cwd.starts_with("/scratch")
            || mounts
                .iter()
                .any(|mount| config.cwd.starts_with(&mount.target)))
    {
        return Err(EvalError::custom(
            "capability-denied: cwd must be within a declared jail mount or scratch",
        ));
    }
    // random names + create_dir, never rm an attacker-selected staging path.
    let mut random = [0u8; 16];
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").map_err(super::backend)?,
        &mut random,
    )
    .map_err(super::backend)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let stage = std::env::temp_dir().join(format!(".zio-jail-{suffix}"));
    if mounts.iter().any(|mount| stage.starts_with(&mount.source)) || stage.starts_with(&scratch) {
        return Err(EvalError::custom(
            "capability-denied: staging root overlaps a grant",
        ));
    }
    std::fs::create_dir(&stage).map_err(super::backend)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))
            .map_err(super::backend)?;
    }
    let root = stage.join("root");
    let rootq = quoted(&root)?;
    let mut script = format!(
        "set -eu; PATH=/usr/bin:/bin; export PATH; mount --make-rprivate /; mkdir -p {rootq}; mount --bind {rootq} {rootq}; mkdir -p {rootq}/old {rootq}/scratch {rootq}/proc {rootq}/dev; "
    );
    // Merged-/usr symlinks are recreated, never bound over /usr/lib.
    for name in ["bin", "sbin", "lib", "lib64"] {
        if let Ok(target) = std::fs::read_link(format!("/{name}")) {
            script.push_str(&format!("ln -s {} {rootq}/{name}; ", quoted(&target)?));
        }
    }
    for mount in &mounts {
        let target = root.join(mount.target.strip_prefix("/").map_err(super::backend)?);
        script.push_str(&format!(
            "mkdir -p {0}; mount --bind {1} {0}; mount -o remount,bind,ro,nosuid,nodev {0}; ",
            quoted(&target)?,
            quoted(&mount.source)?
        ));
    }
    // Individual harmless devices rather than exposing host /dev and its submounts.
    for device in ["null", "zero", "random", "urandom"] {
        script.push_str(&format!("touch {rootq}/dev/{device}; mount --bind /dev/{device} {rootq}/dev/{device}; mount -o remount,bind,ro,nosuid,noexec {rootq}/dev/{device}; "));
    }
    script.push_str(&format!("mount -t proc -o nosuid,nodev,noexec proc {rootq}/proc; mount --bind {} {rootq}/scratch; mount -o remount,bind,ro,nosuid,nodev {rootq}; mount -o remount,bind,rw,nosuid,nodev {rootq}/scratch; cd {rootq}; pivot_root . old; cd /; umount -l /old; ", quoted(&scratch)?));
    let argv = std::iter::once(config.executable.to_string_lossy().into_owned())
        .chain(config.argv.iter().cloned())
        .map(|arg| quote(&arg))
        .collect::<Vec<_>>()
        .join(" ");
    let environment = config
        .environment
        .iter()
        .map(|(key, value)| quote(&format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(" ");
    let inner = format!(
        "printf '%s\\n' '{{\"__zio_jail_ready\":true}}'; exec /usr/bin/env -i HOME=/scratch TMPDIR=/scratch PYTHONDONTWRITEBYTECODE=1 PYTHONNOUSERSITE=1 {environment} {argv}"
    );
    script.push_str(&format!("cd {}; export HOME=/scratch TMPDIR=/scratch PYTHONDONTWRITEBYTECODE=1 PYTHONNOUSERSITE=1; exec setpriv --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all -- /usr/bin/sh -c {};", quoted(&config.cwd)?, quote(&inner)));
    let mut command = Command::new("/usr/bin/unshare");
    command
        .args([
            "-Urnm",
            "--pid",
            "--fork",
            "--kill-child=KILL",
            "--",
            "/bin/sh",
            "-c",
        ])
        .arg(script);
    Ok((command, stage))
}
