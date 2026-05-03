//! Tiny end-to-end driver: source `.fv` file → component `.wasm` bytes.
//!
//! Compiles a single formalang source file (resolving `use` statements
//! through the filesystem rooted at the input file's parent directory),
//! runs the standard codegen pipeline (`Pipeline::for_codegen` =
//! monomorphise + resolve-references + closure-conversion + DCE), and
//! emits a Component-Model artifact.
//!
//! Intended for end-to-end smoke tests and quick experimentation
//! without writing a Rust harness. Usage:
//!
//! ```text
//! formawasm <input.fv> [-o <output.wasm>]
//! ```
//!
//! If `-o` is omitted the output filename is the input with `.fv`
//! replaced by `.wasm`. Diagnostics from the upstream frontend
//! flow through `formalang::report_errors`; backend errors print
//! their typed `Display` form. Non-zero exit on any failure.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "binary entry point — print macros are how we report status to the user"
)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use formalang::{
    FileSystemResolver, Pipeline, compile_to_ir_with_resolver, ir::ClosureConversionPass,
    ir::DeadCodeEliminationPass, ir::MonomorphisePass, ir::ResolveReferencesPass, report_errors,
};
use formawasm::WasmBackend;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let parsed = match parse_args(&argv) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            print_usage(
                &argv
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "formawasm".to_owned()),
            );
            return ExitCode::from(2);
        }
    };
    match run(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
struct Args {
    input: PathBuf,
    output: PathBuf,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err("(usage)".to_owned()),
            "-o" | "--output" => {
                let next = iter
                    .next()
                    .ok_or_else(|| "-o requires a value".to_owned())?;
                output = Some(PathBuf::from(next));
            }
            v if v.starts_with('-') => return Err(format!("unknown flag: {v}")),
            v => {
                if input.is_some() {
                    return Err(format!("unexpected positional argument: {v}"));
                }
                input = Some(PathBuf::from(v));
            }
        }
    }
    let input = input.ok_or_else(|| "missing input file".to_owned())?;
    let output = output.unwrap_or_else(|| derive_output_path(&input));
    Ok(Args { input, output })
}

/// Replace the input's extension with `.wasm`. If the input has no
/// extension, append `.wasm` instead.
fn derive_output_path(input: &Path) -> PathBuf {
    let mut out = input.to_path_buf();
    if !out.set_extension("wasm") {
        let mut s = out.into_os_string();
        s.push(".wasm");
        out = s.into();
    }
    out
}

fn print_usage(prog: &str) {
    eprintln!("usage: {prog} <input.fv> [-o <output.wasm>]");
}

fn run(args: &Args) -> Result<(), String> {
    let source = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("could not read {}: {e}", args.input.display()))?;
    let resolver = FileSystemResolver::new(
        args.input
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    );
    let module = compile_to_ir_with_resolver(&source, resolver).map_err(|errors| {
        let label = args.input.display().to_string();
        let report = report_errors(&errors, &source, &label);
        eprintln!("{report}");
        format!("{} compile errors", errors.len())
    })?;

    let mut pipeline = Pipeline::new()
        .pass(MonomorphisePass::default())
        .pass(ResolveReferencesPass::new())
        .pass(ClosureConversionPass::new())
        .pass(DeadCodeEliminationPass::new());
    let bytes = pipeline
        .emit(module, &WasmBackend::new())
        .map_err(|e| format!("{e}"))?;

    std::fs::write(&args.output, &bytes)
        .map_err(|e| format!("could not write {}: {e}", args.output.display()))?;

    println!(
        "wrote {} ({} bytes) from {}",
        args.output.display(),
        bytes.len(),
        args.input.display()
    );
    Ok(())
}
