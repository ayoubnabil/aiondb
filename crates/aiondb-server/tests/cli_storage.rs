use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_root(name: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "aiondb-cli-storage-{name}-{}-{timestamp}",
        std::process::id()
    ))
}

fn run_aiondb(work_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aiondb"))
        .current_dir(work_dir)
        .args(args)
        .output()
        .expect("run aiondb CLI")
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn dump_restore_cli_roundtrip_uses_canonical_backup_path() {
    let root = unique_test_root("roundtrip");
    let source_dir = root.join("source");
    let restore_dir = root.join("restored");
    fs::create_dir_all(&root).expect("create test root");

    let source = source_dir.to_string_lossy().to_string();
    let restore = restore_dir.to_string_lossy().to_string();

    let dump = run_aiondb(
        &root,
        &[
            "dump",
            "--data-dir",
            &source,
            "--output",
            "cli-storage-roundtrip.sql",
        ],
    );
    assert!(
        dump.status.success(),
        "dump should succeed\n{}",
        output_text(&dump)
    );
    let dump_output = output_text(&dump);
    assert!(dump_output.contains("dump=ok"), "{dump_output}");
    assert!(
        root.join("backups/cli-storage-roundtrip.sql").is_file(),
        "{dump_output}"
    );

    let restore_output = run_aiondb(
        &root,
        &[
            "restore",
            "--data-dir",
            &restore,
            "--input",
            "cli-storage-roundtrip.sql",
        ],
    );
    assert!(
        restore_output.status.success(),
        "restore should succeed\n{}",
        output_text(&restore_output)
    );
    let restore_text = output_text(&restore_output);
    assert!(restore_text.contains("restore=ok"), "{restore_text}");

    let doctor = run_aiondb(&root, &["doctor", "--data-dir", &restore]);
    assert!(
        doctor.status.success(),
        "doctor should accept restored data-dir\n{}",
        output_text(&doctor)
    );
    let doctor_text = output_text(&doctor);
    assert!(doctor_text.contains("storage_format=v1.0"), "{doctor_text}");
    assert!(doctor_text.contains("status=ok"), "{doctor_text}");

    fs::remove_dir_all(&root).expect("remove test root");
}

#[test]
fn dump_cli_refuses_path_traversal() {
    let root = unique_test_root("path-traversal");
    let source_dir = root.join("source");
    fs::create_dir_all(&root).expect("create test root");
    let source = source_dir.to_string_lossy().to_string();

    let output = run_aiondb(
        &root,
        &["dump", "--data-dir", &source, "--output", "../outside.sql"],
    );
    assert!(
        !output.status.success(),
        "dump traversal should fail\n{}",
        output_text(&output)
    );
    let text = output_text(&output);
    assert!(text.contains("dump=refused"), "{text}");
    assert!(text.contains("must not contain '..'"), "{text}");

    fs::remove_dir_all(&root).expect("remove test root");
}
