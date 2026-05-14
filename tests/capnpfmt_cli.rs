//! Integration tests for the capnpfmt binary's .capnpfmtignore support.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Schema with trailing whitespace that capnpfmt will strip on format.
const UGLY: &str =
  "@0xeaf06436acd04fcf;   \nstruct A {\n  foo @0 :Text;   \n}\n";

fn tempdir(label: &str) -> PathBuf {
  static COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
  let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!(
    "capnpfmt-cli-{}-{}-{}-{}",
    label,
    std::process::id(),
    nanos,
    n
  ));
  fs::create_dir_all(&dir).unwrap();
  // Treat as a repo root so .capnpfmtignore discovery stops here and doesn't
  // pick up anything from outside the tempdir.
  fs::create_dir_all(dir.join(".git")).unwrap();
  dir
}

fn capnpfmt() -> Command {
  Command::new(env!("CARGO_BIN_EXE_capnpfmt"))
}

#[test]
fn ignored_files_are_not_reformatted() {
  let dir = tempdir("skip");
  fs::write(dir.join(".capnpfmtignore"), "vendor/\n").unwrap();
  fs::create_dir_all(dir.join("vendor")).unwrap();
  let vendored = dir.join("vendor/foo.capnp");
  let normal = dir.join("normal.capnp");
  fs::write(&vendored, UGLY).unwrap();
  fs::write(&normal, UGLY).unwrap();

  let status = capnpfmt()
    .arg(&vendored)
    .arg(&normal)
    .status()
    .expect("spawn capnpfmt");
  assert!(status.success(), "capnpfmt exited non-zero: {status:?}");

  assert_eq!(
    fs::read_to_string(&vendored).unwrap(),
    UGLY,
    "ignored file should not have been rewritten"
  );
  assert_ne!(
    fs::read_to_string(&normal).unwrap(),
    UGLY,
    "non-ignored file should have been reformatted"
  );

  let _ = fs::remove_dir_all(&dir);
}

#[test]
fn no_ignore_flag_bypasses_capnpfmtignore() {
  let dir = tempdir("bypass");
  fs::write(dir.join(".capnpfmtignore"), "vendor/\n").unwrap();
  fs::create_dir_all(dir.join("vendor")).unwrap();
  let vendored = dir.join("vendor/foo.capnp");
  fs::write(&vendored, UGLY).unwrap();

  let status = capnpfmt()
    .arg("--no-ignore")
    .arg(&vendored)
    .status()
    .expect("spawn capnpfmt");
  assert!(status.success(), "capnpfmt exited non-zero: {status:?}");

  assert_ne!(
    fs::read_to_string(&vendored).unwrap(),
    UGLY,
    "--no-ignore should bypass .capnpfmtignore"
  );

  let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_mode_passes_when_only_ignored_files_are_unformatted() {
  let dir = tempdir("check");
  fs::write(dir.join(".capnpfmtignore"), "vendor/\n").unwrap();
  fs::create_dir_all(dir.join("vendor")).unwrap();
  let vendored = dir.join("vendor/foo.capnp");
  fs::write(&vendored, UGLY).unwrap();

  let status = capnpfmt()
    .arg("--check")
    .arg(&vendored)
    .status()
    .expect("spawn capnpfmt");
  assert!(
    status.success(),
    "--check should ignore the vendored file and exit 0"
  );

  let _ = fs::remove_dir_all(&dir);
}
