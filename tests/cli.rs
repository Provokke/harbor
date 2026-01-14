//! CLI-level tests: `eval`, the REPL (driven over stdin), flags and usage errors.

use std::io::Write;
use std::process::{Command, Stdio};

fn harbor() -> Command {
    Command::new(env!("CARGO_BIN_EXE_harbor"))
}

#[test]
fn eval_prints_and_runs_event_loop() {
    let out = harbor()
        .args([
            "eval",
            "setTimeout(() => console.log('later'), 1); console.log(6 * 7)",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\nlater\n");
}

#[test]
fn eval_uncaught_error_exits_1_with_stack() {
    let out = harbor()
        .args(["eval", "function f(){ throw new TypeError('bad') } f()"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error: Uncaught TypeError: bad"), "{err}");
    assert!(err.contains("at f (eval:1:"), "{err}");
}

#[test]
fn eval_respects_permissions() {
    let out = harbor()
        .args(["eval", "harbor.env.get('PATH')"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PermissionDenied"));
    let out = harbor()
        .args([
            "eval",
            "--allow-env=PATH",
            "console.log(typeof harbor.env.get('PATH'))",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "string\n");
}

#[test]
fn repl_keeps_state_and_settles_promises() {
    let mut child = harbor()
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"var x = 20\nfunction add(a,\n b) { return a + b }\nadd(x, 22)\nPromise.resolve(1).then(v => console.log('resolved', v))\nnull.boom\n'still alive'\n.exit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success());
    assert_eq!(
        stdout, "undefined\nundefined\n42\nresolved 1\nPromise {}\n\"still alive\"\n",
        "{stdout}"
    );
    assert!(stderr.contains("TypeError"), "{stderr}");
}

#[test]
fn usage_errors() {
    let out = harbor()
        .args(["run", "--allow-bogus", "x.js"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = harbor().args(["frobnicate"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = harbor()
        .args(["run", "/definitely/missing.js"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Module not found"));
    let out = harbor().arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("harbor 0.1.0 (v8 "));
}
