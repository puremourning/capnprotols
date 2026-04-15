//! capnpfmt — Cap'n Proto schema formatter.
//!
//! Usage:
//!   capnpfmt [options] [file ...]
//!
//! With no files, reads stdin and writes formatted output to stdout.
//! With files, formats each file in place (unless --check is given).

use std::io::{self, Read, Write};
use std::process::ExitCode;

use capnprotols::config::FormatOptions;
use capnprotols::format::format_document;

fn main() -> ExitCode {
  let args: Vec<String> = std::env::args().skip(1).collect();

  let mut check = false;
  let mut width: u32 = 100;
  let mut files: Vec<String> = Vec::new();

  let mut i = 0;
  while i < args.len() {
    match args[i].as_str() {
      "--check" | "-c" => check = true,
      "--width" | "-w" => {
        i += 1;
        width = args.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
          eprintln!("--width requires a number");
          std::process::exit(2);
        });
      }
      "--help" | "-h" => {
        eprintln!("Usage: capnpfmt [--check] [--width N] [file ...]");
        eprintln!();
        eprintln!("With no files, reads stdin and writes to stdout.");
        eprintln!("With files, formats each file in place.");
        eprintln!();
        eprintln!("Options:");
        eprintln!(
          "  -c, --check   Check if files are formatted; exit 1 if not"
        );
        eprintln!("  -w, --width   Max line width (default: 100)");
        return ExitCode::SUCCESS;
      }
      s if s.starts_with('-') => {
        eprintln!("unknown option: {s}");
        return ExitCode::from(2);
      }
      _ => files.push(args[i].clone()),
    }
    i += 1;
  }

  let opts = FormatOptions {
    enabled: true,
    max_width: width,
    warn_long_lines: false,
  };

  if files.is_empty() {
    // stdin → stdout
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap_or_else(|e| {
      eprintln!("error reading stdin: {e}");
      std::process::exit(1);
    });
    match format_document(&input, &opts) {
      Some(out) => {
        if check {
          if out.text != input {
            return ExitCode::FAILURE;
          }
        } else {
          io::stdout().write_all(out.text.as_bytes()).unwrap();
        }
      }
      None => {
        eprintln!("<stdin>: parse error, skipping");
        io::stdout().write_all(input.as_bytes()).unwrap();
        return ExitCode::FAILURE;
      }
    }
  } else {
    let mut any_failed = false;
    for path in &files {
      let input = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
          eprintln!("{path}: {e}");
          any_failed = true;
          continue;
        }
      };
      match format_document(&input, &opts) {
        Some(out) => {
          if check {
            if out.text != input {
              eprintln!("{path}: not formatted");
              any_failed = true;
            }
          } else if out.text != input {
            std::fs::write(path, &out.text).unwrap_or_else(|e| {
              eprintln!("{path}: {e}");
            });
          }
        }
        None => {
          eprintln!("{path}: parse error, skipping");
          any_failed = true;
        }
      }
    }
    if any_failed {
      return ExitCode::FAILURE;
    }
  }

  ExitCode::SUCCESS
}
