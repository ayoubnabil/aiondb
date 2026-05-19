//! PostgreSQL large-object (`lo_*` / `pg_largeobject`) compat API.
//!
//! Split out of `scalar_functions/mod.rs`. Self-contained in-memory
//! large-object registry + the `eval_lo_*` evaluators dispatched by
//! `eval_scalar_function`. Parent scope reached via `use super::*`.
#![allow(clippy::too_many_lines)]

use super::*;

#[derive(Default)]
pub(super) struct LargeObjectRegistry {
    pub(super) next_oid: i32,
    pub(super) objects: HashMap<i32, Vec<u8>>,
    pub(super) sessions: HashMap<u64, LargeObjectSessionState>,
    pub(super) brin_ranges: HashMap<i32, BTreeSet<i64>>,
}

pub(super) const MAX_COMPAT_LARGE_OBJECT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) struct LargeObjectSessionState {
    pub(super) next_fd: i32,
    pub(super) fds: HashMap<i32, LargeObjectFdState>,
}

#[derive(Clone, Copy)]
pub(super) struct LargeObjectFdState {
    pub(super) oid: i32,
    pub(super) position: usize,
}

pub(super) fn lo_registry() -> &'static Mutex<LargeObjectRegistry> {
    static REGISTRY: OnceLock<Mutex<LargeObjectRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = LargeObjectRegistry::default();
        registry.next_oid = 16_384;
        Mutex::new(registry)
    })
}

pub(super) fn lo_session_key() -> u64 {
    let key = current_lo_session_key();
    if key == 0 {
        1
    } else {
        key
    }
}

pub(super) fn lo_arg_i32(value: &Value, function_name: &str, arg_name: &str) -> DbResult<i32> {
    match value {
        Value::Int(v) => Ok(*v),
        Value::BigInt(v) => i32::try_from(*v).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("{function_name}() {arg_name} is out of range"),
            )
        }),
        Value::Text(text) => text.trim().parse::<i32>().map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{function_name}() {arg_name} must be integer"),
            )
        }),
        _ => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be integer"),
        )),
    }
}

pub(super) fn lo_arg_usize(value: &Value, function_name: &str, arg_name: &str) -> DbResult<usize> {
    let signed = lo_arg_i32(value, function_name, arg_name)?;
    if signed < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be non-negative"),
        ));
    }
    usize::try_from(signed).map_err(|_| {
        DbError::bind_error(
            SqlState::NumericValueOutOfRange,
            format!("{function_name}() {arg_name} is out of range"),
        )
    })
}

pub(super) fn lo_arg_i64(value: &Value, function_name: &str, arg_name: &str) -> DbResult<i64> {
    match value {
        Value::Int(v) => Ok(i64::from(*v)),
        Value::BigInt(v) => Ok(*v),
        Value::Text(text) => text.trim().parse::<i64>().map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{function_name}() {arg_name} must be integer"),
            )
        }),
        _ => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be integer"),
        )),
    }
}

pub(super) fn lo_object_not_found(oid: i32) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("large object {oid} does not exist"),
    )
}

pub(super) fn lo_bad_fd(fd: i32) -> DbError {
    DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!("invalid large-object descriptor: {fd}"),
    )
}

pub(super) fn lo_space_exhausted(kind: &str) -> DbError {
    DbError::program_limit(format!("large object {kind} space exhausted"))
}

pub(super) fn next_available_lo_oid(registry: &LargeObjectRegistry) -> DbResult<i32> {
    let mut candidate = registry.next_oid.max(16_384);
    loop {
        if !registry.objects.contains_key(&candidate) {
            return Ok(candidate);
        }
        candidate = candidate
            .checked_add(1)
            .ok_or_else(|| lo_space_exhausted("OID"))?;
    }
}

pub(super) fn advance_next_lo_oid(registry: &mut LargeObjectRegistry, oid: i32) {
    if let Some(next) = oid.checked_add(1) {
        registry.next_oid = registry.next_oid.max(next);
    } else {
        registry.next_oid = i32::MAX;
    }
}

pub(super) fn next_available_lo_fd(session: &LargeObjectSessionState) -> DbResult<i32> {
    let mut fd = session.next_fd.max(1);
    loop {
        if !session.fds.contains_key(&fd) {
            return Ok(fd);
        }
        fd = fd
            .checked_add(1)
            .ok_or_else(|| lo_space_exhausted("descriptor"))?;
    }
}

pub(super) fn advance_next_lo_fd(session: &mut LargeObjectSessionState, fd: i32) {
    if let Some(next) = fd.checked_add(1) {
        session.next_fd = session.next_fd.max(next);
    } else {
        session.next_fd = i32::MAX;
    }
}

pub(super) fn checked_lo_size(function_name: &str, size: usize) -> DbResult<()> {
    if size > MAX_COMPAT_LARGE_OBJECT_BYTES {
        return Err(DbError::program_limit(format!(
            "{function_name}() large object size {size} exceeds maximum {MAX_COMPAT_LARGE_OBJECT_BYTES}"
        )));
    }
    Ok(())
}

pub(super) fn checked_lo_write_range(
    function_name: &str,
    offset: i64,
    bytes_len: usize,
) -> DbResult<(usize, usize)> {
    let start = usize::try_from(offset).map_err(|_| {
        DbError::program_limit(format!("{function_name}() offset does not fit in usize"))
    })?;
    let end = start.checked_add(bytes_len).ok_or_else(|| {
        DbError::program_limit(format!("{function_name}() large object size overflow"))
    })?;
    checked_lo_size(function_name, end)?;
    Ok((start, end))
}

pub(super) fn eval_lo_create(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_create")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let requested = lo_arg_i32(&args[0], "lo_create", "oid")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let oid = if requested > 0 {
        // Keep deterministic OID behavior for explicit lo_create(oid) calls.
        // Re-create/reset the object when the OID already exists.
        requested
    } else {
        let candidate = next_available_lo_oid(&registry)?;
        advance_next_lo_oid(&mut registry, candidate);
        candidate
    };
    registry.objects.insert(oid, Vec::new());
    advance_next_lo_oid(&mut registry, oid);
    Ok(Value::Int(oid))
}

pub(super) fn eval_lo_open(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_open")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_open", "oid")?;
    let _mode = lo_arg_i32(&args[1], "lo_open", "mode")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    if !registry.objects.contains_key(&oid) {
        return Err(lo_object_not_found(oid));
    }
    let session = registry.sessions.entry(lo_session_key()).or_default();
    let fd = next_available_lo_fd(session)?;
    advance_next_lo_fd(session, fd);
    session
        .fds
        .insert(fd, LargeObjectFdState { oid, position: 0 });
    Ok(Value::Int(fd))
}

pub(super) fn eval_lo_close(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_close")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_close", "fd")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let Some(session) = registry.sessions.get_mut(&lo_session_key()) else {
        return Err(lo_bad_fd(fd));
    };
    if session.fds.remove(&fd).is_some() {
        Ok(Value::Int(0))
    } else {
        Err(lo_bad_fd(fd))
    }
}

pub(super) fn eval_loread(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "loread")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "loread", "fd")?;
    let len = lo_arg_usize(&args[1], "loread", "len")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, start) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get_mut(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let Some(data) = registry.objects.get(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    let start = start.min(data.len());
    let end = start.saturating_add(len).min(data.len());
    let chunk = data[start..end].to_vec();
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = end;
        }
    }
    Ok(Value::Blob(chunk))
}

pub(super) fn eval_lowrite(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lowrite")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lowrite", "fd")?;
    let bytes = match &args[1] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, start) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get_mut(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let Some(data) = registry.objects.get_mut(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    let start = start.min(data.len());
    let needed = start
        .checked_add(bytes.len())
        .ok_or_else(|| DbError::program_limit("lowrite() large object size overflow".to_owned()))?;
    checked_lo_size("lowrite", needed)?;
    if data.len() < needed {
        data.resize(needed, 0);
    }
    data[start..start + bytes.len()].copy_from_slice(&bytes);
    let end = start + bytes.len();
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = end;
        }
    }
    Ok(Value::Int(i32::try_from(bytes.len()).unwrap_or(i32::MAX)))
}

pub(super) fn eval_lo_unlink(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_unlink")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_unlink", "oid")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    if registry.objects.remove(&oid).is_none() {
        return Err(lo_object_not_found(oid));
    }
    for session in registry.sessions.values_mut() {
        session.fds.retain(|_, state| state.oid != oid);
    }
    Ok(Value::Int(1))
}

/// `lo_lseek(fd, offset, whence) → integer`. PG defines `whence` as
/// `SEEK_SET=0`, `SEEK_CUR=1`, `SEEK_END=2`. The result is the resulting
/// position (clamped to `[0, length(lo)]`). Negative offsets are allowed and
/// move the cursor backwards. `bigint=true` selects the `lo_lseek64` variant
/// returning int8 instead of int4.
pub(super) fn eval_lo_lseek(args: &[Value], bigint: bool) -> DbResult<Value> {
    expect_args(args, 3, "lo_lseek")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_lseek", "fd")?;
    let offset = lo_arg_i64(&args[1], "lo_lseek", "offset")?;
    let whence = lo_arg_i32(&args[2], "lo_lseek", "whence")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, current) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let length = registry
        .objects
        .get(&oid)
        .map(Vec::len)
        .ok_or_else(|| lo_object_not_found(oid))?;
    let base: i64 = match whence {
        0 => 0,
        1 => i64::try_from(current).unwrap_or(i64::MAX),
        2 => i64::try_from(length).unwrap_or(i64::MAX),
        _ => {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid lo_lseek whence: {whence}"),
            ));
        }
    };
    let new_pos_signed = base.saturating_add(offset);
    let new_pos = if new_pos_signed < 0 {
        0
    } else {
        new_pos_signed
    };
    let new_pos_usize = usize::try_from(new_pos).unwrap_or(usize::MAX);
    let new_pos_usize = new_pos_usize.min(length);
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = new_pos_usize;
        }
    }
    let returned = i64::try_from(new_pos_usize).unwrap_or(i64::MAX);
    Ok(if bigint {
        Value::BigInt(returned)
    } else {
        Value::Int(i32::try_from(returned).unwrap_or(i32::MAX))
    })
}

/// `lo_creat(mode int) → oid`. The `mode` argument is unused per PG history.
/// Allocates a fresh LO oid in the session-shared registry.
pub(super) fn eval_lo_creat(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_creat")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // Mode is read but ignored, matching PostgreSQL semantics.
    let _mode = lo_arg_i32(&args[0], "lo_creat", "mode")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let candidate = next_available_lo_oid(&registry)?;
    advance_next_lo_oid(&mut registry, candidate);
    registry.objects.insert(candidate, Vec::new());
    Ok(Value::Int(candidate))
}

/// `lo_get(oid)` returns the entire LO contents as bytea.
/// `lo_get(oid, offset, length)` returns a slice.
pub(super) fn eval_lo_get(args: &[Value]) -> DbResult<Value> {
    if !(1..=3).contains(&args.len()) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_get() expects 1 or 3 arguments",
        ));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_get", "oid")?;
    let registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let data = registry
        .objects
        .get(&oid)
        .ok_or_else(|| lo_object_not_found(oid))?;
    if args.len() == 1 {
        return Ok(Value::Blob(data.clone()));
    }
    let offset = lo_arg_i64(&args[1], "lo_get", "offset")?;
    let length = lo_arg_i64(&args[2], "lo_get", "length")?;
    if offset < 0 || length < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_get() offset and length must be non-negative",
        ));
    }
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(data.len());
    let want = usize::try_from(length).unwrap_or(usize::MAX);
    let end = start.saturating_add(want).min(data.len());
    Ok(Value::Blob(data[start..end].to_vec()))
}

/// `lo_put(oid, offset, bytea) → void`. Writes `bytea` into the LO at
/// `offset`, growing the LO if necessary. Returns NULL (mirroring PG's void
/// signature in our Value model).
pub(super) fn eval_lo_put(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "lo_put")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_put", "oid")?;
    let offset = lo_arg_i64(&args[1], "lo_put", "offset")?;
    if offset < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_put() offset must be non-negative",
        ));
    }
    let bytes = match &args[2] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let data = registry
        .objects
        .get_mut(&oid)
        .ok_or_else(|| lo_object_not_found(oid))?;
    let (start, end) = checked_lo_write_range("lo_put", offset, bytes.len())?;
    if data.len() < end {
        data.resize(end, 0);
    }
    data[start..end].copy_from_slice(&bytes);
    Ok(Value::Null)
}

/// `lo_from_bytea(oid, bytea) → oid`. Creates a new LO with the given content.
/// Passing oid=0 lets the server pick a free oid.
pub(super) fn eval_lo_from_bytea(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_from_bytea")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let requested = lo_arg_i32(&args[0], "lo_from_bytea", "oid")?;
    let bytes = match &args[1] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let oid = if requested > 0 {
        requested
    } else {
        let candidate = next_available_lo_oid(&registry)?;
        advance_next_lo_oid(&mut registry, candidate);
        candidate
    };
    checked_lo_size("lo_from_bytea", bytes.len())?;
    registry.objects.insert(oid, bytes);
    advance_next_lo_oid(&mut registry, oid);
    Ok(Value::Int(oid))
}

/// `lo_tell(fd) → integer`. Returns the current cursor position. `bigint=true`
/// is the `lo_tell64` variant returning int8.
pub(super) fn eval_lo_tell(args: &[Value], bigint: bool) -> DbResult<Value> {
    expect_args(args, 1, "lo_tell")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_tell", "fd")?;
    let registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let Some(session) = registry.sessions.get(&lo_session_key()) else {
        return Err(lo_bad_fd(fd));
    };
    let Some(state) = session.fds.get(&fd) else {
        return Err(lo_bad_fd(fd));
    };
    let position = i64::try_from(state.position).unwrap_or(i64::MAX);
    Ok(if bigint {
        Value::BigInt(position)
    } else {
        Value::Int(i32::try_from(position).unwrap_or(i32::MAX))
    })
}

pub(super) fn eval_lo_truncate(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_truncate")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_truncate", "fd")?;
    let len = lo_arg_usize(&args[1], "lo_truncate", "len")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let oid = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        state.oid
    };
    let Some(data) = registry.objects.get_mut(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    checked_lo_size("lo_truncate", len)?;
    data.resize(len, 0);
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = state.position.min(len);
        }
    }
    Ok(Value::Int(0))
}
