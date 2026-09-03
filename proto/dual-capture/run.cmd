@echo off
rem 2系統同時録音プロトタイプ。使い方: run.cmd --list / run.cmd 30
rem UNC パス(\\wsl.localhost\...)から起動すると cmd は cwd にできないので manifest を明示する。
rem 生成物は C: に逃がす（9p ブリッジ越しのビルドは極端に遅い）。
setlocal
if not defined CARGO_TARGET_DIR set CARGO_TARGET_DIR=C:\ms-build\target
cargo run --release --manifest-path "%~dp0Cargo.toml" -- %*
