//! Integration test for the `formawasm` CLI binary.
//!
//! Writes a minimal `.fv` source file to a unique temp dir, spawns
//! the CLI binary at the path Cargo records via
//! `CARGO_BIN_EXE_formawasm`, and confirms it produces valid
//! component-model bytes. Locks in that the binary actually runs
//! against a real source file (rather than just compiling).

use std::path::PathBuf;
use std::process::Command;

use wasmparser::{Validator, WasmFeatures};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const FV_SOURCE: &str = "pub fn id(x: I32) -> I32 { x }\n";

/// Build a unique scratch directory under the system temp root and
/// return its path. The directory name embeds the test's PID so
/// concurrent test runs (`cargo test --jobs N`) don't collide.
fn scratch_dir(label: &str) -> Result<PathBuf, TestError> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "formawasm-cli-test-{}-{}-{}",
        std::process::id(),
        label,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

#[test]
fn cli_compiles_a_minimal_source_file_to_a_valid_component() -> TestResult {
    let dir = scratch_dir("happy-path")?;
    let input = dir.join("id.fv");
    let output = dir.join("id.wasm");
    std::fs::write(&input, FV_SOURCE)?;

    let bin = env!("CARGO_BIN_EXE_formawasm");
    let status = Command::new(bin)
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .status()?;
    if !status.success() {
        return Err(format!("cli exited non-zero: {status:?}").into());
    }

    let bytes = std::fs::read(&output)?;
    validate_component(&bytes)?;

    // Cleanup is best-effort — leaving artifacts in temp_dir is
    // benign and keeps the test from masking real failures with
    // tear-down errors.
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn cli_derives_output_path_from_input_when_no_o_flag_passes() -> TestResult {
    let dir = scratch_dir("derived-output")?;
    let input = dir.join("derived.fv");
    let derived_output = dir.join("derived.wasm");
    std::fs::write(&input, FV_SOURCE)?;

    let bin = env!("CARGO_BIN_EXE_formawasm");
    let status = Command::new(bin).arg(&input).status()?;
    if !status.success() {
        return Err(format!("cli exited non-zero: {status:?}").into());
    }

    if !derived_output.exists() {
        return Err(format!(
            "expected derived output at {}, no file present",
            derived_output.display()
        )
        .into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn cli_exits_non_zero_on_missing_input() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_formawasm");
    let status = Command::new(bin).status()?;
    if status.success() {
        return Err("expected non-zero exit when no input is supplied".into());
    }
    Ok(())
}
