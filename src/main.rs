use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use harbor::{JsRuntime, Outcome, Permissions, permissions::normalize_path};

const USAGE: &str = "harbor - a secure JavaScript/TypeScript runtime

USAGE:
    harbor run [PERMISSIONS] <file> [args...]
    harbor eval [PERMISSIONS] <code>
    harbor repl [PERMISSIONS]
    harbor              (starts the REPL)

PERMISSIONS (all denied by default):
    --allow-read[=<path>,...]    --allow-write[=<path>,...]
    --allow-net[=<host[:port]>,...]
    --allow-env[=<VAR>,...]      --allow-run[=<program>,...]
    -A, --allow-all
";

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn finish(outcome: Outcome) -> ExitCode {
    let _ = std::io::stdout().flush();
    match outcome {
        Outcome::Ok => ExitCode::SUCCESS,
        Outcome::Error(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

/// Split `argv` into (permissions, remaining positional args). Permission
/// flags are only recognised before the first positional argument; the
/// rest is passed to the script as `harbor.args`.
fn parse_perms(argv: &[String]) -> Result<(Permissions, Vec<String>), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut perms = Permissions::default();
    let mut i = 0;
    while i < argv.len() && argv[i].starts_with('-') {
        if argv[i] == "--" {
            i += 1;
            break;
        }
        if !perms.parse_flag(&argv[i], &cwd)? {
            return Err(format!("unknown flag '{}'", argv[i]));
        }
        i += 1;
    }
    Ok((perms, argv[i..].to_vec()))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match argv.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("repl", &[][..]),
    };
    if matches!(cmd, "-h" | "--help" | "help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(cmd, "-V" | "--version") {
        println!(
            "harbor {} (v8 {})",
            env!("CARGO_PKG_VERSION"),
            v8::V8::get_version()
        );
        return ExitCode::SUCCESS;
    }
    let (perms, positional) = match parse_perms(rest) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match cmd {
        "run" => {
            let Some((file, args)) = positional.split_first() else {
                eprintln!("error: missing file\n\n{USAGE}");
                return ExitCode::from(2);
            };
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            let path = normalize_path(Path::new(file), &cwd);
            let mut rt = JsRuntime::new(perms, args.to_vec());
            finish(tokio_rt().block_on(rt.run_main_module(&path)))
        }
        "eval" => {
            let code = positional.join(" ");
            let mut rt = JsRuntime::new(perms, vec![]);
            let outcome = tokio_rt().block_on(async {
                if let Err(e) = rt.execute_script("eval", &code) {
                    return Outcome::Error(e);
                }
                rt.run_event_loop(None).await
            });
            finish(outcome)
        }
        "repl" => repl(perms),
        other => {
            eprintln!("error: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn repl(perms: Permissions) -> ExitCode {
    let mut rt = JsRuntime::new(perms, vec![]);
    let tokio = tokio_rt();
    let stdin = std::io::stdin();
    let interactive = std::io::IsTerminal::is_terminal(&stdin);
    if interactive {
        println!(
            "harbor {} — type .exit or Ctrl-D to quit",
            env!("CARGO_PKG_VERSION")
        );
    }
    let mut buf = String::new();
    loop {
        if interactive {
            print!("{}", if buf.is_empty() { "> " } else { "... " });
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if buf.is_empty() && line.trim() == ".exit" {
            break;
        }
        buf.push_str(&line);
        if !balanced(&buf) {
            continue;
        }
        let code = std::mem::take(&mut buf);
        if code.trim().is_empty() {
            continue;
        }
        match tokio.block_on(rt.eval_and_settle(&code)) {
            Ok(v) => println!("{v}"),
            Err(e) => eprintln!("{e}"),
        }
    }
    ExitCode::SUCCESS
}

/// Very small heuristic for multi-line REPL input: wait while brackets are open.
fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut esc = false;
    for c in s.chars() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && quote.is_none()
}
