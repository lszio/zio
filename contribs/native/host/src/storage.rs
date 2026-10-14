//! Generic persistence and identity mechanisms; no application schema or policy.
#[path = "storage_codec.rs"]
mod codec;
#[path = "storage_fs.rs"]
mod filesystem;
#[path = "storage_vfs.rs"]
mod vfs;
use crate::{HostPolicy, values};
use filesystem::Authority;
use parking_lot::Mutex;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;
use std::thread::ThreadId;
use zio_core::context::{EvalContext, EvalEngine};
use zio_core::error::EvalError;
use zio_core::im::{HashMap, Vector, vector};
use zio_core::value::{NativeFn, Value};

const DATABASE: &str = "host/database-handle";
pub(super) fn bind(
    ctx: &EvalContext,
    name: &str,
    function: impl Fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError> + 'static,
) {
    ctx.env.set(
        name.into(),
        Value::NativeFunction(NativeFn::new(name, function)),
    );
}
pub(super) fn buffer(bytes: Vec<u8>) -> Value {
    Value::Buffer(Arc::new(Mutex::new(bytes)))
}
pub(super) fn count(value: &Value) -> Result<usize, EvalError> {
    usize::try_from(values::integer(value)?).map_err(|_| {
        EvalError::custom("invalid-input: count must be nonnegative and fit the host address space")
    })
}
fn unavailable(authority: &Result<Arc<Authority>, String>) -> Result<&Authority, EvalError> {
    authority.as_deref().map_err(EvalError::custom)
}
fn sql_error(error: rusqlite::Error) -> EvalError {
    let class = match &error {
        rusqlite::Error::SqliteFailure(failure, _) => match failure.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                "resource-busy"
            }
            rusqlite::ErrorCode::PermissionDenied
            | rusqlite::ErrorCode::AuthorizationForStatementDenied
            | rusqlite::ErrorCode::ReadOnly => "capability-denied",
            rusqlite::ErrorCode::CannotOpen => "artifact-unavailable",
            rusqlite::ErrorCode::ConstraintViolation | rusqlite::ErrorCode::TypeMismatch => {
                "invalid-input"
            }
            _ => "backend-failed",
        },
        _ => "invalid-input",
    };
    EvalError::custom(format!("{class}: sqlite: {error}"))
}
struct Authorizer {
    internal: Cell<bool>,
}
unsafe extern "C" fn authorize(
    data: *mut libc::c_void,
    action: i32,
    first: *const libc::c_char,
    second: *const libc::c_char,
    _database: *const libc::c_char,
    _trigger: *const libc::c_char,
) -> i32 {
    let authorizer = unsafe { &*data.cast::<Authorizer>() };
    if matches!(
        action,
        rusqlite::ffi::SQLITE_TRANSACTION | rusqlite::ffi::SQLITE_SAVEPOINT
    ) && !authorizer.internal.get()
    {
        return rusqlite::ffi::SQLITE_DENY;
    }
    if action == rusqlite::ffi::SQLITE_PRAGMA && !second.is_null() {
        let name = if first.is_null() {
            "".into()
        } else {
            unsafe { CStr::from_ptr(first) }
                .to_string_lossy()
                .to_ascii_lowercase()
        };
        let value = unsafe { CStr::from_ptr(second) }
            .to_string_lossy()
            .to_ascii_lowercase();
        let safe = match name.as_str() {
            "journal_mode" => matches!(value.as_str(), "wal" | "delete" | "truncate" | "persist"),
            "synchronous" => matches!(value.as_str(), "full" | "extra" | "2" | "3"),
            "writable_schema" => matches!(value.as_str(), "off" | "false" | "0"),
            _ => true,
        };
        if !safe {
            return rusqlite::ffi::SQLITE_DENY;
        }
    }
    rusqlite::ffi::SQLITE_OK
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Owner {
    thread: ThreadId,
    engine: usize,
}
fn owner(engine: &dyn EvalEngine) -> Owner {
    Owner {
        thread: std::thread::current().id(),
        engine: engine as *const dyn EvalEngine as *const () as usize,
    }
}
struct Database {
    connection: Option<Connection>,
    authorizer: Box<Authorizer>,
    // Dropped only after the connection: SQLite retains this VFS pointer.
    _vfs: vfs::Vfs,
    owner: Option<Owner>,
    depth: usize,
    read_only: bool,
}
impl Database {
    fn connection(&self, caller: Owner) -> Result<&Connection, EvalError> {
        if self.owner.is_some_and(|owner| owner != caller) {
            return Err(EvalError::custom(
                "resource-busy: database transaction belongs to another execution context",
            ));
        }
        self.connection
            .as_ref()
            .ok_or_else(|| EvalError::custom("invalid-input: database is closed"))
    }
    fn control(&self, sql: &str) -> Result<(), EvalError> {
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| EvalError::custom("invalid-input: database is closed"))?;
        self.authorizer.internal.set(true);
        let result = connection.execute_batch(sql).map_err(sql_error);
        self.authorizer.internal.set(false);
        result
    }
}
fn open_database(
    authority: Arc<Authority>,
    path: &Path,
    force_read_only: bool,
) -> Result<Value, EvalError> {
    let read_only = force_read_only || !authority.writable(path)?;
    let entry = authority.entry(path, !read_only, !read_only)?;
    if entry.name.to_bytes() == b"." {
        return Err(EvalError::custom(
            "invalid-input: database path must name a regular file",
        ));
    }
    let metadata = entry.metadata()?;
    if metadata
        .as_ref()
        .is_some_and(|s| s.st_mode & libc::S_IFMT != libc::S_IFREG)
    {
        return Err(EvalError::custom(
            "invalid-input: database path must name a regular file",
        ));
    }
    if read_only && metadata.is_none() {
        return Err(EvalError::custom(
            "artifact-unavailable: read-only database does not exist",
        ));
    }
    let vfs = vfs::Vfs::new(authority)?;
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection =
        Connection::open_with_flags_and_vfs(&entry.path, flags, vfs.name()).map_err(sql_error)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(sql_error)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;")
        .map_err(sql_error)?;
    let mut authorizer = Box::new(Authorizer {
        internal: Cell::new(false),
    });
    let status = unsafe {
        rusqlite::ffi::sqlite3_set_authorizer(
            connection.handle(),
            Some(authorize),
            (&mut *authorizer as *mut Authorizer).cast(),
        )
    };
    if status != rusqlite::ffi::SQLITE_OK {
        return Err(EvalError::custom(
            "backend-failed: install SQLite transaction authority",
        ));
    }
    let state = Arc::new(Mutex::new(Database {
        connection: Some(connection),
        authorizer,
        _vfs: vfs,
        owner: None,
        depth: 0,
        read_only,
    }));
    Ok(Value::NativeFunction(NativeFn::new(
        DATABASE,
        move |args, engine| database_call(&state, args, engine),
    )))
}
fn parameters(value: &Value) -> Result<Vec<SqlValue>, EvalError> {
    let items = match value {
        Value::Vector(items) | Value::List(items) => items,
        _ => {
            return Err(EvalError::type_error(
                "SQL parameter vector/list",
                value.value_type(),
            ));
        }
    };
    items.iter().map(|value| match value {
        Value::Nil => Ok(SqlValue::Null),
        Value::Boolean(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Integer(value) => Ok(SqlValue::Integer(*value)),
        Value::Float(value) if value.is_finite() => Ok(SqlValue::Real(*value)),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Buffer(bytes) => Ok(SqlValue::Blob(bytes.lock().clone())),
        _ => Err(EvalError::custom("invalid-input: SQL parameters must be nil, boolean, finite number, string or buffer")),
    }).collect()
}
fn query(connection: &Connection, sql: &str, params: &[SqlValue]) -> Result<Value, EvalError> {
    let mut statement = connection.prepare(sql).map_err(sql_error)?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut unique = std::collections::BTreeSet::new();
    if names.iter().any(|name| !unique.insert(name)) {
        return Err(EvalError::custom(
            "invalid-input: duplicate SQL column names; use explicit aliases",
        ));
    }
    let mut rows = statement
        .query(params_from_iter(params))
        .map_err(sql_error)?;
    let mut output = Vector::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let mut values = HashMap::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index).map_err(sql_error)? {
                ValueRef::Null => Value::Nil,
                ValueRef::Integer(value) => Value::Integer(value),
                ValueRef::Real(value) if value.is_finite() => Value::Float(value),
                ValueRef::Real(_) => {
                    return Err(EvalError::custom("invalid-input: nonfinite SQL number"));
                }
                ValueRef::Text(value) => Value::String(
                    std::str::from_utf8(value)
                        .map_err(|_| EvalError::custom("invalid-input: SQL text is not UTF-8"))?
                        .into(),
                ),
                ValueRef::Blob(value) => buffer(value.to_vec()),
            };
            values.insert(Value::Keyword(name.clone()), value);
        }
        output.push_back(Value::Map(values));
    }
    Ok(Value::Vector(output))
}
fn transaction(
    state: &Arc<Mutex<Database>>,
    callback: Value,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let caller = owner(engine);
    let depth = {
        let mut database = state
            .try_lock()
            .ok_or_else(|| EvalError::custom("resource-busy: database operation in progress"))?;
        database.connection(caller)?;
        let depth = database.depth;
        let sql = if depth == 0 {
            if database.read_only {
                "BEGIN".to_owned()
            } else {
                "BEGIN IMMEDIATE".to_owned()
            }
        } else {
            format!("SAVEPOINT zio_host_{depth}")
        };
        database.control(&sql)?;
        database.owner = Some(caller);
        database.depth += 1;
        depth
    };
    // Do not retain a mutex or a rusqlite borrow across a language callback.
    let outcome = values::invoke(callback, Vector::new(), engine);
    let mut database = state.lock();
    let result = match outcome {
        Ok(value) => {
            let sql = if depth == 0 {
                "COMMIT".to_owned()
            } else {
                format!("RELEASE SAVEPOINT zio_host_{depth}")
            };
            match database.control(&sql) {
                Ok(()) => Ok(value),
                Err(error) => {
                    let rollback = if depth == 0 {
                        "ROLLBACK".to_owned()
                    } else {
                        format!(
                            "ROLLBACK TO SAVEPOINT zio_host_{depth}; RELEASE SAVEPOINT zio_host_{depth}"
                        )
                    };
                    match database.control(&rollback) {
                        Ok(()) => Err(error),
                        Err(rollback_error) => Err(EvalError::custom(format!(
                            "backend-failed: commit failed ({error}); rollback failed ({rollback_error})"
                        ))),
                    }
                }
            }
        }
        Err(error) => {
            let rollback = if depth == 0 {
                "ROLLBACK".to_owned()
            } else {
                format!(
                    "ROLLBACK TO SAVEPOINT zio_host_{depth}; RELEASE SAVEPOINT zio_host_{depth}"
                )
            };
            match database.control(&rollback) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(EvalError::custom(format!(
                    "backend-failed: callback failed ({error}); rollback failed ({rollback_error})"
                ))),
            }
        }
    };
    database.depth = depth;
    if depth == 0 {
        database.owner = None;
    }
    // A failed rollback must never leave a usable half-transaction connection.
    if depth == 0
        && database
            .connection
            .as_ref()
            .is_some_and(|c| !c.is_autocommit())
    {
        database.connection.take();
    }
    result
}
fn database_call(
    state: &Arc<Mutex<Database>>,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let operation = match args.front() {
        Some(Value::Keyword(name)) => name.as_str(),
        _ => {
            return Err(EvalError::custom(
                "invalid-input: opaque database operation",
            ));
        }
    };
    if operation == "transaction" {
        values::arity(&args, 2)?;
        return transaction(state, args[1].clone(), engine);
    }
    let mut database = state
        .try_lock()
        .ok_or_else(|| EvalError::custom("resource-busy: database operation in progress"))?;
    database.connection(owner(engine))?;
    match operation {
        "close" => {
            values::arity(&args, 1)?;
            if database.depth != 0 {
                return Err(EvalError::custom(
                    "resource-busy: cannot close database during a transaction",
                ));
            }
            let connection = database.connection.take().unwrap();
            match connection.close() {
                Ok(()) => Ok(Value::Nil),
                Err((connection, error)) => {
                    database.connection = Some(connection);
                    Err(sql_error(error))
                }
            }
        }
        "query" | "execute" => {
            values::arity(&args, 3)?;
            let sql = values::string(&args[1])?;
            let params = parameters(&args[2])?;
            let connection = database.connection(owner(engine))?;
            if operation == "query" {
                query(connection, sql, &params)
            } else {
                let mut statement = connection.prepare(sql).map_err(sql_error)?;
                let affected = statement
                    .execute(params_from_iter(&params))
                    .map_err(sql_error)?;
                Ok(Value::Integer(i64::try_from(affected).map_err(|_| {
                    EvalError::custom("backend-failed: SQL affected row count overflow")
                })?))
            }
        }
        _ => Err(EvalError::custom(
            "invalid-input: unknown opaque database operation",
        )),
    }
}

/// Install primitives with a private immutable capability capture. Directory
/// opening failures remain fail-closed and are reported on use, not ignored.
pub fn install(ctx: &EvalContext, policy: &HostPolicy) {
    let authority = Authority::capture(policy)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    let database_authority = authority.clone();
    bind(ctx, "host/db-open", move |args, _| {
        if args.len() != 1 && args.len() != 2 {
            return Err(EvalError::wrong_arg_count(1, args.len()));
        }
        let read_only = if args.len() == 2 {
            let Value::Map(options) = &args[1] else {
                return Err(EvalError::type_error(
                    "database options map",
                    args[1].value_type(),
                ));
            };
            if options
                .keys()
                .any(|key| !matches!(key,Value::Keyword(s) | Value::String(s) if s == "read-only"))
            {
                return Err(EvalError::custom("invalid-input: unknown database option"));
            }
            match values::get(&args[1], "read-only") {
                None | Some(Value::Boolean(false)) => false,
                Some(Value::Boolean(true)) => true,
                _ => {
                    return Err(EvalError::custom(
                        "invalid-input: read-only option must be boolean",
                    ));
                }
            }
        } else {
            false
        };
        let authority = database_authority
            .as_ref()
            .map_err(EvalError::custom)?
            .clone();
        open_database(authority, Path::new(values::string(&args[0])?), read_only)
    });
    for (name, operation, arity) in [
        ("host/db-query", "query", 3),
        ("host/db-execute", "execute", 3),
        ("host/db-transaction", "transaction", 2),
        ("host/db-close", "close", 1),
    ] {
        bind(ctx, name, move |args, engine| {
            values::arity(&args, arity)?;
            let mut request = vector![Value::Keyword(operation.into())];
            for value in args.iter().skip(1) {
                request.push_back(value.clone());
            }
            values::handle(&args[0], DATABASE, request, engine)
        });
    }
    for name in [
        "host/read-bytes",
        "host/write-atomic",
        "host/mkdir",
        "host/list-dir",
        "host/remove",
        "host/rename",
        "host/canonical-path",
        "host/stat-path",
    ] {
        let authority = authority.clone();
        bind(ctx, name, move |args, _| {
            values::arity(
                &args,
                if matches!(name, "host/write-atomic" | "host/rename") {
                    2
                } else {
                    1
                },
            )?;
            let authority = unavailable(&authority)?;
            let path = Path::new(values::string(&args[0])?);
            match name {
                "host/read-bytes" => Ok(buffer(authority.read(path)?)),
                "host/write-atomic" => {
                    values::with_bytes(&args[1], |bytes| authority.write_atomic(path, bytes))??;
                    Ok(Value::Nil)
                }
                "host/mkdir" => {
                    authority.mkdir(path)?;
                    Ok(Value::Nil)
                }
                "host/list-dir" => Ok(Value::Vector(
                    authority
                        .list(path)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                )),
                "host/remove" => {
                    let entry = authority.entry(path, true, false)?;
                    if entry.name.to_bytes() == b"." {
                        return Err(EvalError::custom(
                            "capability-denied: cannot remove capability root",
                        ));
                    }
                    Ok(Value::Boolean(entry.remove(true)?))
                }
                "host/rename" => {
                    authority.rename(path, Path::new(values::string(&args[1])?))?;
                    Ok(Value::Nil)
                }
                "host/stat-path" => {
                    let entry = authority.entry(path, false, false)?;
                    match entry.metadata()? {
                        None => Ok(Value::Nil),
                        Some(stat) => {
                            let kind = match stat.st_mode & libc::S_IFMT {
                                libc::S_IFREG => "file",
                                libc::S_IFDIR => "directory",
                                _ => "other",
                            };
                            let modified = stat
                                .st_mtime
                                .checked_mul(1000)
                                .and_then(|n| n.checked_add(stat.st_mtime_nsec / 1_000_000))
                                .ok_or_else(|| {
                                    EvalError::custom("invalid-input: file timestamp out of range")
                                })?;
                            Ok(values::map([
                                ("type", Value::Keyword(kind.into())),
                                ("size", Value::Integer(stat.st_size)),
                                ("modified-ms", Value::Integer(modified)),
                            ]))
                        }
                    }
                }
                _ => Ok(Value::String(
                    authority
                        .canonical(path)?
                        .to_str()
                        .ok_or_else(|| {
                            EvalError::custom("invalid-input: canonical path is not UTF-8")
                        })?
                        .into(),
                )),
            }
        });
    }
    bind(ctx, "host/sha256", |args, _| {
        values::arity(&args, 1)?;
        values::with_bytes(&args[0], |bytes| {
            Value::String(filesystem::hex(&Sha256::digest(bytes)))
        })
    });
    bind(ctx, "host/random-bytes", |args, _| {
        values::arity(&args, 1)?;
        let mut bytes = Vec::new();
        let count = count(&args[0])?;
        bytes
            .try_reserve_exact(count)
            .map_err(|_| EvalError::custom("invalid-input: random byte allocation too large"))?;
        bytes.resize(count, 0);
        filesystem::random_fill(&mut bytes)?;
        Ok(buffer(bytes))
    });
    bind(ctx, "host/clock-ms", |args, _| {
        values::arity(&args, 0)?;
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| EvalError::custom(format!("backend-failed: clock: {e}")))?;
        Ok(Value::Integer(
            i64::try_from(duration.as_millis())
                .map_err(|_| EvalError::custom("backend-failed: clock overflow"))?,
        ))
    });
    bind(ctx, "host/sleep-ms", |args, _| {
        values::arity(&args, 1)?;
        let millis = count(&args[0])?;
        std::thread::sleep(std::time::Duration::from_millis(millis as u64));
        Ok(Value::Nil)
    });
    let argv = policy.argv.clone();
    bind(ctx, "host/argv", move |args, _| {
        values::arity(&args, 0)?;
        Ok(Value::Vector(
            argv.iter().cloned().map(Value::String).collect(),
        ))
    });
    let environment = policy.environment.clone();
    bind(ctx, "host/getenv", move |args, _| {
        values::arity(&args, 1)?;
        let name = values::string(&args[0])?;
        if !environment.contains(name) {
            return Err(EvalError::custom(format!(
                "capability-denied: environment {name}"
            )));
        }
        match std::env::var(name) {
            Ok(value) => Ok(Value::String(value)),
            Err(std::env::VarError::NotPresent) => Ok(Value::Nil),
            Err(_) => Err(EvalError::custom(
                "invalid-input: environment value is not UTF-8",
            )),
        }
    });
    codec::install(ctx);
}
