use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use aiondb_core::{DbError, DbResult, Value};

use super::expect_args;
use super::value_convert::i64_to_f64;

const SYNTHETIC_POSTMASTER_PID: &[u8] =
    b"12345\n/tmp/aiondb\n1710172800\n5432\n/tmp/aiondb.sock\nlocalhost\n";
const MAX_SERVER_FILE_READ_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SERVER_DIR_ENTRIES: usize = 10_000;

pub(super) fn eval_pg_aggregate_helper(name: &str, args: &[Value]) -> Option<DbResult<Value>> {
    match name {
        "float8_accum" => Some(eval_float8_accum(args)),
        "float8_combine" => Some(eval_float8_combine(args)),
        "float8_regr_accum" => Some(eval_float8_regr_accum(args)),
        "float8_regr_combine" => Some(eval_float8_regr_combine(args)),
        "booland_statefunc" => Some(eval_bool_statefunc(args, true)),
        "boolor_statefunc" => Some(eval_bool_statefunc(args, false)),
        _ => None,
    }
}

pub(super) fn eval_pg_read_file(args: &[Value]) -> DbResult<Value> {
    eval_pg_read_file_with_base_dir(args, None)
}

pub fn eval_pg_read_file_with_base_dir(args: &[Value], base_dir: Option<&Path>) -> DbResult<Value> {
    let request = parse_read_file_request(args, "pg_read_file")?;
    let bytes = read_server_file(&request, base_dir)?;
    let Some(bytes) = bytes else {
        return Ok(Value::Null);
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Value::Text(text))
}

pub(super) fn eval_pg_read_binary_file(args: &[Value]) -> DbResult<Value> {
    eval_pg_read_binary_file_with_base_dir(args, None)
}

pub fn eval_pg_read_binary_file_with_base_dir(
    args: &[Value],
    base_dir: Option<&Path>,
) -> DbResult<Value> {
    let request = parse_read_file_request(args, "pg_read_binary_file")?;
    let bytes = read_server_file(&request, base_dir)?;
    let Some(bytes) = bytes else {
        return Ok(Value::Null);
    };
    Ok(Value::Blob(bytes))
}

pub(super) fn eval_pg_ls_dir(name: &str, args: &[Value]) -> DbResult<Value> {
    eval_pg_ls_dir_with_base_dir(name, args, None)
}

pub fn eval_pg_ls_dir_with_base_dir(
    name: &str,
    args: &[Value],
    base_dir: Option<&Path>,
) -> DbResult<Value> {
    let (path, missing_ok, include_dot_dirs) = if matches!(name, "pg_ls_dir") {
        let (path, missing_ok, include_dot_dirs) = parse_generic_dir_request(name, args)?;
        (
            resolve_user_supplied_path(path.as_path(), base_dir)?,
            missing_ok,
            include_dot_dirs,
        )
    } else {
        parse_special_dir_request(name, args, base_dir)?
    };

    let entries = list_directory_entries(&path, missing_ok, include_dot_dirs)?;
    Ok(Value::Array(
        entries.into_iter().map(Value::Text).collect::<Vec<_>>(),
    ))
}

fn parse_generic_dir_request(name: &str, args: &[Value]) -> DbResult<(PathBuf, bool, bool)> {
    if name != "pg_ls_dir" {
        return Err(DbError::internal(format!(
            "{name} is not a directory-listing helper"
        )));
    }
    if args.is_empty() || args.len() > 3 {
        return Err(DbError::internal(format!(
            "{name} requires 1 to 3 argument(s), got {}",
            args.len()
        )));
    }

    let path = match &args[0] {
        Value::Text(path) => PathBuf::from(path),
        Value::Null => return Ok((PathBuf::new(), false, false)),
        _ => return Err(DbError::internal(format!("{name} path must be text"))),
    };
    let missing_ok = match args.get(1) {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Null) | None => false,
        _ => {
            return Err(DbError::internal(format!(
                "{name} missing_ok must be boolean"
            )));
        }
    };
    let include_dot_dirs = match args.get(2) {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Null) | None => false,
        _ => {
            return Err(DbError::internal(format!(
                "{name} include_dot_dirs must be boolean"
            )));
        }
    };

    Ok((path, missing_ok, include_dot_dirs))
}

fn parse_special_dir_request(
    name: &str,
    args: &[Value],
    base_dir: Option<&Path>,
) -> DbResult<(PathBuf, bool, bool)> {
    if args.len() > 2 {
        return Err(DbError::internal(format!(
            "{name} requires 0 to 2 argument(s), got {}",
            args.len()
        )));
    }

    let missing_ok = match args.first() {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Null) | None => false,
        _ => {
            return Err(DbError::internal(format!(
                "{name} missing_ok must be boolean"
            )));
        }
    };
    let include_dot_dirs = match args.get(1) {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Null) | None => false,
        _ => {
            return Err(DbError::internal(format!(
                "{name} include_dot_dirs must be boolean"
            )));
        }
    };

    Ok((
        resolve_special_dir_path(name, base_dir)?,
        missing_ok,
        include_dot_dirs,
    ))
}

fn resolve_special_dir_path(name: &str, base_dir: Option<&Path>) -> DbResult<PathBuf> {
    let base_dir = approved_base_dir(base_dir)?;

    let path = match name {
        "pg_ls_archive_statusdir" => base_dir.join("pg_wal").join("archive_status"),
        "pg_ls_logdir" => base_dir.join("log"),
        "pg_ls_tmpdir" => base_dir.join("base").join("pgsql_tmp"),
        _ => {
            return Err(DbError::internal(format!(
                "{name} is not a directory-listing helper"
            )));
        }
    };

    Ok(path)
}

fn eval_float8_accum(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "float8_accum")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let state = parse_numeric_state(&args[0], 3)?;
    let x = value_to_f64(&args[1])?;
    let updated = accumulate_moments3(&state, x);
    Ok(render_numeric_state(&updated))
}

fn eval_float8_combine(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "float8_combine")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let left = parse_numeric_state(&args[0], 3)?;
    let right = parse_numeric_state(&args[1], 3)?;
    let combined = combine_moments3(&left, &right);
    Ok(render_numeric_state(&combined))
}

fn eval_float8_regr_accum(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "float8_regr_accum")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let state = parse_numeric_state(&args[0], 6)?;
    let y = value_to_f64(&args[1])?;
    let x = value_to_f64(&args[2])?;
    let updated = accumulate_moments6(&state, y, x);
    Ok(render_numeric_state(&updated))
}

fn eval_float8_regr_combine(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "float8_regr_combine")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let left = parse_numeric_state(&args[0], 6)?;
    let right = parse_numeric_state(&args[1], 6)?;
    let combined = combine_moments6(&left, &right);
    Ok(render_numeric_state(&combined))
}

fn eval_bool_statefunc(args: &[Value], and_mode: bool) -> DbResult<Value> {
    expect_args(
        args,
        2,
        if and_mode {
            "booland_statefunc"
        } else {
            "boolor_statefunc"
        },
    )?;
    let Value::Boolean(left) = &args[0] else {
        return Ok(Value::Null);
    };
    let Value::Boolean(right) = &args[1] else {
        return Ok(Value::Null);
    };
    Ok(Value::Boolean(if and_mode {
        *left && *right
    } else {
        *left || *right
    }))
}

struct ReadFileRequest {
    path: String,
    offset: i64,
    length: Option<i64>,
    missing_ok: bool,
}

fn parse_read_file_request(args: &[Value], function_name: &str) -> DbResult<ReadFileRequest> {
    if args.is_empty() || args.len() > 4 {
        return Err(DbError::internal(format!(
            "{function_name} requires 1 to 4 argument(s), got {}",
            args.len()
        )));
    }

    let path = match &args[0] {
        Value::Text(path) => path.clone(),
        Value::Null => {
            return Ok(ReadFileRequest {
                path: String::new(),
                offset: 0,
                length: None,
                missing_ok: true,
            });
        }
        _ => {
            return Err(DbError::internal(format!(
                "{function_name} path must be text"
            )));
        }
    };

    let (offset, length, missing_ok) = match args.len() {
        1 => (0, None, false),
        2 => match &args[1] {
            Value::Boolean(missing_ok) => (0, None, *missing_ok),
            other => (value_to_i64(other, "offset")?, None, false),
        },
        3 => (
            value_to_i64(&args[1], "offset")?,
            Some(value_to_i64(&args[2], "length")?),
            false,
        ),
        4 => (
            value_to_i64(&args[1], "offset")?,
            Some(value_to_i64(&args[2], "length")?),
            value_to_bool(&args[3], "missing_ok")?,
        ),
        _ => unreachable!(),
    };

    if length.is_some_and(|length| length < 0) {
        return Err(DbError::internal("requested length cannot be negative"));
    }

    Ok(ReadFileRequest {
        path,
        offset,
        length,
        missing_ok,
    })
}

fn read_server_file(
    request: &ReadFileRequest,
    base_dir: Option<&Path>,
) -> DbResult<Option<Vec<u8>>> {
    if request.path.is_empty() {
        return Ok(None);
    }
    if request.path == "postmaster.pid" {
        return Ok(Some(apply_file_slice(
            SYNTHETIC_POSTMASTER_PID,
            request.offset,
            request.length,
        )));
    }

    let resolved = resolve_user_supplied_path(Path::new(&request.path), base_dir)?;
    let metadata = match fs::symlink_metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) if request.missing_ok && error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(DbError::internal(format!(
                "could not open file \"{}\" for reading: {}",
                request.path,
                display_io_error(&error)
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(DbError::internal(format!(
            "could not open file \"{}\" for reading: symbolic links are not allowed",
            request.path
        )));
    }
    if !metadata.file_type().is_file() {
        return Err(DbError::internal(format!(
            "could not open file \"{}\" for reading: not a regular file",
            request.path
        )));
    }

    let mut file = File::open(&resolved).map_err(|error| {
        DbError::internal(format!(
            "could not open file \"{}\" for reading: {}",
            request.path,
            display_io_error(&error)
        ))
    })?;
    let len = file.metadata().map(|meta| meta.len()).map_err(|error| {
        DbError::internal(format!(
            "could not open file \"{}\" for reading: {}",
            request.path,
            display_io_error(&error)
        ))
    })?;
    read_file_slice_from_disk(
        &mut file,
        len,
        request.offset,
        request.length,
        &request.path,
    )
    .map(Some)
}

fn resolve_user_supplied_path(path: &Path, base_dir: Option<&Path>) -> DbResult<PathBuf> {
    validate_relative_server_path(path)?;
    let base_dir = approved_base_dir(base_dir)?;
    let joined = join_relative_path(&base_dir, path)?;
    if let Ok(canonical) = fs::canonicalize(&joined) {
        if !canonical.starts_with(&base_dir) {
            return Err(DbError::internal(format!(
                "path \"{}\" resolves outside the approved server directory",
                path.display()
            )));
        }
        return Ok(canonical);
    }
    Ok(joined)
}

fn approved_base_dir(base_dir: Option<&Path>) -> DbResult<PathBuf> {
    let base_dir = match base_dir {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| {
            DbError::internal(format!("could not determine current directory: {error}"))
        })?,
    };
    let canonical = fs::canonicalize(&base_dir).map_err(|error| {
        DbError::internal(format!(
            "could not resolve approved server directory \"{}\": {}",
            base_dir.display(),
            display_io_error(&error)
        ))
    })?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|error| {
        DbError::internal(format!(
            "could not inspect approved server directory \"{}\": {}",
            canonical.display(),
            display_io_error(&error)
        ))
    })?;
    if !metadata.file_type().is_dir() {
        return Err(DbError::internal(format!(
            "approved server directory \"{}\" is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn validate_relative_server_path(path: &Path) -> DbResult<()> {
    if path.is_absolute() {
        return Err(DbError::internal(format!(
            "absolute path \"{}\" is not allowed",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir => {
                return Err(DbError::internal(format!(
                    "path traversal is not allowed in \"{}\"",
                    path.display()
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(DbError::internal(format!(
                    "absolute path \"{}\" is not allowed",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn join_relative_path(base_dir: &Path, path: &Path) -> DbResult<PathBuf> {
    let mut joined = base_dir.to_path_buf();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                joined.push(part);
                if let Ok(metadata) = fs::symlink_metadata(&joined) {
                    if metadata.file_type().is_symlink() {
                        return Err(DbError::internal(format!(
                            "path \"{}\" must not contain symbolic links",
                            path.display()
                        )));
                    }
                }
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => unreachable!(),
        }
    }
    Ok(joined)
}

fn list_directory_entries(
    path: &Path,
    missing_ok: bool,
    include_dot_dirs: bool,
) -> DbResult<Vec<String>> {
    let display_path = path.display().to_string();
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if missing_ok && error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(DbError::internal(format!(
                "could not open directory \"{display_path}\": {}",
                display_io_error(&error)
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(DbError::internal(format!(
            "could not open directory \"{display_path}\": symbolic links are not allowed"
        )));
    }
    if !metadata.file_type().is_dir() {
        return Err(DbError::internal(format!(
            "could not open directory \"{display_path}\": not a directory"
        )));
    }

    let mut entries = match fs::read_dir(path) {
        Ok(read_dir) => {
            let mut entries = Vec::new();
            for entry in read_dir {
                let entry = entry.map_err(|error| {
                    DbError::internal(format!(
                        "could not open directory \"{display_path}\": {}",
                        display_io_error(&error)
                    ))
                })?;
                push_limited_directory_entry(
                    &mut entries,
                    entry.file_name().to_string_lossy().into_owned(),
                    &display_path,
                )?;
            }
            entries.sort_unstable();
            entries
        }
        Err(error) if missing_ok && error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(DbError::internal(format!(
                "could not open directory \"{display_path}\": {}",
                display_io_error(&error)
            )));
        }
    };

    if include_dot_dirs {
        entries.insert(0, ".".to_string());
    }
    Ok(entries)
}

fn push_limited_directory_entry(
    entries: &mut Vec<String>,
    entry: String,
    display_path: &str,
) -> DbResult<()> {
    if entries.len() >= MAX_SERVER_DIR_ENTRIES {
        return Err(DbError::program_limit(format!(
            "directory \"{display_path}\" has too many entries (maximum {MAX_SERVER_DIR_ENTRIES})"
        )));
    }
    entries.push(entry);
    Ok(())
}

fn apply_file_slice(bytes: &[u8], offset: i64, length: Option<i64>) -> Vec<u8> {
    let start = if offset >= 0 {
        usize::try_from(offset).unwrap_or(usize::MAX)
    } else {
        bytes
            .len()
            .saturating_sub(usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX))
    };
    let start = start.min(bytes.len());
    let end = match length {
        Some(length) => start
            .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
            .min(bytes.len()),
        None => bytes.len(),
    };
    bytes[start..end].to_vec()
}

fn read_file_slice_from_disk(
    file: &mut File,
    file_len: u64,
    offset: i64,
    length: Option<i64>,
    display_path: &str,
) -> DbResult<Vec<u8>> {
    let start = if offset >= 0 {
        u64::try_from(offset).unwrap_or(u64::MAX)
    } else {
        file_len.saturating_sub(offset.unsigned_abs())
    }
    .min(file_len);
    let end = match length {
        Some(length) => start
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX))
            .min(file_len),
        None => file_len,
    };
    let bytes_to_read = end.saturating_sub(start);
    if bytes_to_read > MAX_SERVER_FILE_READ_BYTES {
        return Err(DbError::internal(format!(
            "could not open file \"{display_path}\" for reading: requested slice exceeds {MAX_SERVER_FILE_READ_BYTES} bytes"
        )));
    }

    if bytes_to_read == 0 {
        return Ok(Vec::new());
    }

    file.seek(SeekFrom::Start(start)).map_err(|error| {
        DbError::internal(format!(
            "could not open file \"{display_path}\" for reading: {}",
            display_io_error(&error)
        ))
    })?;
    let bytes_to_read_usize = usize::try_from(bytes_to_read).map_err(|_| {
        DbError::internal(format!(
            "could not open file \"{display_path}\" for reading: requested slice exceeds platform limits"
        ))
    })?;
    let mut bytes = vec![0u8; bytes_to_read_usize];
    file.read_exact(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "could not open file \"{display_path}\" for reading: {}",
            display_io_error(&error)
        ))
    })?;
    Ok(bytes)
}

fn value_to_i64(value: &Value, field_name: &str) -> DbResult<i64> {
    match value {
        Value::Int(value) => Ok(i64::from(*value)),
        Value::BigInt(value) => Ok(*value),
        _ => Err(DbError::internal(format!(
            "{field_name} must be an integer value"
        ))),
    }
}

fn value_to_bool(value: &Value, field_name: &str) -> DbResult<bool> {
    match value {
        Value::Boolean(value) => Ok(*value),
        _ => Err(DbError::internal(format!(
            "{field_name} must be a boolean value"
        ))),
    }
}

fn display_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::NotFound => "No such file or directory".to_string(),
        _ => error.to_string(),
    }
}

fn accumulate_moments3(state: &[f64], x: f64) -> [f64; 3] {
    let n = state[0];
    let sx = state[1];
    let sxx = state[2];
    if n == 0.0 {
        return [1.0, x, 0.0];
    }

    let n1 = n + 1.0;
    let delta = x - sx / n;
    let sxx1 = sxx + delta * delta * n / n1;
    [n1, sx + x, sxx1]
}

fn combine_moments3(left: &[f64], right: &[f64]) -> [f64; 3] {
    let n1 = left[0];
    let n2 = right[0];
    if n1 == 0.0 {
        return [right[0], right[1], right[2]];
    }
    if n2 == 0.0 {
        return [left[0], left[1], left[2]];
    }

    let n = n1 + n2;
    let sx = left[1] + right[1];
    let mean_delta = left[1] / n1 - right[1] / n2;
    let sxx = left[2] + right[2] + n1 * n2 * mean_delta * mean_delta / n;
    [n, sx, sxx]
}

fn accumulate_moments6(state: &[f64], y: f64, x: f64) -> [f64; 6] {
    let n = state[0];
    let sx = state[1];
    let sxx = state[2];
    let sy = state[3];
    let syy = state[4];
    let sxy = state[5];
    if n == 0.0 {
        return [1.0, x, 0.0, y, 0.0, 0.0];
    }

    let n1 = n + 1.0;
    let dx = x - sx / n;
    let dy = y - sy / n;
    let adjust = n / n1;
    [
        n1,
        sx + x,
        sxx + dx * dx * adjust,
        sy + y,
        syy + dy * dy * adjust,
        sxy + dx * dy * adjust,
    ]
}

fn combine_moments6(left: &[f64], right: &[f64]) -> [f64; 6] {
    let n1 = left[0];
    let n2 = right[0];
    if n1 == 0.0 {
        return [right[0], right[1], right[2], right[3], right[4], right[5]];
    }
    if n2 == 0.0 {
        return [left[0], left[1], left[2], left[3], left[4], left[5]];
    }

    let n = n1 + n2;
    let sx = left[1] + right[1];
    let sy = left[3] + right[3];
    let mean_dx = left[1] / n1 - right[1] / n2;
    let mean_dy = left[3] / n1 - right[3] / n2;
    [
        n,
        sx,
        left[2] + right[2] + n1 * n2 * mean_dx * mean_dx / n,
        sy,
        left[4] + right[4] + n1 * n2 * mean_dy * mean_dy / n,
        left[5] + right[5] + n1 * n2 * mean_dx * mean_dy / n,
    ]
}

fn parse_numeric_state(value: &Value, expected_len: usize) -> DbResult<Vec<f64>> {
    let values = match value {
        Value::Array(values) => values
            .iter()
            .map(value_to_f64)
            .collect::<DbResult<Vec<_>>>()?,
        Value::Text(text) => parse_pg_numeric_array(text)?,
        other => {
            return Err(DbError::internal(format!(
                "expected numeric state array, got {:?}",
                other.data_type()
            )));
        }
    };

    if values.len() != expected_len {
        return Err(DbError::internal(format!(
            "expected state array with {expected_len} element(s), got {}",
            values.len()
        )));
    }

    Ok(values)
}

fn parse_pg_numeric_array(text: &str) -> DbResult<Vec<f64>> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(|| DbError::internal(format!("invalid array literal: {trimmed}")))?;
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    inner
        .split(',')
        .map(|part| parse_pg_f64(part.trim()))
        .collect::<DbResult<Vec<_>>>()
}

fn parse_pg_f64(text: &str) -> DbResult<f64> {
    if text.eq_ignore_ascii_case("nan") {
        return Ok(f64::NAN);
    }
    if text.eq_ignore_ascii_case("inf") || text.eq_ignore_ascii_case("infinity") {
        return Ok(f64::INFINITY);
    }
    if text.eq_ignore_ascii_case("-inf") || text.eq_ignore_ascii_case("-infinity") {
        return Ok(f64::NEG_INFINITY);
    }
    text.parse::<f64>()
        .map_err(|_| DbError::internal(format!("invalid floating-point value: {text}")))
}

fn value_to_f64(value: &Value) -> DbResult<f64> {
    match value {
        Value::Int(v) => Ok(f64::from(*v)),
        Value::BigInt(v) => Ok(i64_to_f64(*v)),
        Value::Real(v) => Ok(f64::from(*v)),
        Value::Double(v) => Ok(*v),
        Value::Numeric(v) => v
            .to_string()
            .parse::<f64>()
            .map_err(|_| DbError::internal("cannot convert numeric to double")),
        Value::Boolean(v) => Ok(if *v { 1.0 } else { 0.0 }),
        Value::Text(text) => parse_pg_f64(text),
        Value::Null => Ok(f64::NAN),
        other => Err(DbError::internal(format!(
            "expected numeric value, got {:?}",
            other.data_type()
        ))),
    }
}

fn render_numeric_state(values: &[f64]) -> Value {
    Value::Text(
        Value::Array(
            values
                .iter()
                .copied()
                .map(Value::Double)
                .collect::<Vec<_>>(),
        )
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float8_accum_updates_centered_sum_of_squares() {
        let result = eval_float8_accum(&[
            Value::Array(vec![
                Value::Double(4.0),
                Value::Double(140.0),
                Value::Double(2900.0),
            ]),
            Value::Double(100.0),
        ])
        .expect("float8_accum");
        assert_eq!(result, Value::Text("{5,240,6280}".to_string()));
    }

    #[test]
    fn float8_regr_combine_matches_postgres_reference_case() {
        let result = eval_float8_regr_combine(&[
            Value::Array(vec![
                Value::Double(3.0),
                Value::Double(60.0),
                Value::Double(200.0),
                Value::Double(750.0),
                Value::Double(20000.0),
                Value::Double(2000.0),
            ]),
            Value::Array(vec![
                Value::Double(2.0),
                Value::Double(180.0),
                Value::Double(200.0),
                Value::Double(740.0),
                Value::Double(57800.0),
                Value::Double(-3400.0),
            ]),
        ])
        .expect("float8_regr_combine");
        assert_eq!(
            result,
            Value::Text("{5,240,6280,1490,95080,8680}".to_string())
        );
    }

    #[test]
    fn bool_statefuncs_propagate_nulls() {
        assert_eq!(
            eval_bool_statefunc(&[Value::Null, Value::Boolean(true)], true).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval_bool_statefunc(&[Value::Boolean(true), Value::Null], false).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn pg_read_file_supports_missing_ok_and_slices() {
        let base_dir = unique_test_dir("read-file");
        fs::create_dir_all(&base_dir).expect("create base dir");

        let result = eval_pg_read_file(&[
            Value::Text("postmaster.pid".into()),
            Value::Int(1),
            Value::Int(20),
        ])
        .unwrap();
        let Value::Text(slice) = result else {
            panic!("expected text slice");
        };
        assert_eq!(slice.len(), 20);
        assert_eq!(
            eval_pg_read_file_with_base_dir(
                &[Value::Text("does not exist".into()), Value::Boolean(true)],
                Some(base_dir.as_path()),
            )
            .unwrap(),
            Value::Null
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn pg_read_binary_file_rejects_negative_length() {
        let error = eval_pg_read_binary_file(&[
            Value::Text("does not exist".into()),
            Value::Int(0),
            Value::Int(-1),
        ])
        .expect_err("negative length should fail");
        assert_eq!(
            error.report().message,
            "requested length cannot be negative"
        );
    }

    #[test]
    fn pg_ls_logdir_reads_from_base_directory() {
        let base_dir = unique_test_dir("logdir");
        let log_dir = base_dir.join("log");
        fs::create_dir_all(&log_dir).expect("create log dir");
        fs::write(log_dir.join("server.log"), b"log").expect("write log file");

        let result =
            eval_pg_ls_dir_with_base_dir("pg_ls_logdir", &[], Some(base_dir.as_path())).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![Value::Text("server.log".to_owned())])
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn pg_ls_archive_statusdir_errors_when_directory_is_missing() {
        let base_dir = unique_test_dir("archive-missing");
        fs::create_dir_all(&base_dir).expect("create base dir");

        let error =
            eval_pg_ls_dir_with_base_dir("pg_ls_archive_statusdir", &[], Some(base_dir.as_path()))
                .expect_err("missing archive_status dir should fail");
        assert!(
            error.report().message.contains("could not open directory"),
            "unexpected error: {}",
            error.report().message
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn pg_ls_tmpdir_respects_missing_ok() {
        let base_dir = unique_test_dir("tmp-missing-ok");
        fs::create_dir_all(&base_dir).expect("create base dir");

        let result = eval_pg_ls_dir_with_base_dir(
            "pg_ls_tmpdir",
            &[Value::Boolean(true)],
            Some(base_dir.as_path()),
        )
        .unwrap();
        assert_eq!(result, Value::Array(Vec::new()));

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn pg_read_file_rejects_absolute_paths() {
        let error = eval_pg_read_file(&[Value::Text("/etc/passwd".into())])
            .expect_err("absolute paths should fail");
        assert!(
            error.report().message.contains("absolute path"),
            "unexpected error: {}",
            error.report().message
        );
    }

    #[test]
    fn pg_read_file_rejects_path_traversal() {
        let base_dir = unique_test_dir("read-traversal");
        fs::create_dir_all(&base_dir).expect("create base dir");

        let error = eval_pg_read_file_with_base_dir(
            &[Value::Text("../secret".into())],
            Some(base_dir.as_path()),
        )
        .expect_err("path traversal should fail");
        assert!(
            error.report().message.contains("path traversal"),
            "unexpected error: {}",
            error.report().message
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn pg_ls_dir_reads_relative_to_base_directory() {
        let base_dir = unique_test_dir("ls-dir-base");
        fs::create_dir_all(base_dir.join("nested")).expect("create nested dir");
        fs::write(base_dir.join("nested").join("entry.txt"), b"ok").expect("write nested file");

        let result = eval_pg_ls_dir_with_base_dir(
            "pg_ls_dir",
            &[Value::Text("nested".into())],
            Some(base_dir.as_path()),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Array(vec![Value::Text("entry.txt".to_owned())])
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn directory_entry_limit_rejects_unbounded_listing() {
        let mut entries = vec![String::new(); MAX_SERVER_DIR_ENTRIES];
        let error = push_limited_directory_entry(&mut entries, "overflow".to_owned(), "test-dir")
            .expect_err("entry above limit must fail");

        assert!(
            error.report().message.contains("too many entries"),
            "unexpected error: {}",
            error.report().message
        );
        assert_eq!(entries.len(), MAX_SERVER_DIR_ENTRIES);
    }

    #[cfg(unix)]
    #[test]
    fn pg_read_file_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let base_dir = unique_test_dir("read-symlink");
        fs::create_dir_all(&base_dir).expect("create base dir");
        fs::write(base_dir.join("real.txt"), b"safe").expect("write real file");
        symlink(base_dir.join("real.txt"), base_dir.join("link.txt")).expect("create symlink");

        let error = eval_pg_read_file_with_base_dir(
            &[Value::Text("link.txt".into())],
            Some(base_dir.as_path()),
        )
        .expect_err("symlink targets should fail");
        assert!(
            error.report().message.contains("symbolic links"),
            "unexpected error: {}",
            error.report().message
        );

        let _ = fs::remove_dir_all(&base_dir);
    }

    /// Audit: `pg_read_file('/etc/passwd')` must never return the host
    /// `/etc/passwd`. Path must be rejected as absolute before any open.
    #[cfg(unix)]
    #[test]
    fn security_audit_pg_read_file_blocks_etc_passwd() {
        let base_dir = unique_test_dir("audit-etc-passwd");
        fs::create_dir_all(&base_dir).expect("create base dir");
        let error = eval_pg_read_file_with_base_dir(
            &[Value::Text("/etc/passwd".into())],
            Some(base_dir.as_path()),
        )
        .expect_err("absolute host path must be rejected");
        assert!(
            error.report().message.contains("absolute path"),
            "unexpected: {}",
            error.report().message
        );
        let _ = fs::remove_dir_all(&base_dir);
    }

    /// Audit: deep relative traversal `../../../etc/shadow` must be rejected.
    #[test]
    fn security_audit_pg_read_file_blocks_deep_traversal() {
        let base_dir = unique_test_dir("audit-deep-traversal");
        fs::create_dir_all(&base_dir).expect("create base dir");
        let error = eval_pg_read_file_with_base_dir(
            &[Value::Text("../../../etc/shadow".into())],
            Some(base_dir.as_path()),
        )
        .expect_err("parent traversal must be rejected");
        assert!(
            error.report().message.contains("path traversal"),
            "unexpected: {}",
            error.report().message
        );
        let _ = fs::remove_dir_all(&base_dir);
    }

    /// Audit: a symlink nested under the approved base dir but pointing at a
    /// `join_relative_path` catches this before opening the file.
    #[cfg(unix)]
    #[test]
    fn security_audit_pg_read_file_blocks_intermediate_symlink() {
        use std::os::unix::fs::symlink;
        let base_dir = unique_test_dir("audit-symlink-escape");
        fs::create_dir_all(&base_dir).expect("create base dir");
        // Create `escape` symlink pointing at /etc inside the approved base.
        symlink("/etc", base_dir.join("escape")).expect("create symlink");
        let error = eval_pg_read_file_with_base_dir(
            &[Value::Text("escape/passwd".into())],
            Some(base_dir.as_path()),
        )
        .expect_err("symlink escape must be rejected");
        assert!(
            error.report().message.contains("symbolic links"),
            "unexpected: {}",
            error.report().message
        );
        let _ = fs::remove_dir_all(&base_dir);
    }

    /// `PathBuf::from` accepts NUL; the syscall layer must reject or the
    /// validator must catch it. Either outcome is acceptable as long as the
    /// host file is not returned.
    #[test]
    fn security_audit_pg_read_file_nul_byte_is_not_truncated() {
        let base_dir = unique_test_dir("audit-nul");
        fs::create_dir_all(&base_dir).expect("create base dir");
        fs::write(base_dir.join("visible.txt"), b"leak").expect("write visible");
        // Attempt to truncate the path at the NUL: if the OS stripped it,
        let result = eval_pg_read_file_with_base_dir(
            &[Value::Text("visible.txt\0ignored".into())],
            Some(base_dir.as_path()),
        );
        match result {
            Ok(Value::Text(text)) => assert_ne!(
                text, "leak",
                "NUL byte truncation leaked host file contents"
            ),
            Ok(_) | Err(_) => {}
        }
        let _ = fs::remove_dir_all(&base_dir);
    }

    /// Audit: Windows-style backslash path on Unix is just a filename
    #[test]
    fn security_audit_pg_read_file_windows_style_is_not_escape() {
        let base_dir = unique_test_dir("audit-unc");
        fs::create_dir_all(&base_dir).expect("create base dir");
        let result = eval_pg_read_file_with_base_dir(
            &[Value::Text("..\\..\\etc\\passwd".into())],
            Some(base_dir.as_path()),
        );
        // On Unix this is a single filename that should either be treated as
        // succeed with host /etc/passwd content.
        if let Ok(Value::Text(text)) = result {
            assert!(
                !text.contains("root:"),
                "Windows-style backslash escaped to host /etc/passwd"
            );
        }
        let _ = fs::remove_dir_all(&base_dir);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        static TEST_DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TEST_DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "aiondb-pg-internal-{name}-{}-{:?}-{seq}-{nanos}",
            std::process::id(),
            std::thread::current().id()
        ))
    }
}
