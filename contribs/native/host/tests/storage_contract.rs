use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::im::{Vector, vector};
use zio_core::value::{NativeFn, Value};
use zio_host::{HostPolicy, storage, values};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zio-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn context(&self) -> EvalContext {
        let ctx = language_context(ModuleRoots::empty()).unwrap();
        storage::install(
            &ctx,
            &HostPolicy {
                read_roots: vec![self.0.clone()],
                write_roots: vec![self.0.clone()],
                ..Default::default()
            },
        );
        ctx
    }
    fn path(&self, name: &str) -> Value {
        Value::String(self.0.join(name).to_str().unwrap().into())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn call(ctx: &EvalContext, name: &str, args: Vector<Value>) -> Result<Value, EvalError> {
    values::invoke(ctx.env.get(name).unwrap(), args, ctx)
}
fn execute(
    ctx: &EvalContext,
    db: &Value,
    sql: &str,
    params: Vector<Value>,
) -> Result<Value, EvalError> {
    call(
        ctx,
        "host/db-execute",
        vector![db.clone(), Value::String(sql.into()), Value::Vector(params)],
    )
}
fn query(ctx: &EvalContext, db: &Value, sql: &str) -> Value {
    call(
        ctx,
        "host/db-query",
        vector![
            db.clone(),
            Value::String(sql.into()),
            Value::Vector(Vector::new())
        ],
    )
    .unwrap()
}

#[test]
fn callback_failure_rolls_back_and_nested_callback_operations_share_connection() {
    let dir = Scratch::new();
    let ctx = dir.context();
    let db = call(&ctx, "host/db-open", vector![dir.path("state.db")]).unwrap();
    ctx.env.set("db".into(), db.clone());
    execute(
        &ctx,
        &db,
        "CREATE TABLE counters(id TEXT PRIMARY KEY, version INTEGER)",
        Vector::new(),
    )
    .unwrap();
    eval_source(&ctx, "transaction", "(host/db-transaction db (fn [] (host/db-execute db \"INSERT INTO counters VALUES (?1,?2)\" [\"one\" 0]) (host/db-transaction db (fn [] (host/db-execute db \"UPDATE counters SET version=1\" []))) 17))").unwrap();
    let failed = NativeFn::new("rollback", {
        let db = db.clone();
        move |_, engine| {
            values::invoke(
                engine.env().get("host/db-execute").unwrap(),
                vector![
                    db.clone(),
                    Value::String("UPDATE counters SET version=9".into()),
                    Value::Vector(Vector::new())
                ],
                engine,
            )?;
            Err(EvalError::custom("intentional callback failure"))
        }
    });
    assert!(
        call(
            &ctx,
            "host/db-transaction",
            vector![db.clone(), Value::NativeFunction(failed)]
        )
        .is_err()
    );
    let rows = query(&ctx, &db, "SELECT id,version FROM counters");
    let Value::Vector(rows) = rows else {
        panic!("rows")
    };
    assert_eq!(values::get(&rows[0], "version"), Some(&Value::Integer(1)));
    assert_eq!(
        execute(
            &ctx,
            &db,
            "UPDATE counters SET version=2 WHERE id=?1 AND version=?2",
            vector![Value::String("one".into()), Value::Integer(1)]
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        execute(
            &ctx,
            &db,
            "UPDATE counters SET version=3 WHERE id=?1 AND version=?2",
            vector![Value::String("one".into()), Value::Integer(1)]
        )
        .unwrap(),
        Value::Integer(0)
    );
    call(&ctx, "host/db-close", vector![db.clone()]).unwrap();
    assert!(call(&ctx, "host/db-close", vector![db.clone()]).is_err());
    assert!(
        call(
            &ctx,
            "host/db-query",
            vector![
                db,
                Value::String("SELECT 1".into()),
                Value::Vector(Vector::new())
            ]
        )
        .is_err()
    );
}

#[test]
fn sql_parameters_blob_and_wal_survive_reopen_and_competing_cas() {
    let dir = Scratch::new();
    let ctx = dir.context();
    let db = call(&ctx, "host/db-open", vector![dir.path("state.db")]).unwrap();
    let Value::Vector(mode) = query(&ctx, &db, "PRAGMA journal_mode=WAL") else {
        panic!("mode")
    };
    assert_eq!(
        values::get(&mode[0], "journal_mode"),
        Some(&Value::String("wal".into()))
    );
    execute(
        &ctx,
        &db,
        "CREATE TABLE t(k TEXT PRIMARY KEY, v INTEGER, payload BLOB)",
        Vector::new(),
    )
    .unwrap();
    let blob = Value::Buffer(Arc::new(parking_lot::Mutex::new(vec![0, 255, 10, 0])));
    execute(
        &ctx,
        &db,
        "INSERT INTO t VALUES (?1,?2,?3)",
        vector![
            Value::String("x'; DROP TABLE t;--".into()),
            Value::Integer(0),
            blob
        ],
    )
    .unwrap();
    assert!(execute(&ctx, &db, "UPDATE t SET v=?1", Vector::new()).is_err());
    assert!(execute(&ctx, &db, "BEGIN", Vector::new()).is_err());
    assert!(execute(&ctx, &db, "PRAGMA journal_mode=OFF", Vector::new()).is_err());
    assert!(execute(&ctx, &db, "PRAGMA synchronous=OFF", Vector::new()).is_err());
    let second = call(&ctx, "host/db-open", vector![dir.path("state.db")]).unwrap();
    assert_eq!(
        execute(&ctx, &db, "UPDATE t SET v=1 WHERE v=0", Vector::new()).unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        execute(&ctx, &second, "UPDATE t SET v=2 WHERE v=0", Vector::new()).unwrap(),
        Value::Integer(0)
    );
    call(&ctx, "host/db-close", vector![db]).unwrap();
    call(&ctx, "host/db-close", vector![second]).unwrap();
    let reopened = call(&ctx, "host/db-open", vector![dir.path("state.db")]).unwrap();
    let Value::Vector(rows) = query(&ctx, &reopened, "SELECT v,payload FROM t") else {
        panic!("rows")
    };
    assert_eq!(values::get(&rows[0], "v"), Some(&Value::Integer(1)));
    assert_eq!(
        values::with_bytes(values::get(&rows[0], "payload").unwrap(), |b| b.to_vec()).unwrap(),
        [0, 255, 10, 0]
    );
}

#[test]
fn exact_binary_atomic_io_and_symlink_authority() {
    use std::os::unix::fs::symlink;
    let dir = Scratch::new();
    let foreign = Scratch::new();
    let ctx = dir.context();
    let payload = Value::Buffer(Arc::new(parking_lot::Mutex::new(vec![0, 255, 10, 0, 128])));
    call(
        &ctx,
        "host/write-atomic",
        vector![dir.path("nested/data.bin"), payload],
    )
    .unwrap();
    let bytes = call(
        &ctx,
        "host/read-bytes",
        vector![dir.path("nested/data.bin")],
    )
    .unwrap();
    assert_eq!(
        values::with_bytes(&bytes, |b| b.to_vec()).unwrap(),
        [0, 255, 10, 0, 128]
    );
    assert_eq!(
        std::fs::read(dir.0.join("nested/data.bin")).unwrap(),
        [0, 255, 10, 0, 128]
    );
    std::fs::write(foreign.0.join("canary"), b"untouched").unwrap();
    symlink(&foreign.0, dir.0.join("escape")).unwrap();
    symlink(foreign.0.join("canary"), dir.0.join("alias")).unwrap();
    for path in [
        dir.path("escape/canary"),
        dir.path("alias"),
        foreign.path("canary"),
    ] {
        assert!(call(&ctx, "host/read-bytes", vector![path.clone()]).is_err());
        assert!(
            call(
                &ctx,
                "host/write-atomic",
                vector![path.clone(), Value::String("attack".into())]
            )
            .is_err()
        );
        assert!(call(&ctx, "host/db-open", vector![path.clone()]).is_err());
        assert!(call(&ctx, "host/remove", vector![path]).is_err());
    }
    assert_eq!(
        std::fs::read(foreign.0.join("canary")).unwrap(),
        b"untouched"
    );
    call(
        &ctx,
        "host/rename",
        vector![dir.path("nested/data.bin"), dir.path("nested/new.bin")],
    )
    .unwrap();
    assert!(dir.0.join("nested/new.bin").is_file());
    call(&ctx, "host/remove", vector![dir.path("nested/new.bin")]).unwrap();
    assert!(!dir.0.join("nested/new.bin").exists());
}

#[test]
fn held_root_descriptors_ignore_replacement_symlinks_and_sql_sidecars_refuse_them() {
    use std::os::unix::fs::symlink;
    let dir = Scratch::new();
    let foreign = Scratch::new();
    let ctx = dir.context();
    let original = dir.0.with_extension("held");
    std::fs::rename(&dir.0, &original).unwrap();
    symlink(&foreign.0, &dir.0).unwrap();
    call(
        &ctx,
        "host/write-atomic",
        vector![dir.path("pinned"), Value::String("held".into())],
    )
    .unwrap();
    assert_eq!(std::fs::read(original.join("pinned")).unwrap(), b"held");
    assert!(!foreign.0.join("pinned").exists());
    let db = call(&ctx, "host/db-open", vector![dir.path("db")]).unwrap();
    std::fs::write(foreign.0.join("canary"), b"safe").unwrap();
    symlink(foreign.0.join("canary"), original.join("db-journal")).unwrap();
    assert!(execute(&ctx, &db, "CREATE TABLE t(x)", Vector::new()).is_err());
    assert_eq!(std::fs::read(foreign.0.join("canary")).unwrap(), b"safe");
    assert!(
        execute(
            &ctx,
            &db,
            &format!(
                "ATTACH DATABASE '{}' AS foreign",
                foreign.0.join("other.db").display()
            ),
            Vector::new()
        )
        .is_err()
    );
    call(&ctx, "host/db-close", vector![db]).unwrap();
    std::fs::remove_file(&dir.0).unwrap();
    std::fs::rename(original, &dir.0).unwrap();
}

#[test]
fn scalar_and_ordered_json_encodings_preserve_exact_historical_bytes() {
    let dir = Scratch::new();
    let ctx = dir.context();
    let encoded = call(
        &ctx,
        "host/binary-encode",
        vector![Value::Keyword("f32-le".into()), Value::Float(1.5)],
    )
    .unwrap();
    assert_eq!(
        values::with_bytes(&encoded, |b| b.to_vec()).unwrap(),
        1.5f32.to_le_bytes()
    );
    assert_eq!(
        call(
            &ctx,
            "host/binary-decode",
            vector![Value::Keyword("f32-le".into()), encoded, Value::Integer(0)]
        )
        .unwrap(),
        Value::Float(1.5)
    );
    let pairs = Value::Vector(vector![
        Value::Vector(vector![Value::String("z".into()), Value::Integer(1)]),
        Value::Vector(vector![Value::String("a".into()), Value::Nil])
    ]);
    let encoded = call(&ctx, "host/json-ordered", vector![pairs]).unwrap();
    assert_eq!(
        values::with_bytes(&encoded, |b| b.to_vec()).unwrap(),
        b"{\"z\":1,\"a\":null}"
    );
    assert_eq!(
        call(&ctx, "host/sha256", vector![Value::String("abc".into())]).unwrap(),
        Value::String("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into())
    );
    assert!(
        call(
            &ctx,
            "host/binary-decode",
            vector![
                Value::Keyword("u32-le".into()),
                Value::String("a".into()),
                Value::Integer(0)
            ]
        )
        .is_err()
    );
    assert!(call(&ctx, "host/getenv", vector![Value::String("HOME".into())]).is_err());
}
