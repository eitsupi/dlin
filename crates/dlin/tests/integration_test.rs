use filetime::{FileTime, set_file_mtime};
use std::path::{Path, PathBuf};

// We need to reference the library modules — use the binary crate via process for CLI tests,
// but for unit-level integration tests, we'll test the core logic inline.
// For artifact tests, we test the JSON parsing directly.

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture_dir() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("simple_project")
}

fn binary_path() -> PathBuf {
    let mut path = workspace_root();
    path.push("target");
    path.push("debug");
    path.push("dlin");
    path
}

/// Copy the fixture project into a temp directory and return the temp dir.
fn copy_fixture_to_temp() -> tempfile::TempDir {
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let fixture = fixture_dir();

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                fs::copy(&src_path, &dst_path).unwrap();
            }
        }
    }

    copy_dir_recursive(&fixture, tmp.path());
    tmp
}

const MTIME_OFFSET_SECS: i64 = 10;

fn set_mtime_newer_than(newer: &Path, older: &Path) {
    let older_mtime = FileTime::from_last_modification_time(&std::fs::metadata(older).unwrap());
    let newer_mtime = FileTime::from_unix_time(
        older_mtime.unix_seconds() + MTIME_OFFSET_SECS,
        older_mtime.nanoseconds(),
    );
    set_file_mtime(newer, newer_mtime).unwrap();
}

#[path = "integration/artifacts.rs"]
mod artifacts;
#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/error_format.rs"]
mod error_format;
#[path = "integration/freshness.rs"]
mod freshness;
#[path = "integration/generic_test_metadata.rs"]
mod generic_test_metadata;
#[path = "integration/manifest_only.rs"]
mod manifest_only;
#[path = "integration/parsing.rs"]
mod parsing;
#[path = "integration/sql_mode_test_warning.rs"]
mod sql_mode_test_warning;
