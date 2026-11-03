//! SQLite connections backing `node:sqlite` (`js/node/sqlite.js`): a port of Node 22.23's
//! `src/node_sqlite.cc` onto the bundled SQLite C API (`rusqlite::ffi`).
//!
//! Connections, prepared statements and backups live in per-thread tables keyed by id; the JS
//! classes hold the ids. `sqliteOpen` and `sqlitePrepare` also return a guard object whose
//! collection closes the connection or finalizes the statement, like Node's weak
//! `BaseObject`s. The release is queued and done by the next sqlite op (or teardown), never
//! inside the garbage collector.
//!
//! User functions and aggregates call back into JS from inside `sqlite3_step`. A JS exception
//! raised there is parked on the connection and rethrown in place of the SQLite error the
//! callback reports, which is Node's `ignore_next_sqlite_error_`.
//!
//! Values follow Node: numbers bind as REAL, bigints as INTEGER, strings as TEXT, typed arrays
//! and DataViews as BLOB; INTEGER results are numbers (bigints with `readBigInts`), BLOBs are
//! `Uint8Array`s, and rows are null-prototype objects (arrays with `returnArrays`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::rc::{Rc, Weak};

use crate::js::{
    Context, JsArgs, JsBigInt, JsError, JsFunction, JsObject, JsResult, JsString, JsTypedArray,
    JsValue, PropertyKey,
};
use rusqlite::ffi;

use super::{array, bytes_arg, bytes_to_js, coded_error, native_object, object};
use crate::engine::{OpFn, arg_f64, js_str};

unsafe extern "C" {
    /// Close a connection, deferring the close while statements or backups are open
    /// (libsqlite3-sys leaves it out of its generated bindings; the bundled library has it).
    fn sqlite3_close_v2(db: *mut ffi::sqlite3) -> c_int;
}

/// `Number.MAX_SAFE_INTEGER`: larger INTEGERs only read back as bigints.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Statement call flags (`sqlite.js` keeps the per-statement settings and passes them along).
const READ_BIG_INTS: u32 = 1;
const RETURN_ARRAYS: u32 = 2;
const ALLOW_BARE_NAMED: u32 = 4;
const ALLOW_UNKNOWN_NAMED: u32 = 8;

/// One open connection.
struct Database {
    handle: *mut ffi::sqlite3,
    /// The `allowExtension` constructor option; `enableLoadExtension(true)` needs it.
    allow_extension: bool,
    /// Prepared statements of this connection, finalized when it closes.
    statements: RefCell<HashSet<u64>>,
    /// Backups reading from this connection, abandoned when it closes.
    backups: RefCell<HashSet<u64>>,
    /// A JS exception thrown by a user function during the current SQLite call.
    pending: RefCell<Option<JsError>>,
    /// Nesting depth of SQLite calls that may run user functions (closing then is refused).
    busy: Cell<u32>,
}

/// One prepared statement. `raw` is null when the SQL held no statement (Node treats such a
/// statement as finalized).
struct Statement {
    raw: *mut ffi::sqlite3_stmt,
    db: u64,
    /// Bare name -> prefixed name, built on the first named bind (`bare_named_params_`).
    bare: Option<Rc<HashMap<String, String>>>,
}

/// One backup in progress from `source` into its own destination connection.
struct Backup {
    source: u64,
    dest: *mut ffi::sqlite3,
    backup: *mut ffi::sqlite3_backup,
}

/// A connection or statement whose JS owner was collected.
enum Release {
    Database(u64),
    Statement(u64),
}

/// Queues its release when the owning JS object is collected.
struct ReleaseGuard(Option<Release>);

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = RELEASED.try_with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    r.push(release);
                }
            });
        }
    }
}

thread_local! {
    static DATABASES: RefCell<HashMap<u64, Rc<Database>>> = RefCell::new(HashMap::new());
    static STATEMENTS: RefCell<HashMap<u64, Statement>> = RefCell::new(HashMap::new());
    static BACKUPS: RefCell<HashMap<u64, Backup>> = RefCell::new(HashMap::new());
    static RELEASED: RefCell<Vec<Release>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

fn next_id() -> u64 {
    NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `"text"` as a C string, cut at the first NUL like Node's `Utf8Value` C strings.
fn c_text(s: &str) -> CString {
    let end = s.find('\0').unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}

/// A NUL-terminated C string as Rust text (invalid UTF-8 replaced).
///
/// # Safety
/// `p` is null or points to a NUL-terminated string valid for the call.
unsafe fn c_str(p: *const c_char) -> Option<String> {
    // SAFETY: the caller guarantees `p` is null or NUL-terminated.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// A JS string as UTF-8, each lone surrogate becoming one U+FFFD (V8's `Utf8Value`).
fn utf8(s: &JsString) -> String {
    let text = s.to_std_string_lossy();
    if text.contains('\u{FFFD}') {
        String::from_utf16_lossy(&s.to_utf16())
    } else {
        text
    }
}

/// Argument `i` as UTF-8 text (see [`utf8`]).
fn arg_text(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    Ok(utf8(&args.get_or_undefined(i).to_string(ctx)?))
}

fn errstr(code: c_int) -> String {
    // SAFETY: `sqlite3_errstr` returns a static string for any code.
    unsafe { c_str(ffi::sqlite3_errstr(code)) }.unwrap_or_default()
}

/// `ERR_SQLITE_ERROR` with only a message (`CreateSQLiteError(isolate, message)`).
fn error_message(ctx: &mut Context, message: &str) -> JsError {
    coded_error(
        ctx,
        "Error",
        message,
        &[("code", js_str("ERR_SQLITE_ERROR"))],
    )
}

/// `ERR_SQLITE_ERROR` for a result code (`CreateSQLiteError(isolate, errcode)`).
fn error_code(ctx: &mut Context, code: c_int) -> JsError {
    let text = errstr(code);
    coded_error(
        ctx,
        "Error",
        &text,
        &[
            ("code", js_str("ERR_SQLITE_ERROR")),
            ("errcode", JsValue::from(code)),
            ("errstr", js_str(&text)),
        ],
    )
}

/// `ERR_SQLITE_ERROR` for the last error of a connection (`CreateSQLiteError(isolate, db)`).
fn error_handle(ctx: &mut Context, handle: *mut ffi::sqlite3) -> JsError {
    // SAFETY: `handle` is a connection of this thread (or null, which SQLite reports as
    // out of memory).
    let (code, message) = unsafe {
        (
            ffi::sqlite3_extended_errcode(handle),
            c_str(ffi::sqlite3_errmsg(handle)).unwrap_or_default(),
        )
    };
    coded_error(
        ctx,
        "Error",
        &message,
        &[
            ("code", js_str("ERR_SQLITE_ERROR")),
            ("errcode", JsValue::from(code)),
            ("errstr", js_str(&errstr(code))),
        ],
    )
}

/// The error for a failed call on `db`: the JS exception a callback raised, else SQLite's.
fn sqlite_error(ctx: &mut Context, db: &Database) -> JsError {
    match db.pending.borrow_mut().take() {
        Some(e) => e,
        None => error_handle(ctx, db.handle),
    }
}

fn check(ctx: &mut Context, db: &Database, r: c_int, expected: c_int) -> JsResult<()> {
    if r == expected {
        Ok(())
    } else {
        Err(sqlite_error(ctx, db))
    }
}

fn invalid_state(ctx: &mut Context, message: &str) -> JsError {
    coded_error(
        ctx,
        "Error",
        message,
        &[("code", js_str("ERR_INVALID_STATE"))],
    )
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn db_id(args: &[JsValue], ctx: &mut Context) -> JsResult<u64> {
    Ok(arg_f64(args, 0, ctx)? as u64)
}

fn database(ctx: &mut Context, id: u64) -> JsResult<Rc<Database>> {
    release_collected();
    match DATABASES.with(|dbs| dbs.borrow().get(&id).cloned()) {
        Some(db) => Ok(db),
        None => Err(invalid_state(ctx, "database is not open")),
    }
}

/// The live statement `id` and its connection.
fn statement(ctx: &mut Context, id: u64) -> JsResult<(*mut ffi::sqlite3_stmt, Rc<Database>)> {
    release_collected();
    let found = STATEMENTS.with(|s| s.borrow().get(&id).map(|st| (st.raw, st.db)));
    let db = found
        .filter(|(raw, _)| !raw.is_null())
        .and_then(|(raw, db)| {
            DATABASES
                .with(|dbs| dbs.borrow().get(&db).cloned())
                .map(|db| (raw, db))
        });
    db.ok_or_else(|| invalid_state(ctx, "statement has been finalized"))
}

fn finalize_statement(id: u64) {
    let Some(st) = STATEMENTS.with(|s| s.borrow_mut().remove(&id)) else {
        return;
    };
    if let Some(db) = DATABASES.with(|dbs| dbs.borrow().get(&st.db).cloned()) {
        db.statements.borrow_mut().remove(&id);
    }
    // SAFETY: the statement was removed from the table, so this is its only finalize.
    unsafe { ffi::sqlite3_finalize(st.raw) };
}

fn cleanup_backup(id: u64) {
    let Some(b) = BACKUPS.with(|t| t.borrow_mut().remove(&id)) else {
        return;
    };
    if let Some(db) = DATABASES.with(|dbs| dbs.borrow().get(&b.source).cloned()) {
        db.backups.borrow_mut().remove(&id);
    }
    // SAFETY: the backup was removed from the table, so it is finished and closed once.
    unsafe {
        if !b.backup.is_null() {
            ffi::sqlite3_backup_finish(b.backup);
        }
        sqlite3_close_v2(b.dest);
    }
}

/// Close connection `id`: abandon its backups, finalize its statements, then close it (which
/// drops its user functions).
fn close_database(id: u64) -> Option<(Rc<Database>, c_int)> {
    let db = DATABASES.with(|dbs| dbs.borrow().get(&id).cloned())?;
    let backups: Vec<u64> = db.backups.borrow().iter().copied().collect();
    for b in backups {
        cleanup_backup(b);
    }
    let statements: Vec<u64> = db.statements.borrow().iter().copied().collect();
    for s in statements {
        finalize_statement(s);
    }
    DATABASES.with(|dbs| dbs.borrow_mut().remove(&id));
    // SAFETY: the connection was removed from the table, so it is closed once; its statements
    // are finalized. Closing runs the destructors of its user functions, which only drop JS
    // values (no table is borrowed here).
    let r = unsafe { sqlite3_close_v2(db.handle) };
    Some((db, r))
}

/// Release connections and statements whose JS owners were collected.
fn release_collected() {
    let released = RELEASED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for release in released {
        match release {
            Release::Statement(id) => finalize_statement(id),
            Release::Database(id) => {
                let busy =
                    DATABASES.with(|dbs| dbs.borrow().get(&id).is_some_and(|db| db.busy.get() > 0));
                if busy {
                    // Retried by a later op, once the running call has returned.
                    RELEASED.with(|r| r.borrow_mut().push(Release::Database(id)));
                } else {
                    close_database(id);
                }
            }
        }
    }
}

/// Marks `db` busy while SQLite may call back into JS.
struct BusyGuard<'a>(&'a Database);

impl<'a> BusyGuard<'a> {
    fn new(db: &'a Database) -> Self {
        db.busy.set(db.busy.get() + 1);
        BusyGuard(db)
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.set(self.0.busy.get() - 1);
    }
}

/// Resets a statement when a read ends (Node's `OnScopeLeave` reset).
struct ResetGuard(*mut ffi::sqlite3_stmt);

impl Drop for ResetGuard {
    fn drop(&mut self) {
        // SAFETY: the statement stays alive while the op runs: finalizing it needs its
        // connection closed, which is refused while the connection is busy.
        unsafe { ffi::sqlite3_reset(self.0) };
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

fn integer_to_js(ctx: &mut Context, i: i64, big: bool) -> JsResult<JsValue> {
    if big {
        Ok(JsValue::from(JsBigInt::from(i)))
    } else if i.unsigned_abs() <= MAX_SAFE_INTEGER {
        Ok(JsValue::from(i as f64))
    } else {
        Err(coded_error(
            ctx,
            "RangeError",
            &format!("Value is too large to be represented as a JavaScript number: {i}"),
            &[("code", js_str("ERR_OUT_OF_RANGE"))],
        ))
    }
}

/// TEXT bytes as a JS string, cut at the first NUL like V8's `NewFromUtf8` of a C string.
fn text_to_js(bytes: &[u8]) -> JsValue {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    js_str(&String::from_utf8_lossy(&bytes[..end]))
}

/// A slice of SQLite-owned bytes.
///
/// # Safety
/// `p` is null or valid for `n` bytes for the call.
unsafe fn sqlite_bytes<'a>(p: *const c_void, n: c_int) -> &'a [u8] {
    if p.is_null() || n <= 0 {
        &[]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n as usize) }
    }
}

/// Column `i` of the current row (`SQLITE_VALUE_TO_JS(column, ...)`).
fn column_value(
    ctx: &mut Context,
    raw: *mut ffi::sqlite3_stmt,
    i: c_int,
    big: bool,
) -> JsResult<JsValue> {
    // SAFETY: `raw` is a live statement positioned on a row, and `i` is below its column
    // count; the text/blob pointers are read before any other call on the statement.
    unsafe {
        match ffi::sqlite3_column_type(raw, i) {
            ffi::SQLITE_INTEGER => integer_to_js(ctx, ffi::sqlite3_column_int64(raw, i), big),
            ffi::SQLITE_FLOAT => Ok(JsValue::from(ffi::sqlite3_column_double(raw, i))),
            ffi::SQLITE_TEXT => {
                let p = ffi::sqlite3_column_text(raw, i).cast::<c_void>();
                Ok(text_to_js(sqlite_bytes(
                    p,
                    ffi::sqlite3_column_bytes(raw, i),
                )))
            }
            ffi::SQLITE_BLOB => {
                let p = ffi::sqlite3_column_blob(raw, i);
                bytes_to_js(sqlite_bytes(p, ffi::sqlite3_column_bytes(raw, i)), ctx)
            }
            _ => Ok(JsValue::null()),
        }
    }
}

/// A user-function argument (`SQLITE_VALUE_TO_JS(value, ...)`).
fn sqlite_value_to_js(
    ctx: &mut Context,
    v: *mut ffi::sqlite3_value,
    big: bool,
) -> JsResult<JsValue> {
    // SAFETY: `v` is an argument of the running callback.
    unsafe {
        match ffi::sqlite3_value_type(v) {
            ffi::SQLITE_INTEGER => integer_to_js(ctx, ffi::sqlite3_value_int64(v), big),
            ffi::SQLITE_FLOAT => Ok(JsValue::from(ffi::sqlite3_value_double(v))),
            ffi::SQLITE_TEXT => {
                let p = ffi::sqlite3_value_text(v).cast::<c_void>();
                Ok(text_to_js(sqlite_bytes(p, ffi::sqlite3_value_bytes(v))))
            }
            ffi::SQLITE_BLOB => {
                let p = ffi::sqlite3_value_blob(v);
                bytes_to_js(sqlite_bytes(p, ffi::sqlite3_value_bytes(v)), ctx)
            }
            _ => Ok(JsValue::null()),
        }
    }
}

/// Whether `v` is an `ArrayBufferView` (a typed array or DataView).
fn is_view(v: &JsValue) -> bool {
    v.as_object().is_some_and(|o| {
        JsTypedArray::from_object(o.clone()).is_ok()
            || crate::js::JsDataView::from_object(o).is_ok()
    })
}

/// `bigint` as an `i64` when it fits.
fn bigint_to_i64(b: &JsBigInt) -> Option<i64> {
    b.to_string().parse::<i64>().ok()
}

/// Bind `v` to parameter `index` (`StatementSync::BindValue`).
fn bind_value(
    ctx: &mut Context,
    db: &Database,
    raw: *mut ffi::sqlite3_stmt,
    v: &JsValue,
    index: c_int,
) -> JsResult<()> {
    // SAFETY (all binds below): `raw` is a live statement; SQLITE_TRANSIENT makes SQLite copy
    // the bytes before the call returns.
    let r = if let Some(n) = v.as_number() {
        unsafe { ffi::sqlite3_bind_double(raw, index, n) }
    } else if let Some(s) = v.as_string() {
        let s = utf8(&s);
        unsafe {
            ffi::sqlite3_bind_text64(
                raw,
                index,
                s.as_ptr().cast::<c_char>(),
                s.len() as u64,
                ffi::SQLITE_TRANSIENT(),
                ffi::SQLITE_UTF8 as u8,
            )
        }
    } else if v.is_null() {
        unsafe { ffi::sqlite3_bind_null(raw, index) }
    } else if is_view(v) {
        let bytes = bytes_arg(v, ctx)?;
        unsafe {
            ffi::sqlite3_bind_blob64(
                raw,
                index,
                bytes.as_ptr().cast::<c_void>(),
                bytes.len() as u64,
                ffi::SQLITE_TRANSIENT(),
            )
        }
    } else if let Some(b) = v.as_bigint() {
        let Some(i) = bigint_to_i64(&b) else {
            return Err(coded_error(
                ctx,
                "TypeError",
                "BigInt value is too large to bind.",
                &[("code", js_str("ERR_INVALID_ARG_VALUE"))],
            ));
        };
        unsafe { ffi::sqlite3_bind_int64(raw, index, i) }
    } else {
        return Err(coded_error(
            ctx,
            "TypeError",
            &format!("Provided value cannot be bound to SQLite parameter {index}."),
            &[("code", js_str("ERR_INVALID_ARG_TYPE"))],
        ));
    };
    check(ctx, db, r, ffi::SQLITE_OK)
}

/// Bare name -> prefixed name for statement `id`, built once per statement. A conflict throws
/// but keeps the partial map, as Node does.
fn bare_names(
    ctx: &mut Context,
    id: u64,
    raw: *mut ffi::sqlite3_stmt,
) -> JsResult<Rc<HashMap<String, String>>> {
    if let Some(map) = STATEMENTS.with(|s| s.borrow().get(&id).and_then(|st| st.bare.clone())) {
        return Ok(map);
    }
    let mut map = HashMap::new();
    let mut conflict = None;
    // SAFETY: `raw` is a live statement and parameter indexes start at one.
    let count = unsafe { ffi::sqlite3_bind_parameter_count(raw) };
    for i in 1..=count {
        // SAFETY: as above; names are NUL-terminated and owned by the statement.
        let Some(full) = (unsafe { c_str(ffi::sqlite3_bind_parameter_name(raw, i)) }) else {
            continue;
        };
        let bare: String = full.chars().skip(1).collect();
        match map.get(&bare) {
            Some(existing) if *existing != full => {
                conflict = Some(format!(
                    "Cannot create bare named parameter '{bare}' because of conflicting names '{existing}' and '{full}'."
                ));
                break;
            }
            Some(_) => {}
            None => {
                map.insert(bare, full);
            }
        }
    }
    let map = Rc::new(map);
    STATEMENTS.with(|s| {
        if let Some(st) = s.borrow_mut().get_mut(&id) {
            st.bare = Some(map.clone());
        }
    });
    match conflict {
        Some(message) => Err(invalid_state(ctx, &message)),
        None => Ok(map),
    }
}

fn parameter_index(raw: *mut ffi::sqlite3_stmt, name: &str) -> c_int {
    let name = c_text(name);
    // SAFETY: `raw` is a live statement and `name` is NUL-terminated.
    unsafe { ffi::sqlite3_bind_parameter_index(raw, name.as_ptr()) }
}

fn key_text(key: &PropertyKey) -> String {
    match key {
        PropertyKey::Name(s) => s.to_string(),
        PropertyKey::String(s) => utf8(s),
        PropertyKey::Index(i) => i.to_string(),
        PropertyKey::Symbol(_) => String::new(),
    }
}

/// Bind a call's arguments: an optional leading named-parameter object, then anonymous
/// values (`StatementSync::BindParams`).
fn bind_params(
    ctx: &mut Context,
    db: &Database,
    id: u64,
    raw: *mut ffi::sqlite3_stmt,
    flags: u32,
    args: &[JsValue],
) -> JsResult<()> {
    // SAFETY: `raw` is a live statement.
    check(
        ctx,
        db,
        unsafe { ffi::sqlite3_clear_bindings(raw) },
        ffi::SQLITE_OK,
    )?;
    let mut anon_start = 0;
    if let Some(obj) = args
        .first()
        .filter(|v| !is_view(v))
        .and_then(JsValue::as_object)
    {
        let keys = obj.own_enumerable_string_keys(ctx)?;
        let bare = if flags & ALLOW_BARE_NAMED != 0 {
            Some(bare_names(ctx, id, raw)?)
        } else {
            None
        };
        for key in keys {
            let name = key_text(&key);
            let mut index = parameter_index(raw, &name);
            if index == 0
                && let Some(full) = bare.as_ref().and_then(|b| b.get(&name))
            {
                index = parameter_index(raw, full);
            }
            if index == 0 {
                if flags & ALLOW_UNKNOWN_NAMED != 0 {
                    continue;
                }
                return Err(invalid_state(
                    ctx,
                    &format!("Unknown named parameter '{name}'"),
                ));
            }
            let value = obj.get(key, ctx)?;
            bind_value(ctx, db, raw, &value, index)?;
        }
        anon_start = 1;
    }
    let mut anon_index: c_int = 1;
    for value in args.iter().skip(anon_start) {
        loop {
            // SAFETY: `raw` is a live statement; a name pointer is only inspected when set.
            let name = unsafe { ffi::sqlite3_bind_parameter_name(raw, anon_index) };
            if name.is_null() || unsafe { *name } == b'?' as c_char {
                break;
            }
            anon_index += 1;
        }
        bind_value(ctx, db, raw, value, anon_index)?;
        anon_index += 1;
    }
    Ok(())
}

/// Column names of `raw` as property keys.
fn column_keys(
    ctx: &mut Context,
    raw: *mut ffi::sqlite3_stmt,
    count: c_int,
) -> JsResult<Vec<JsString>> {
    (0..count)
        .map(|i| {
            // SAFETY: `raw` is a live statement and `i` below its column count.
            match unsafe { c_str(ffi::sqlite3_column_name(raw, i)) } {
                Some(name) => Ok(JsString::from(name.as_str())),
                None => Err(invalid_state(
                    ctx,
                    &format!("Cannot get name of column {i}"),
                )),
            }
        })
        .collect()
}

/// The current row as an array or a null-prototype object (`keys` caches the column names).
fn read_row(
    ctx: &mut Context,
    raw: *mut ffi::sqlite3_stmt,
    flags: u32,
    keys: &mut Option<Vec<JsString>>,
) -> JsResult<JsValue> {
    // SAFETY: `raw` is a live statement.
    let count = unsafe { ffi::sqlite3_column_count(raw) };
    let big = flags & READ_BIG_INTS != 0;
    let mut values = Vec::with_capacity(count as usize);
    for i in 0..count {
        values.push(column_value(ctx, raw, i, big)?);
    }
    if flags & RETURN_ARRAYS != 0 {
        return Ok(array(ctx, values));
    }
    if keys.is_none() {
        *keys = Some(column_keys(ctx, raw, count)?);
    }
    let row = JsObject::with_null_proto();
    for (key, value) in keys.iter().flatten().zip(values) {
        row.create_data_property_or_throw(key.clone(), value, ctx)?;
    }
    Ok(row.into())
}

// ---------------------------------------------------------------------------
// DatabaseSync
// ---------------------------------------------------------------------------

/// `ops.sqliteOpen(location, readOnly, timeout, foreignKeys, doubleQuotedStrings,
/// allowExtension) -> [id, guard]` (`DatabaseSync::Open`). `location` is a string or the
/// bytes of a `Uint8Array` path.
fn op_open(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    release_collected();
    let location = args.get_or_undefined(0);
    let bytes = if location.is_string() {
        arg_text(args, 0, ctx)?.into_bytes()
    } else {
        bytes_arg(location, ctx)?
    };
    let read_only = args.get_or_undefined(1).to_boolean();
    let timeout = arg_f64(args, 2, ctx)? as c_int;
    let foreign_keys = c_int::from(args.get_or_undefined(3).to_boolean());
    let dqs = c_int::from(args.get_or_undefined(4).to_boolean());
    let allow_extension = args.get_or_undefined(5).to_boolean();
    let path = CString::new(bytes).unwrap_or_default();
    let flags = ffi::SQLITE_OPEN_URI
        | if read_only {
            ffi::SQLITE_OPEN_READONLY
        } else {
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE
        };
    let mut handle = ptr::null_mut();
    // SAFETY: `path` is NUL-terminated and `handle` receives the connection (set even when
    // opening fails, and closed below in that case).
    let configured = unsafe {
        let mut r = ffi::sqlite3_open_v2(path.as_ptr(), &mut handle, flags, ptr::null());
        if r == ffi::SQLITE_OK {
            r = ffi::sqlite3_db_config(
                handle,
                ffi::SQLITE_DBCONFIG_DQS_DML,
                dqs,
                ptr::null_mut::<c_int>(),
            );
        }
        if r == ffi::SQLITE_OK {
            r = ffi::sqlite3_db_config(
                handle,
                ffi::SQLITE_DBCONFIG_DQS_DDL,
                dqs,
                ptr::null_mut::<c_int>(),
            );
        }
        let mut enabled: c_int = 0;
        if r == ffi::SQLITE_OK {
            r = ffi::sqlite3_db_config(
                handle,
                ffi::SQLITE_DBCONFIG_ENABLE_FKEY,
                foreign_keys,
                &mut enabled,
            );
        }
        if r == ffi::SQLITE_OK {
            ffi::sqlite3_busy_timeout(handle, timeout);
            if allow_extension {
                r = ffi::sqlite3_db_config(
                    handle,
                    ffi::SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION,
                    1 as c_int,
                    ptr::null_mut::<c_int>(),
                );
            }
        }
        r
    };
    if configured != ffi::SQLITE_OK {
        let err = error_handle(ctx, handle);
        // SAFETY: the failed connection is closed once and never stored.
        unsafe { sqlite3_close_v2(handle) };
        return Err(err);
    }
    let id = next_id();
    let db = Database {
        handle,
        allow_extension,
        statements: RefCell::default(),
        backups: RefCell::default(),
        pending: RefCell::default(),
        busy: Cell::new(0),
    };
    DATABASES.with(|dbs| dbs.borrow_mut().insert(id, Rc::new(db)));
    let guard = native_object(ReleaseGuard(Some(Release::Database(id))));
    Ok(array(ctx, vec![JsValue::from(id as f64), guard]))
}

/// `ops.sqliteClose(id)` (`DatabaseSync::Close`).
fn op_close(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    if db.busy.get() > 0 {
        return Err(invalid_state(ctx, "database is busy"));
    }
    drop(db);
    match close_database(id) {
        None => Err(invalid_state(ctx, "database is not open")),
        Some((_, ffi::SQLITE_OK)) => Ok(JsValue::undefined()),
        Some((db, _)) => Err(error_handle(ctx, db.handle)),
    }
}

/// `ops.sqliteExec(id, sql)`: run every statement of `sql` (`sqlite3_exec`).
fn op_exec(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let sql = c_text(&arg_text(args, 1, ctx)?);
    let _busy = BusyGuard::new(&db);
    // SAFETY: the connection is open and `sql` is NUL-terminated.
    let r = unsafe {
        ffi::sqlite3_exec(
            db.handle,
            sql.as_ptr(),
            None,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    Ok(JsValue::undefined())
}

/// `ops.sqliteInTransaction(id)`: `!sqlite3_get_autocommit`.
fn op_in_transaction(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    // SAFETY: the connection is open.
    Ok(JsValue::from(
        unsafe { ffi::sqlite3_get_autocommit(db.handle) } == 0,
    ))
}

/// `ops.sqliteLocation(id, dbName) -> path | null` (`sqlite3_db_filename`).
fn op_location(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let name = c_text(&arg_text(args, 1, ctx)?);
    // SAFETY: the connection is open and `name` is NUL-terminated; the result is owned by
    // the connection.
    let file = unsafe { c_str(ffi::sqlite3_db_filename(db.handle, name.as_ptr())) };
    Ok(file
        .filter(|f| !f.is_empty())
        .map_or(JsValue::null(), |f| js_str(&f)))
}

/// `ops.sqlitePrepare(id, sql) -> [statementId, guard]` (`sqlite3_prepare_v2`; only the first
/// statement of `sql` is compiled).
fn op_prepare(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let sql = c_text(&arg_text(args, 1, ctx)?);
    let mut raw = ptr::null_mut();
    // SAFETY: the connection is open and `sql` is NUL-terminated.
    let r =
        unsafe { ffi::sqlite3_prepare_v2(db.handle, sql.as_ptr(), -1, &mut raw, ptr::null_mut()) };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    let stmt_id = next_id();
    STATEMENTS.with(|s| {
        s.borrow_mut().insert(
            stmt_id,
            Statement {
                raw,
                db: id,
                bare: None,
            },
        )
    });
    db.statements.borrow_mut().insert(stmt_id);
    let guard = native_object(ReleaseGuard(Some(Release::Statement(stmt_id))));
    Ok(array(ctx, vec![JsValue::from(stmt_id as f64), guard]))
}

/// `ops.sqliteEnableLoadExtension(id, enable)`.
fn op_enable_load_extension(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let enable = c_int::from(args.get_or_undefined(1).to_boolean());
    // SAFETY: the connection is open.
    let r = unsafe {
        ffi::sqlite3_db_config(
            db.handle,
            ffi::SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION,
            enable,
            ptr::null_mut::<c_int>(),
        )
    };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    Ok(JsValue::undefined())
}

/// `ops.sqliteLoadExtension(id, path, entryPoint | null)` (`sqlite3_load_extension`).
fn op_load_extension(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    if !db.allow_extension {
        return Err(invalid_state(ctx, "extension loading is not allowed"));
    }
    let path = c_text(&arg_text(args, 1, ctx)?);
    let entry = args
        .get_or_undefined(2)
        .as_string()
        .map(|s| c_text(&utf8(&s)));
    let mut message: *mut c_char = ptr::null_mut();
    // SAFETY: the connection is open, the strings are NUL-terminated, and SQLite allocates
    // `message`, freed below.
    let (r, message) = unsafe {
        let r = ffi::sqlite3_load_extension(
            db.handle,
            path.as_ptr(),
            entry.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
            &mut message,
        );
        let text = c_str(message);
        ffi::sqlite3_free(message.cast::<c_void>());
        (r, text)
    };
    if r != ffi::SQLITE_OK {
        return Err(coded_error(
            ctx,
            "Error",
            &message.unwrap_or_default(),
            &[("code", js_str("ERR_LOAD_SQLITE_EXTENSION"))],
        ));
    }
    Ok(JsValue::undefined())
}

// ---------------------------------------------------------------------------
// User functions and aggregates
// ---------------------------------------------------------------------------

/// A JS scalar function (`UserDefinedFunction`).
struct Function {
    ctx: Context,
    db: Weak<Database>,
    func: JsFunction,
    big: bool,
}

/// A JS aggregate or window function (`CustomAggregate`).
struct Aggregate {
    ctx: Context,
    db: Weak<Database>,
    big: bool,
    start: JsValue,
    step: JsFunction,
    inverse: Option<JsFunction>,
    result: Option<JsFunction>,
}

/// Per-group state of an [`Aggregate`], owned through SQLite's aggregate context.
struct AggregateState {
    value: JsValue,
    is_window: bool,
}

/// Report a JS exception from a callback: park it on the connection and fail the call with an
/// empty SQLite error that [`sqlite_error`] replaces with it.
fn fail(sctx: *mut ffi::sqlite3_context, db: &Weak<Database>, e: JsError) {
    if let Some(db) = db.upgrade() {
        *db.pending.borrow_mut() = Some(e);
    }
    // SAFETY: `sctx` is the running callback's context.
    unsafe { ffi::sqlite3_result_error(sctx, c"".as_ptr(), 0) };
}

fn result_error(sctx: *mut ffi::sqlite3_context, message: &CStr) {
    // SAFETY: `sctx` is the running callback's context; SQLite copies the message.
    unsafe { ffi::sqlite3_result_error(sctx, message.as_ptr(), -1) };
}

/// Set a callback's result from a JS value (`JSValueToSQLiteResult`).
fn set_result(ctx: &mut Context, sctx: *mut ffi::sqlite3_context, v: &JsValue) {
    // SAFETY (all results below): `sctx` is the running callback's context and
    // SQLITE_TRANSIENT makes SQLite copy the bytes.
    if v.is_null_or_undefined() {
        unsafe { ffi::sqlite3_result_null(sctx) };
    } else if let Some(n) = v.as_number() {
        unsafe { ffi::sqlite3_result_double(sctx, n) };
    } else if let Some(s) = v.as_string() {
        let s = utf8(&s);
        unsafe {
            ffi::sqlite3_result_text64(
                sctx,
                s.as_ptr().cast::<c_char>(),
                s.len() as u64,
                ffi::SQLITE_TRANSIENT(),
                ffi::SQLITE_UTF8 as u8,
            )
        };
    } else if is_view(v) {
        match bytes_arg(v, ctx) {
            Ok(bytes) => unsafe {
                ffi::sqlite3_result_blob64(
                    sctx,
                    bytes.as_ptr().cast::<c_void>(),
                    bytes.len() as u64,
                    ffi::SQLITE_TRANSIENT(),
                )
            },
            Err(_) => result_error(
                sctx,
                c"Returned JavaScript value cannot be converted to a SQLite value",
            ),
        }
    } else if let Some(b) = v.as_bigint() {
        match bigint_to_i64(&b) {
            Some(i) => unsafe { ffi::sqlite3_result_int64(sctx, i) },
            None => result_error(sctx, c"BigInt value is too large for SQLite"),
        }
    } else if v.is_promise() {
        result_error(
            sctx,
            c"Asynchronous user-defined functions are not supported",
        );
    } else {
        result_error(
            sctx,
            c"Returned JavaScript value cannot be converted to a SQLite value",
        );
    }
}

/// Run a callback body, turning a Rust panic into a SQLite error instead of unwinding into C.
fn guard_panic(sctx: *mut ffi::sqlite3_context, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        result_error(sctx, c"internal error in a JavaScript callback");
    }
}

/// Convert a callback's SQLite arguments.
///
/// # Safety
/// `argv` holds `argc` values of the running callback.
unsafe fn callback_args(
    ctx: &mut Context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
    big: bool,
) -> JsResult<Vec<JsValue>> {
    (0..argc.max(0) as usize)
        // SAFETY: guaranteed by the caller.
        .map(|i| sqlite_value_to_js(ctx, unsafe { *argv.add(i) }, big))
        .collect()
}

unsafe extern "C" fn function_call(
    sctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    guard_panic(sctx, || {
        // SAFETY: the user data is the `Function` registered with this callback, alive until
        // its destructor runs.
        let f = unsafe { &*ffi::sqlite3_user_data(sctx).cast::<Function>() };
        let mut ctx = f.ctx.clone();
        // SAFETY: SQLite passes `argc` arguments.
        let args = match unsafe { callback_args(&mut ctx, argc, argv, f.big) } {
            Ok(args) => args,
            Err(e) => return fail(sctx, &f.db, e),
        };
        match f.func.call(&JsValue::undefined(), &args, &mut ctx) {
            Ok(v) => set_result(&mut ctx, sctx, &v),
            Err(e) => fail(sctx, &f.db, e),
        }
    });
}

unsafe extern "C" fn function_destroy(p: *mut c_void) {
    // SAFETY: `p` came from `Box::into_raw` in `op_function`; SQLite destroys it once.
    drop(unsafe { Box::from_raw(p.cast::<Function>()) });
}

/// `ops.sqliteFunction(id, name, fn, argc, deterministic, directOnly, useBigIntArguments)`
/// (`sqlite3_create_function_v2`).
fn op_function(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let name = c_text(&arg_text(args, 1, ctx)?);
    let Some(func) = args.get_or_undefined(2).as_callable() else {
        return Err(crate::engine::type_error("function expected"));
    };
    let argc = arg_f64(args, 3, ctx)? as c_int;
    let mut text_rep = ffi::SQLITE_UTF8;
    if args.get_or_undefined(4).to_boolean() {
        text_rep |= ffi::SQLITE_DETERMINISTIC;
    }
    if args.get_or_undefined(5).to_boolean() {
        text_rep |= ffi::SQLITE_DIRECTONLY;
    }
    let data = Box::into_raw(Box::new(Function {
        ctx: ctx.clone(),
        db: Rc::downgrade(&db),
        func,
        big: args.get_or_undefined(6).to_boolean(),
    }));
    // SAFETY: the connection is open; SQLite owns `data` from here and frees it through
    // `function_destroy` (also when registration fails).
    let r = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle,
            name.as_ptr(),
            argc,
            text_rep,
            data.cast::<c_void>(),
            Some(function_call),
            None,
            None,
            Some(function_destroy),
        )
    };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    Ok(JsValue::undefined())
}

/// The state of the current group, initialized from `start` on first use
/// (`CustomAggregate::GetAggregate`).
fn aggregate_state(
    ctx: &mut Context,
    sctx: *mut ffi::sqlite3_context,
    agg: &Aggregate,
) -> Option<*mut AggregateState> {
    // SAFETY: `sctx` is a running aggregate callback; the context memory is zeroed on first
    // allocation and holds one pointer owned by this module.
    let slot = unsafe {
        ffi::sqlite3_aggregate_context(sctx, std::mem::size_of::<*mut AggregateState>() as c_int)
            .cast::<*mut AggregateState>()
    };
    if slot.is_null() {
        // SAFETY: as above.
        unsafe { ffi::sqlite3_result_error_nomem(sctx) };
        return None;
    }
    // SAFETY: `slot` is valid and aligned for a pointer.
    if unsafe { (*slot).is_null() } {
        let mut start = agg.start.clone();
        if let Some(f) = start.as_callable() {
            match f.call(&JsValue::null(), &[], ctx) {
                Ok(v) => start = v,
                Err(e) => {
                    fail(sctx, &agg.db, e);
                    return None;
                }
            }
        }
        let state = Box::into_raw(Box::new(AggregateState {
            value: start,
            is_window: false,
        }));
        // SAFETY: as above.
        unsafe { *slot = state };
    }
    // SAFETY: as above.
    Some(unsafe { *slot })
}

/// Drop the state of the current group.
fn destroy_aggregate_state(sctx: *mut ffi::sqlite3_context) {
    // SAFETY: `sctx` is a running aggregate callback; asking for zero bytes never allocates.
    let slot = unsafe { ffi::sqlite3_aggregate_context(sctx, 0).cast::<*mut AggregateState>() };
    // SAFETY: a non-null slot holds null or a pointer from `Box::into_raw`, reset here so it
    // is freed once.
    unsafe {
        if !slot.is_null() && !(*slot).is_null() {
            drop(Box::from_raw(*slot));
            *slot = ptr::null_mut();
        }
    }
}

/// `xStep` / `xInverse`: fold the row into the state with `step` or `inverse`.
///
/// # Safety
/// Called by SQLite with an [`Aggregate`] as user data and `argc` arguments.
unsafe fn aggregate_step(
    sctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
    inverse: bool,
) {
    guard_panic(sctx, || {
        // SAFETY: the user data is the registered `Aggregate`, alive until its destructor.
        let agg = unsafe { &*ffi::sqlite3_user_data(sctx).cast::<Aggregate>() };
        let mut ctx = agg.ctx.clone();
        let Some(state) = aggregate_state(&mut ctx, sctx, agg) else {
            return;
        };
        // SAFETY: `state` is the live state of this group.
        let mut call_args = vec![unsafe { (*state).value.clone() }];
        // SAFETY: SQLite passes `argc` arguments.
        match unsafe { callback_args(&mut ctx, argc, argv, agg.big) } {
            Ok(args) => call_args.extend(args),
            Err(e) => return fail(sctx, &agg.db, e),
        }
        let func = if inverse {
            agg.inverse.as_ref()
        } else {
            Some(&agg.step)
        };
        let Some(func) = func else { return };
        match func.call(&JsValue::undefined(), &call_args, &mut ctx) {
            // SAFETY: as above.
            Ok(v) => unsafe { (*state).value = v },
            Err(e) => fail(sctx, &agg.db, e),
        }
    });
}

unsafe extern "C" fn aggregate_x_step(
    sctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    // SAFETY: registered as the step callback of an `Aggregate`.
    unsafe { aggregate_step(sctx, argc, argv, false) }
}

unsafe extern "C" fn aggregate_x_inverse(
    sctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    // SAFETY: registered as the inverse callback of an `Aggregate`.
    unsafe { aggregate_step(sctx, argc, argv, true) }
}

/// `xValue` / `xFinal` (`CustomAggregate::xValueBase`).
fn aggregate_value(sctx: *mut ffi::sqlite3_context, is_final: bool) {
    guard_panic(sctx, || {
        // SAFETY: the user data is the registered `Aggregate`, alive until its destructor.
        let agg = unsafe { &*ffi::sqlite3_user_data(sctx).cast::<Aggregate>() };
        let mut ctx = agg.ctx.clone();
        let Some(state) = aggregate_state(&mut ctx, sctx, agg) else {
            return;
        };
        // SAFETY: `state` is the live state of this group until destroyed below.
        let state = unsafe { &mut *state };
        if !is_final {
            state.is_window = true;
        } else if state.is_window {
            destroy_aggregate_state(sctx);
            return;
        }
        let result = match &agg.result {
            Some(f) => match f.call(
                &JsValue::null(),
                std::slice::from_ref(&state.value),
                &mut ctx,
            ) {
                Ok(v) => Some(v),
                Err(e) => {
                    fail(sctx, &agg.db, e);
                    None
                }
            },
            None => Some(state.value.clone()),
        };
        if let Some(v) = result {
            set_result(&mut ctx, sctx, &v);
        }
        if is_final {
            destroy_aggregate_state(sctx);
        }
    });
}

unsafe extern "C" fn aggregate_x_final(sctx: *mut ffi::sqlite3_context) {
    aggregate_value(sctx, true);
}

unsafe extern "C" fn aggregate_x_value(sctx: *mut ffi::sqlite3_context) {
    aggregate_value(sctx, false);
}

unsafe extern "C" fn aggregate_destroy(p: *mut c_void) {
    // SAFETY: `p` came from `Box::into_raw` in `op_aggregate`; SQLite destroys it once.
    drop(unsafe { Box::from_raw(p.cast::<Aggregate>()) });
}

/// `ops.sqliteAggregate(id, name, argc, directOnly, useBigIntArguments, start, step, inverse,
/// result)` (`sqlite3_create_window_function`; `inverse` and `result` may be undefined).
fn op_aggregate(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = db_id(args, ctx)?;
    let db = database(ctx, id)?;
    let name = c_text(&arg_text(args, 1, ctx)?);
    let argc = arg_f64(args, 2, ctx)? as c_int;
    let mut text_rep = ffi::SQLITE_UTF8;
    if args.get_or_undefined(3).to_boolean() {
        text_rep |= ffi::SQLITE_DIRECTONLY;
    }
    let Some(step) = args.get_or_undefined(6).as_callable() else {
        return Err(crate::engine::type_error("step function expected"));
    };
    let inverse = args.get_or_undefined(7).as_callable();
    let windowed = inverse.is_some();
    let data = Box::into_raw(Box::new(Aggregate {
        ctx: ctx.clone(),
        db: Rc::downgrade(&db),
        big: args.get_or_undefined(4).to_boolean(),
        start: args.get_or_undefined(5).clone(),
        step,
        inverse,
        result: args.get_or_undefined(8).as_callable(),
    }));
    // SAFETY: the connection is open; SQLite owns `data` from here and frees it through
    // `aggregate_destroy` (also when registration fails).
    let r = unsafe {
        ffi::sqlite3_create_window_function(
            db.handle,
            name.as_ptr(),
            argc,
            text_rep,
            data.cast::<c_void>(),
            Some(aggregate_x_step),
            Some(aggregate_x_final),
            if windowed {
                Some(aggregate_x_value)
            } else {
                None
            },
            if windowed {
                Some(aggregate_x_inverse)
            } else {
                None
            },
            Some(aggregate_destroy),
        )
    };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    Ok(JsValue::undefined())
}

// ---------------------------------------------------------------------------
// StatementSync
// ---------------------------------------------------------------------------

fn stmt_args(args: &[JsValue], ctx: &mut Context) -> JsResult<(u64, u32)> {
    Ok((arg_f64(args, 0, ctx)? as u64, arg_f64(args, 1, ctx)? as u32))
}

/// Reset, then bind `args[2..]` (the start of every execution).
fn begin(
    ctx: &mut Context,
    id: u64,
    flags: u32,
    args: &[JsValue],
) -> JsResult<(*mut ffi::sqlite3_stmt, Rc<Database>)> {
    let (raw, db) = statement(ctx, id)?;
    // SAFETY: `raw` is a live statement.
    check(ctx, &db, unsafe { ffi::sqlite3_reset(raw) }, ffi::SQLITE_OK)?;
    bind_params(ctx, &db, id, raw, flags, args.get(2..).unwrap_or_default())?;
    Ok((raw, db))
}

/// `ops.sqliteRun(id, flags, ...params) -> {lastInsertRowid, changes}`.
fn op_run(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (id, flags) = stmt_args(args, ctx)?;
    let (raw, db) = begin(ctx, id, flags, args)?;
    let (r, rowid, changes) = {
        let _busy = BusyGuard::new(&db);
        // SAFETY: `raw` is a live statement of the open connection `db`.
        unsafe {
            ffi::sqlite3_step(raw);
            let r = ffi::sqlite3_reset(raw);
            (
                r,
                ffi::sqlite3_last_insert_rowid(db.handle),
                ffi::sqlite3_changes64(db.handle),
            )
        }
    };
    check(ctx, &db, r, ffi::SQLITE_OK)?;
    let (rowid, changes) = if flags & READ_BIG_INTS != 0 {
        (
            JsValue::from(JsBigInt::from(rowid)),
            JsValue::from(JsBigInt::from(changes)),
        )
    } else {
        (JsValue::from(rowid as f64), JsValue::from(changes as f64))
    };
    object(ctx, vec![("lastInsertRowid", rowid), ("changes", changes)])
}

/// `ops.sqliteGet(id, flags, ...params) -> row | undefined`.
fn op_get(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (id, flags) = stmt_args(args, ctx)?;
    let (raw, db) = begin(ctx, id, flags, args)?;
    let _reset = ResetGuard(raw);
    let r = {
        let _busy = BusyGuard::new(&db);
        // SAFETY: `raw` is a live statement.
        unsafe { ffi::sqlite3_step(raw) }
    };
    if r == ffi::SQLITE_DONE {
        return Ok(JsValue::undefined());
    }
    if r != ffi::SQLITE_ROW {
        return Err(sqlite_error(ctx, &db));
    }
    // SAFETY: `raw` is a live statement.
    if unsafe { ffi::sqlite3_column_count(raw) } == 0 {
        return Ok(JsValue::undefined());
    }
    read_row(ctx, raw, flags, &mut None)
}

/// `ops.sqliteAll(id, flags, ...params) -> rows`.
fn op_all(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (id, flags) = stmt_args(args, ctx)?;
    let (raw, db) = begin(ctx, id, flags, args)?;
    let _reset = ResetGuard(raw);
    let mut rows = Vec::new();
    let mut keys = None;
    let r = loop {
        let r = {
            let _busy = BusyGuard::new(&db);
            // SAFETY: `raw` is a live statement.
            unsafe { ffi::sqlite3_step(raw) }
        };
        if r != ffi::SQLITE_ROW {
            break r;
        }
        rows.push(read_row(ctx, raw, flags, &mut keys)?);
    };
    check(ctx, &db, r, ffi::SQLITE_DONE)?;
    Ok(array(ctx, rows))
}

/// `ops.sqliteIterate(id, flags, ...params)`: reset and bind for a lazy iteration.
fn op_iterate(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (id, flags) = stmt_args(args, ctx)?;
    begin(ctx, id, flags, args)?;
    Ok(JsValue::undefined())
}

/// `ops.sqliteIterateNext(id, flags) -> row | undefined` (undefined once done; the statement
/// is then reset).
fn op_iterate_next(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (id, flags) = stmt_args(args, ctx)?;
    let (raw, db) = statement(ctx, id)?;
    let r = {
        let _busy = BusyGuard::new(&db);
        // SAFETY: `raw` is a live statement.
        unsafe { ffi::sqlite3_step(raw) }
    };
    if r != ffi::SQLITE_ROW {
        check(ctx, &db, r, ffi::SQLITE_DONE)?;
        // SAFETY: `raw` is a live statement.
        unsafe { ffi::sqlite3_reset(raw) };
        return Ok(JsValue::undefined());
    }
    read_row(ctx, raw, flags, &mut None)
}

/// `ops.sqliteReset(id)`: end an iteration early (`StatementSyncIterator::Return`); also
/// checks that the statement is live.
fn op_reset(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let (raw, _) = statement(ctx, id)?;
    if args.get_or_undefined(1).to_boolean() {
        // SAFETY: `raw` is a live statement.
        unsafe { ffi::sqlite3_reset(raw) };
    }
    Ok(JsValue::undefined())
}

/// `ops.sqliteColumns(id) -> [{column, database, name, table, type}]`.
fn op_columns(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let (raw, _) = statement(ctx, id)?;
    // SAFETY: `raw` is a live statement.
    let count = unsafe { ffi::sqlite3_column_count(raw) };
    let names = column_keys(ctx, raw, count)?;
    let nullable = |s: Option<String>| s.map_or(JsValue::null(), |s| js_str(&s));
    let mut cols = Vec::with_capacity(count as usize);
    for (i, name) in (0..count).zip(names) {
        // SAFETY: `raw` is a live statement and `i` below its column count.
        let (column, database, table, ty) = unsafe {
            (
                c_str(ffi::sqlite3_column_origin_name(raw, i)),
                c_str(ffi::sqlite3_column_database_name(raw, i)),
                c_str(ffi::sqlite3_column_table_name(raw, i)),
                c_str(ffi::sqlite3_column_decltype(raw, i)),
            )
        };
        let col = JsObject::with_null_proto();
        col.create_data_property_or_throw(JsString::from("column"), nullable(column), ctx)?;
        col.create_data_property_or_throw(JsString::from("database"), nullable(database), ctx)?;
        col.create_data_property_or_throw(JsString::from("name"), name, ctx)?;
        col.create_data_property_or_throw(JsString::from("table"), nullable(table), ctx)?;
        col.create_data_property_or_throw(JsString::from("type"), nullable(ty), ctx)?;
        cols.push(col.into());
    }
    Ok(array(ctx, cols))
}

/// `ops.sqliteSourceSql(id)` (`sqlite3_sql`).
fn op_source_sql(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let (raw, _) = statement(ctx, id)?;
    // SAFETY: `raw` is a live statement; the text is owned by it.
    Ok(js_str(
        &unsafe { c_str(ffi::sqlite3_sql(raw)) }.unwrap_or_default(),
    ))
}

/// `ops.sqliteExpandedSql(id)` (`sqlite3_expanded_sql`).
fn op_expanded_sql(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let (raw, _) = statement(ctx, id)?;
    // SAFETY: `raw` is a live statement; the expansion is allocated by SQLite and freed here.
    let expanded = unsafe {
        let p = ffi::sqlite3_expanded_sql(raw);
        let text = c_str(p);
        ffi::sqlite3_free(p.cast::<c_void>());
        text
    };
    match expanded {
        Some(text) => Ok(js_str(&text)),
        None => Err(error_message(
            ctx,
            "Expanded SQL text would exceed configured limits",
        )),
    }
}

// ---------------------------------------------------------------------------
// backup()
// ---------------------------------------------------------------------------

/// `ops.sqliteBackupInit(id, destination, sourceDb, targetDb) -> backupId`: open the
/// destination and start the backup (`BackupJob::ScheduleBackup`).
fn op_backup_init(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let source = db_id(args, ctx)?;
    let db = database(ctx, source)?;
    let path = c_text(&arg_text(args, 1, ctx)?);
    let source_name = c_text(&arg_text(args, 2, ctx)?);
    let dest_name = c_text(&arg_text(args, 3, ctx)?);
    let mut dest = ptr::null_mut();
    // SAFETY: the strings are NUL-terminated; `dest` receives a connection (closed below on
    // failure) and the source connection is open.
    let backup = unsafe {
        let r = ffi::sqlite3_open_v2(
            path.as_ptr(),
            &mut dest,
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE,
            ptr::null(),
        );
        if r == ffi::SQLITE_OK {
            ffi::sqlite3_backup_init(dest, dest_name.as_ptr(), db.handle, source_name.as_ptr())
        } else {
            ptr::null_mut()
        }
    };
    if backup.is_null() {
        let err = error_handle(ctx, dest);
        // SAFETY: the destination is closed once and never stored.
        unsafe { sqlite3_close_v2(dest) };
        return Err(err);
    }
    let id = next_id();
    BACKUPS.with(|t| {
        t.borrow_mut().insert(
            id,
            Backup {
                source,
                dest,
                backup,
            },
        )
    });
    db.backups.borrow_mut().insert(id);
    Ok(JsValue::from(id as f64))
}

/// `ops.sqliteBackupStep(backupId, pages) -> [done, totalPages, remainingPages]`; a hard
/// failure ends the backup and throws.
fn op_backup_step(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let pages = arg_f64(args, 1, ctx)? as c_int;
    let Some(backup) = BACKUPS.with(|t| t.borrow().get(&id).map(|b| b.backup)) else {
        return Err(invalid_state(ctx, "database is not open"));
    };
    // SAFETY: the backup is live (it is in the table).
    let (status, total, remaining) = unsafe {
        let status = ffi::sqlite3_backup_step(backup, pages);
        (
            status,
            ffi::sqlite3_backup_pagecount(backup),
            ffi::sqlite3_backup_remaining(backup),
        )
    };
    if !matches!(
        status,
        ffi::SQLITE_OK | ffi::SQLITE_DONE | ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED
    ) {
        cleanup_backup(id);
        return Err(error_code(ctx, status));
    }
    let done = JsValue::from(status == ffi::SQLITE_DONE);
    Ok(array(
        ctx,
        vec![done, JsValue::from(total), JsValue::from(remaining)],
    ))
}

/// `ops.sqliteBackupFinish(backupId, failed)`: end the backup; with `failed`, throw the
/// destination's error first.
fn op_backup_finish(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let failed = args.get_or_undefined(1).to_boolean();
    let err = if failed {
        BACKUPS
            .with(|t| t.borrow().get(&id).map(|b| b.dest))
            .map(|dest| error_handle(ctx, dest))
    } else {
        None
    };
    cleanup_backup(id);
    match err {
        Some(e) => Err(e),
        None => Ok(JsValue::undefined()),
    }
}

pub(crate) const OPS: &[(&str, usize, OpFn)] = &[
    ("sqliteOpen", 6, op_open),
    ("sqliteClose", 1, op_close),
    ("sqliteExec", 2, op_exec),
    ("sqliteInTransaction", 1, op_in_transaction),
    ("sqliteLocation", 2, op_location),
    ("sqlitePrepare", 2, op_prepare),
    ("sqliteEnableLoadExtension", 2, op_enable_load_extension),
    ("sqliteLoadExtension", 3, op_load_extension),
    ("sqliteFunction", 7, op_function),
    ("sqliteAggregate", 9, op_aggregate),
    ("sqliteRun", 2, op_run),
    ("sqliteGet", 2, op_get),
    ("sqliteAll", 2, op_all),
    ("sqliteIterate", 2, op_iterate),
    ("sqliteIterateNext", 2, op_iterate_next),
    ("sqliteReset", 2, op_reset),
    ("sqliteColumns", 1, op_columns),
    ("sqliteSourceSql", 1, op_source_sql),
    ("sqliteExpandedSql", 1, op_expanded_sql),
    ("sqliteBackupInit", 4, op_backup_init),
    ("sqliteBackupStep", 2, op_backup_step),
    ("sqliteBackupFinish", 2, op_backup_finish),
];

/// Close every connection of this runtime (finalizing statements and dropping user functions
/// while the engine is still alive).
pub(crate) fn teardown(_ctx: &mut Context) {
    let _ = RELEASED.try_with(|r| r.borrow_mut().clear());
    let ids: Vec<u64> = DATABASES.with(|dbs| dbs.borrow().keys().copied().collect());
    for id in ids {
        close_database(id);
    }
    STATEMENTS.with(|s| s.borrow_mut().clear());
    BACKUPS.with(|t| t.borrow_mut().clear());
}
