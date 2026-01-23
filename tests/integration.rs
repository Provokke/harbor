//! Runs every script in `tests/js/*.{js,ts}` through the `harbor` binary and
//! compares stdout (against `<name>.out`), the exit code and stderr.
//!
//! Header directives (leading `//` comment lines of the script):
//!   `// args: <flags>`     permission flags (`$TMP` = fresh temp dir)
//!   `// argv: a|b c|d`     script arguments (default: the temp dir)
//!   `// exit: <code>`      expected exit code (default 0)
//!   `// stderr: <text>`    stderr must contain text (repeatable; `$DIR` = tests/js)
//! In `.out` files, `$CWD` expands to the working directory (tests/js).

use std::path::{Path, PathBuf};
use std::process::Command;

struct Case {
    path: PathBuf,
    args: Vec<String>,
    argv: Option<Vec<String>>,
    exit: i32,
    stderr: Vec<String>,
}

fn parse(path: &Path) -> Case {
    let src = std::fs::read_to_string(path).unwrap();
    let mut c = Case {
        path: path.to_path_buf(),
        args: vec![],
        argv: None,
        exit: 0,
        stderr: vec![],
    };
    for line in src.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(a) = rest.strip_prefix("args: ") {
            if !a.starts_with('(') {
                c.args.extend(a.split_whitespace().map(String::from));
            }
        } else if let Some(a) = rest.strip_prefix("argv: ") {
            c.argv = Some(a.split('|').map(String::from).collect());
        } else if let Some(e) = rest.strip_prefix("exit: ") {
            c.exit = e.trim().parse().unwrap();
        } else if let Some(s) = rest.strip_prefix("stderr: ") {
            c.stderr.push(s.to_string());
        }
    }
    c
}

fn run_case(c: &Case, js_dir: &Path) -> Result<(), String> {
    let tmp = tempfile::tempdir().unwrap();
    let tmp_s = tmp.path().to_string_lossy().to_string();
    let dir_s = js_dir.to_string_lossy().to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harbor"));
    cmd.current_dir(js_dir).arg("run");
    for a in &c.args {
        cmd.arg(a.replace("$TMP", &tmp_s));
    }
    cmd.arg(c.path.file_name().unwrap());
    match &c.argv {
        Some(v) => {
            cmd.args(v);
        }
        None => {
            cmd.arg(&tmp_s);
        }
    }
    cmd.env("HARBOR_OK", "from-test");
    let out = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut errs = vec![];
    let code = out.status.code().unwrap_or(-1);
    if code != c.exit {
        errs.push(format!("exit code {code}, expected {}", c.exit));
    }
    let expected_path = c.path.with_extension("out");
    let expected = std::fs::read_to_string(&expected_path)
        .map_err(|_| format!("missing {}", expected_path.display()))?
        .replace("$CWD", &dir_s);
    if stdout != expected {
        errs.push(format!(
            "stdout mismatch\n--- expected\n{expected}--- actual\n{stdout}---"
        ));
    }
    for s in &c.stderr {
        let s = s.replace("$DIR", &dir_s);
        if !stderr.contains(&s) {
            errs.push(format!("stderr missing {s:?}"));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(format!("{}\n--- stderr\n{stderr}", errs.join("\n")))
    }
}

#[test]
fn js_corpus() {
    let js_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&js_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            !name.starts_with('_')
                && matches!(p.extension().and_then(|e| e.to_str()), Some("js" | "ts"))
        })
        .collect();
    cases.sort();
    let filter = std::env::var("HARBOR_TEST_FILTER").unwrap_or_default();
    let mut failures = vec![];
    let mut passed = 0;
    for p in &cases {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !name.contains(&filter) {
            continue;
        }
        let c = parse(p);
        match run_case(&c, &js_dir) {
            Ok(()) => {
                passed += 1;
                eprintln!("PASS {name}");
            }
            Err(e) => {
                eprintln!("FAIL {name}\n{e}");
                failures.push(name);
            }
        }
    }
    eprintln!("{passed} passed, {} failed", failures.len());
    assert!(failures.is_empty(), "failed: {failures:?}");
    assert!(passed > 0);
}
