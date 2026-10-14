//! SQLite file, journal, WAL, shared-memory and temporary I/O use the same
//! held-directory authority as host filesystem calls. No default-VFS escape.
use super::filesystem::{self, Authority, Entry};
use rusqlite::ffi;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use zio_core::error::EvalError;

pub(super) struct Vfs {
    definition: Box<ffi::sqlite3_vfs>,
    name: CString,
    _authority: Arc<Authority>,
}
impl Vfs {
    pub fn new(authority: Arc<Authority>) -> Result<Self, EvalError> {
        let mut random = [0u8; 16];
        filesystem::random_fill(&mut random)?;
        let name = CString::new(format!("zio-capability-{}", filesystem::hex(&random))).unwrap();
        let mut definition: Box<ffi::sqlite3_vfs> = Box::new(unsafe { std::mem::zeroed() });
        definition.iVersion = 2;
        definition.szOsFile = std::mem::size_of::<SqlFile>() as i32;
        definition.mxPathname = 4096;
        definition.zName = name.as_ptr();
        definition.pAppData = Arc::as_ptr(&authority).cast_mut().cast();
        definition.xOpen = Some(open);
        definition.xDelete = Some(delete);
        definition.xAccess = Some(access);
        definition.xFullPathname = Some(full_path);
        definition.xRandomness = Some(randomness);
        definition.xSleep = Some(sleep);
        definition.xCurrentTime = Some(current_time);
        definition.xCurrentTimeInt64 = Some(current_time_ms);
        definition.xGetLastError = Some(last_error);
        if unsafe { ffi::sqlite3_vfs_register(&mut *definition, 0) } != ffi::SQLITE_OK {
            return Err(EvalError::custom(
                "backend-failed: register capability SQLite VFS",
            ));
        }
        Ok(Self {
            definition,
            name,
            _authority: authority,
        })
    }
    pub fn name(&self) -> &str {
        self.name.to_str().unwrap()
    }
}
impl Drop for Vfs {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_vfs_unregister(&mut *self.definition);
        }
    }
}
#[repr(C)]
struct SqlFile {
    base: ffi::sqlite3_file,
    state: *mut FileState,
}
struct FileState {
    file: File,
    entry: Entry,
    lock: i32,
    delete_on_close: bool,
    writable: bool,
    shm: Option<SharedMemory>,
}
struct SharedMemory {
    file: File,
    name: CString,
    regions: Vec<(*mut libc::c_void, usize)>,
}
impl Drop for SharedMemory {
    fn drop(&mut self) {
        for (address, size) in self.regions.drain(..) {
            if !address.is_null() {
                unsafe {
                    libc::munmap(address, size);
                }
            }
        }
    }
}
unsafe fn state<'a>(file: *mut ffi::sqlite3_file) -> &'a mut FileState {
    unsafe { &mut *(*(file.cast::<SqlFile>())).state }
}
unsafe fn authority<'a>(vfs: *mut ffi::sqlite3_vfs) -> &'a Authority {
    unsafe { &*((*vfs).pAppData.cast::<Authority>()) }
}
unsafe fn path<'a>(name: *const libc::c_char) -> Option<&'a Path> {
    if name.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(name) }.to_str().ok().map(Path::new)
}
unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const libc::c_char,
    file: *mut ffi::sqlite3_file,
    flags: i32,
    out: *mut i32,
) -> i32 {
    unsafe {
        (*file).pMethods = ptr::null();
    }
    let authority = unsafe { authority(vfs) };
    let writable = flags & ffi::SQLITE_OPEN_READWRITE != 0;
    let entry = if name.is_null() {
        authority.temporary()
    } else {
        let Some(path) = (unsafe { path(name) }) else {
            return ffi::SQLITE_CANTOPEN;
        };
        authority.entry(path, writable, false)
    };
    let Ok(entry) = entry else {
        return ffi::SQLITE_CANTOPEN;
    };
    let mut os_flags = if writable {
        libc::O_RDWR
    } else {
        libc::O_RDONLY
    };
    if flags & ffi::SQLITE_OPEN_CREATE != 0 {
        os_flags |= libc::O_CREAT;
    }
    if flags & ffi::SQLITE_OPEN_EXCLUSIVE != 0 || name.is_null() {
        os_flags |= libc::O_EXCL;
    }
    let Ok(opened) = entry.open(os_flags | libc::O_NONBLOCK) else {
        return ffi::SQLITE_CANTOPEN;
    };
    if !opened.metadata().is_ok_and(|s| s.is_file()) {
        return ffi::SQLITE_CANTOPEN;
    }
    let state = Box::new(FileState {
        file: opened,
        entry,
        lock: ffi::SQLITE_LOCK_NONE,
        delete_on_close: flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 || name.is_null(),
        writable,
        shm: None,
    });
    unsafe {
        ptr::write(
            file.cast::<SqlFile>(),
            SqlFile {
                base: ffi::sqlite3_file { pMethods: &METHODS },
                state: Box::into_raw(state),
            },
        );
        if !out.is_null() {
            *out = flags;
        }
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> i32 {
    unsafe {
        shm_unmap(file, 0);
    }
    let state = unsafe { Box::from_raw((*(file.cast::<SqlFile>())).state) };
    let result = if state.delete_on_close {
        state.entry.remove(true).map(|_| ())
    } else {
        Ok(())
    };
    drop(state);
    unsafe {
        (*file).pMethods = ptr::null();
    }
    if result.is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_CLOSE
    }
}
unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    output: *mut libc::c_void,
    amount: i32,
    offset: i64,
) -> i32 {
    if amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(output.cast::<u8>(), amount as usize) };
    let file = &unsafe { state(file) }.file;
    let mut used = 0;
    while used < bytes.len() {
        match file.read_at(&mut bytes[used..], offset as u64 + used as u64) {
            Ok(0) => {
                bytes[used..].fill(0);
                return ffi::SQLITE_IOERR_SHORT_READ;
            }
            Ok(n) => used += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return ffi::SQLITE_IOERR_READ,
        }
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    input: *const libc::c_void,
    amount: i32,
    offset: i64,
) -> i32 {
    if amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_WRITE;
    }
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), amount as usize) };
    if unsafe { state(file) }
        .file
        .write_all_at(bytes, offset as u64)
        .is_ok()
    {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_WRITE
    }
}
unsafe extern "C" fn truncate(file: *mut ffi::sqlite3_file, size: i64) -> i32 {
    if size >= 0 && unsafe { state(file) }.file.set_len(size as u64).is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_TRUNCATE
    }
}
unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, _flags: i32) -> i32 {
    let state = unsafe { state(file) };
    if state.file.sync_all().is_err() {
        return ffi::SQLITE_IOERR_FSYNC;
    }
    if state.entry.directory.sync_all().is_err() {
        return ffi::SQLITE_IOERR_DIR_FSYNC;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn size(file: *mut ffi::sqlite3_file, output: *mut i64) -> i32 {
    match unsafe { state(file) }.file.metadata() {
        Ok(metadata) => {
            unsafe {
                *output = metadata.len() as i64;
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_IOERR_FSTAT,
    }
}
const PENDING: i64 = 0x40000000;
const RESERVED: i64 = PENDING + 1;
const SHARED: i64 = PENDING + 2;
fn range_lock(file: &File, kind: i16, start: i64, len: i64) -> i32 {
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as i16;
    lock.l_start = start;
    lock.l_len = len;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } == 0 {
        ffi::SQLITE_OK
    } else if matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EAGAIN | libc::EACCES)
    ) {
        ffi::SQLITE_BUSY
    } else {
        ffi::SQLITE_IOERR_LOCK
    }
}
unsafe extern "C" fn lock(file: *mut ffi::sqlite3_file, requested: i32) -> i32 {
    let state = unsafe { state(file) };
    if state.lock >= requested {
        return ffi::SQLITE_OK;
    }
    if state.lock == ffi::SQLITE_LOCK_NONE {
        let result = range_lock(&state.file, libc::F_RDLCK as i16, PENDING, 1);
        if result != ffi::SQLITE_OK {
            return result;
        }
        let result = range_lock(&state.file, libc::F_RDLCK as i16, SHARED, 510);
        range_lock(&state.file, libc::F_UNLCK as i16, PENDING, 1);
        if result != ffi::SQLITE_OK {
            return result;
        }
        state.lock = ffi::SQLITE_LOCK_SHARED;
    }
    if requested == ffi::SQLITE_LOCK_RESERVED {
        let result = range_lock(&state.file, libc::F_WRLCK as i16, RESERVED, 1);
        if result == ffi::SQLITE_OK {
            state.lock = requested;
        }
        return result;
    }
    if requested >= ffi::SQLITE_LOCK_PENDING {
        let result = range_lock(&state.file, libc::F_WRLCK as i16, PENDING, 1);
        if result != ffi::SQLITE_OK {
            return result;
        }
        state.lock = ffi::SQLITE_LOCK_PENDING;
    }
    if requested == ffi::SQLITE_LOCK_EXCLUSIVE {
        let result = range_lock(&state.file, libc::F_WRLCK as i16, SHARED, 510);
        if result == ffi::SQLITE_OK {
            state.lock = requested;
        }
        return result;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn unlock(file: *mut ffi::sqlite3_file, requested: i32) -> i32 {
    let state = unsafe { state(file) };
    let mut result = ffi::SQLITE_OK;
    if requested == ffi::SQLITE_LOCK_NONE {
        result = range_lock(&state.file, libc::F_UNLCK as i16, PENDING, 512);
    } else if requested == ffi::SQLITE_LOCK_SHARED {
        for (kind, start, len) in [
            (libc::F_RDLCK as i16, SHARED, 510),
            (libc::F_UNLCK as i16, PENDING, 2),
        ] {
            let rc = range_lock(&state.file, kind, start, len);
            if rc != ffi::SQLITE_OK {
                result = rc;
            }
        }
    }
    if result == ffi::SQLITE_OK {
        state.lock = requested;
    }
    if result == ffi::SQLITE_BUSY {
        ffi::SQLITE_IOERR_UNLOCK
    } else {
        result
    }
}
unsafe extern "C" fn reserved(file: *mut ffi::sqlite3_file, output: *mut i32) -> i32 {
    let state = unsafe { state(file) };
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as i16;
    lock.l_whence = libc::SEEK_SET as i16;
    lock.l_start = RESERVED;
    lock.l_len = 1;
    if unsafe { libc::fcntl(state.file.as_raw_fd(), libc::F_OFD_GETLK, &mut lock) } < 0 {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    }
    unsafe {
        *output = i32::from(
            state.lock >= ffi::SQLITE_LOCK_RESERVED || lock.l_type != libc::F_UNLCK as i16,
        );
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn control(
    file: *mut ffi::sqlite3_file,
    operation: i32,
    arg: *mut libc::c_void,
) -> i32 {
    let state = unsafe { state(file) };
    match operation {
        ffi::SQLITE_FCNTL_LOCKSTATE => {
            unsafe {
                *arg.cast::<i32>() = state.lock;
            }
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_HAS_MOVED => {
            let current = state.file.metadata();
            use std::os::unix::fs::MetadataExt;
            let moved = match (current, state.entry.metadata()) {
                (Ok(current), Ok(Some(named))) => {
                    current.dev() != named.st_dev || current.ino() != named.st_ino
                }
                _ => true,
            };
            unsafe {
                *arg.cast::<i32>() = i32::from(moved);
            }
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_SIZE_HINT
        | ffi::SQLITE_FCNTL_SYNC
        | ffi::SQLITE_FCNTL_COMMIT_PHASETWO => ffi::SQLITE_OK,
        _ => ffi::SQLITE_NOTFOUND,
    }
}
unsafe extern "C" fn sector(_file: *mut ffi::sqlite3_file) -> i32 {
    4096
}
unsafe extern "C" fn characteristics(_file: *mut ffi::sqlite3_file) -> i32 {
    0
}
unsafe extern "C" fn shm_map(
    file: *mut ffi::sqlite3_file,
    page: i32,
    page_size: i32,
    extend: i32,
    output: *mut *mut libc::c_void,
) -> i32 {
    unsafe {
        *output = ptr::null_mut();
    }
    if page < 0 || page_size <= 0 {
        return ffi::SQLITE_IOERR_SHMMAP;
    }
    let state = unsafe { state(file) };
    if state.shm.is_none() {
        let mut name = state.entry.name.to_bytes().to_vec();
        name.extend_from_slice(b"-shm");
        let Ok(name) = CString::new(name) else {
            return ffi::SQLITE_IOERR_SHMOPEN;
        };
        let flags = if state.writable {
            libc::O_RDWR | libc::O_CREAT
        } else {
            libc::O_RDONLY
        };
        let Ok(shm) = filesystem::open_at(&state.entry.directory, &name, flags | libc::O_NONBLOCK)
        else {
            return ffi::SQLITE_IOERR_SHMOPEN;
        };
        if !shm.metadata().is_ok_and(|m| m.is_file()) {
            return ffi::SQLITE_IOERR_SHMOPEN;
        }
        if state.writable && range_lock(&shm, libc::F_WRLCK as i16, 128, 1) == ffi::SQLITE_OK {
            if shm.set_len(0).is_err() {
                return ffi::SQLITE_IOERR_SHMSIZE;
            }
        }
        let result = range_lock(&shm, libc::F_RDLCK as i16, 128, 1);
        if result != ffi::SQLITE_OK {
            return result;
        }
        state.shm = Some(SharedMemory {
            file: shm,
            name,
            regions: Vec::new(),
        });
    }
    let shm = state.shm.as_mut().unwrap();
    let index = page as usize;
    if let Some((address, size)) = shm.regions.get(index) {
        if !address.is_null() {
            if *size != page_size as usize {
                return ffi::SQLITE_IOERR_SHMMAP;
            }
            unsafe {
                *output = *address;
            }
            return ffi::SQLITE_OK;
        }
    }
    let Some(required) = (page as u64 + 1).checked_mul(page_size as u64) else {
        return ffi::SQLITE_IOERR_SHMSIZE;
    };
    let Ok(metadata) = shm.file.metadata() else {
        return ffi::SQLITE_IOERR_SHMSIZE;
    };
    if metadata.len() < required {
        if extend == 0 {
            return ffi::SQLITE_OK;
        }
        if !state.writable || shm.file.set_len(required).is_err() {
            return ffi::SQLITE_IOERR_SHMSIZE;
        }
    }
    let protection = libc::PROT_READ | if state.writable { libc::PROT_WRITE } else { 0 };
    let address = unsafe {
        libc::mmap(
            ptr::null_mut(),
            page_size as usize,
            protection,
            libc::MAP_SHARED,
            shm.file.as_raw_fd(),
            page as i64 * page_size as i64,
        )
    };
    if address == libc::MAP_FAILED {
        return ffi::SQLITE_IOERR_SHMMAP;
    }
    shm.regions.resize(index + 1, (ptr::null_mut(), 0));
    shm.regions[index] = (address, page_size as usize);
    unsafe {
        *output = address;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn shm_lock(
    file: *mut ffi::sqlite3_file,
    offset: i32,
    count: i32,
    flags: i32,
) -> i32 {
    let state = unsafe { state(file) };
    let Some(shm) = &state.shm else {
        return ffi::SQLITE_IOERR_SHMLOCK;
    };
    if offset < 0 || count <= 0 || offset + count > 8 {
        return ffi::SQLITE_IOERR_SHMLOCK;
    }
    let kind = if flags & ffi::SQLITE_SHM_UNLOCK != 0 {
        libc::F_UNLCK
    } else if flags & ffi::SQLITE_SHM_EXCLUSIVE != 0 {
        libc::F_WRLCK
    } else {
        libc::F_RDLCK
    };
    range_lock(&shm.file, kind as i16, 120 + offset as i64, count as i64)
}
unsafe extern "C" fn shm_barrier(_file: *mut ffi::sqlite3_file) {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}
unsafe extern "C" fn shm_unmap(file: *mut ffi::sqlite3_file, delete: i32) -> i32 {
    let state = unsafe { state(file) };
    if let Some(shm) = state.shm.take() {
        if delete != 0
            && state.writable
            && range_lock(&shm.file, libc::F_WRLCK as i16, 128, 1) == ffi::SQLITE_OK
        {
            if unsafe { libc::unlinkat(state.entry.directory.as_raw_fd(), shm.name.as_ptr(), 0) }
                < 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT)
            {
                return ffi::SQLITE_IOERR_DELETE;
            }
            if state.entry.directory.sync_all().is_err() {
                return ffi::SQLITE_IOERR_DIR_FSYNC;
            }
        }
        drop(shm);
    }
    ffi::SQLITE_OK
}
static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 2,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector),
    xDeviceCharacteristics: Some(characteristics),
    xShmMap: Some(shm_map),
    xShmLock: Some(shm_lock),
    xShmBarrier: Some(shm_barrier),
    xShmUnmap: Some(shm_unmap),
    xFetch: None,
    xUnfetch: None,
};
unsafe extern "C" fn delete(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const libc::c_char,
    _sync: i32,
) -> i32 {
    let Some(path) = (unsafe { path(name) }) else {
        return ffi::SQLITE_IOERR_DELETE;
    };
    let Ok(entry) = unsafe { authority(vfs) }.entry(path, true, false) else {
        return ffi::SQLITE_IOERR_DELETE;
    };
    if entry.name.to_bytes() == b"." {
        return ffi::SQLITE_IOERR_DELETE;
    }
    if entry.remove(true).is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_DELETE
    }
}
unsafe extern "C" fn access(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const libc::c_char,
    flags: i32,
    output: *mut i32,
) -> i32 {
    unsafe {
        *output = 0;
    }
    let Some(path) = (unsafe { path(name) }) else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    let Ok(entry) =
        unsafe { authority(vfs) }.entry(path, flags == ffi::SQLITE_ACCESS_READWRITE, false)
    else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    match entry.metadata() {
        Ok(metadata) => {
            unsafe {
                *output = i32::from(metadata.is_some());
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_IOERR_ACCESS,
    }
}
unsafe extern "C" fn full_path(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const libc::c_char,
    capacity: i32,
    output: *mut libc::c_char,
) -> i32 {
    let Some(path) = (unsafe { path(name) }) else {
        return ffi::SQLITE_CANTOPEN;
    };
    let Ok(entry) = unsafe { authority(vfs) }.entry(path, false, false) else {
        return ffi::SQLITE_CANTOPEN;
    };
    use std::os::unix::ffi::OsStrExt;
    let bytes = entry.path.as_os_str().as_bytes();
    if capacity <= 0 || bytes.len() >= capacity as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast::<u8>(), bytes.len());
        *output.add(bytes.len()) = 0;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn randomness(
    _vfs: *mut ffi::sqlite3_vfs,
    count: i32,
    output: *mut libc::c_char,
) -> i32 {
    if count <= 0 {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(output.cast::<u8>(), count as usize) };
    if filesystem::random_fill(bytes).is_ok() {
        count
    } else {
        0
    }
}
unsafe extern "C" fn sleep(_vfs: *mut ffi::sqlite3_vfs, microseconds: i32) -> i32 {
    if microseconds > 0 {
        std::thread::sleep(std::time::Duration::from_micros(microseconds as u64));
    }
    microseconds.max(0)
}
unsafe extern "C" fn current_time(_vfs: *mut ffi::sqlite3_vfs, output: *mut f64) -> i32 {
    let Ok(time) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return ffi::SQLITE_ERROR;
    };
    unsafe {
        *output = 2440587.5 + time.as_secs_f64() / 86400.0;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn current_time_ms(_vfs: *mut ffi::sqlite3_vfs, output: *mut i64) -> i32 {
    let Ok(time) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return ffi::SQLITE_ERROR;
    };
    unsafe {
        *output = 210866760000000 + time.as_millis() as i64;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    capacity: i32,
    output: *mut libc::c_char,
) -> i32 {
    let error = std::io::Error::last_os_error();
    if capacity > 0 && !output.is_null() {
        let text = error.to_string();
        let count = text.len().min(capacity as usize - 1);
        unsafe {
            ptr::copy_nonoverlapping(text.as_ptr(), output.cast::<u8>(), count);
            *output.add(count) = 0;
        }
    }
    error.raw_os_error().unwrap_or(0)
}
