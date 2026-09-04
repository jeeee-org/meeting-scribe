@echo off
rem dual-capture launcher. Japanese docs live in README.md next to this file.
rem
rem ASCII ONLY in this file. cmd.exe reads .cmd as CP932, so UTF-8 Japanese
rem bytes get mangled; some decode to & or |, which cmd treats as command
rem separators even on rem lines. Keep comments in English here.
rem
rem   run.cmd --list                       list audio endpoints
rem   run.cmd --mic=Logi --loopback=Logi   record until Ctrl+C or Enter
rem   run.cmd 30 --mic=Logi                record 30 seconds (measurement only)
rem   run.cmd --repair <file.wav>          rebuild the header of a truncated WAV
rem
rem --manifest-path must stay BEFORE the "--": appending it would place it
rem after "--", where cargo passes it to the program instead of reading it.
rem Build output goes to C: because building over the 9p bridge is very slow.
setlocal
if not defined CARGO_TARGET_DIR set CARGO_TARGET_DIR=C:\ms-build\target
cargo run --release --manifest-path "%~dp0Cargo.toml" -- %*
