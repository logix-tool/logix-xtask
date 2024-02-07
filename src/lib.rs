#![deny(warnings, clippy::all)]

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Command,
};

struct Vars {
    cwd: PathBuf,
    target_dir: PathBuf,
    verbose: bool,
}

impl Vars {
    fn verbose_arg(&self) -> &'static [&'static str] {
        if self.verbose {
            &["--verbose"]
        } else {
            &[]
        }
    }
}

enum Action<'a> {
    Cargo(&'a str, &'a [&'a str]),
    Call(&'static (dyn Fn(&Vars) + Sync)),
    Run(&'a str),
}

use Action::*;

static ACTIONS: &[(&str, &[Action])] = &[
    (
        "before-pr",
        &[
            Cargo("update", &[]),
            Run("lints"),
            Run("build-all"),
            Run("all-tests"),
            Run("all-checks"),
        ],
    ),
    (
        "all-checks",
        &[
            Cargo("deny", &["check"]),
            Cargo("semver-checks", &[]),
            Cargo("outdated", &["--exit-code", "1"]),
            // TODO(2023.10): NightlyCargo("udeps", &[]),
            // TODO(2023.10): Cargo("audit", &[]),
            // TODO(2023.10): Cargo("pants", &[]),
        ],
    ),
    (
        "lints",
        &[
            Cargo("fmt", &["--check"]),
            Cargo("clippy", &["--workspace"]),
        ],
    ),
    (
        "build-all",
        &[
            Cargo("build", &["--workspace"]),
            Cargo("build", &["--workspace", "--tests"]),
            Cargo("build", &["--workspace", "--release"]),
        ],
    ),
    ("all-tests", &[Cargo("test", &["--workspace"])]),
    ("lcov-coverage", &[Call(&run_lcov_coverage)]),
    ("html-coverage", &[Call(&run_html_coverage)]),
];

fn grcov(target_dir: &Path, format: &str, build_type: &str) {
    let ret = Command::new("grcov")
        .args(["."])
        .args([
            "--binary-path",
            target_dir
                .join(format!("{build_type}/deps"))
                .to_str()
                .unwrap(),
        ])
        .args(["-s", "."])
        .args(["-t", format])
        .args(["--branch"])
        .args(["--ignore-not-existing"])
        .args(["-o", target_dir.join(format).to_str().unwrap()])
        .args(["--keep-only", "src/*"])
        .args(["--keep-only", "derive/src/*"])
        .status()
        .expect("Perhaps you need to run 'cargo install grcov'")
        .success();
    assert!(ret);
}

fn run_lcov_coverage(vars: &Vars) {
    code_coverage(vars, "lcov")
}

fn run_html_coverage(vars: &Vars) {
    code_coverage(vars, "html")
}

fn code_coverage(vars: &Vars, format: &str) {
    let build_type = "debug";
    let target_dir = vars.target_dir.join(format!("coverage-{format}"));

    if target_dir.is_dir() {
        std::fs::remove_dir_all(&target_dir)
            .unwrap_or_else(|e| panic!("Failed to delete {target_dir:?}: {e}"));
    }

    let ret = Command::new("cargo")
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_INCREMENTAL", "0")
        .env("RUSTFLAGS", "-Cinstrument-coverage")
        .env(
            "LLVM_PROFILE_FILE",
            target_dir.join("cargo-test-%p-%m.profraw"),
        )
        .arg("test")
        .arg("--workspace")
        .args(match build_type {
            "release" => vec!["--release"],
            "debug" => vec![],
            _ => unreachable!("{build_type:?}"),
        })
        .status()
        .unwrap_or_else(|e| panic!("Failed to run cargo: {e}"))
        .success();
    assert!(ret);

    match format {
        "html" => {
            grcov(&target_dir, "html", build_type);
            grcov(&target_dir, "lcov", build_type);

            let ret = Command::new("genhtml")
                .args(["-o", target_dir.join("html2").to_str().unwrap()])
                .args(["--show-details"])
                .args(["--highlight"])
                .args(["--ignore-errors", "source"])
                .args(["--legend", target_dir.join("lcov").to_str().unwrap()])
                .status()
                .unwrap_or_else(|e| panic!("Failed to run genhtml: {e}"))
                .success();
            assert!(ret);

            println!("Now open:");
            println!(
                "  file://{}/html/index.html",
                vars.cwd.join(&target_dir).display()
            );
            println!(
                "  file://{}/html2/index.html",
                vars.cwd.join(&target_dir).display()
            );
        }
        "lcov" => {
            grcov(&target_dir, "lcov", build_type);
        }
        _ => panic!("Unknown format {format:?}"),
    }
}

fn cargo_cmd(command: &str, args: &[&str], vars: &Vars) {
    print!("Running cargo {command}");
    for arg in args.iter() {
        print!(" {arg}");
    }
    println!();

    let ret = Command::new("cargo")
        .args(vars.verbose_arg())
        .arg(command)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("Failed to run cargo: {e}"))
        .success();
    assert!(ret);
}

pub fn run_xtask() {
    let mut vars = Vars {
        cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
        target_dir: std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "target".into())
            .canonicalize()
            .unwrap(),
        verbose: false,
    };

    let mut tasks = VecDeque::new();

    for arg in std::env::args().skip(1) {
        if arg == "--verbose" || arg == "-v" {
            vars.verbose = true;
        } else if let Some((_, actions)) = ACTIONS.iter().find(|&&(t, _)| arg == t) {
            tasks.extend(actions.iter());
        } else {
            eprintln!("Invalid argument {arg:?}");
            std::process::exit(1);
        }
    }

    if tasks.is_empty() {
        eprint!("Missing action, use one of ");
        for (i, &(t, _)) in ACTIONS.iter().enumerate() {
            if i != 0 {
                eprint!(", {t}");
            } else {
                eprint!("{t}");
            }
        }
        eprintln!();
        std::process::exit(1);
    }

    while let Some(action) = tasks.pop_front() {
        match *action {
            Action::Cargo(cmd, args) => cargo_cmd(cmd, args, &vars),
            Action::Call(clb) => clb(&vars),
            Action::Run(name) => {
                tasks.extend(
                    ACTIONS
                        .iter()
                        .find(|&&(t, _)| name == t)
                        .unwrap_or_else(|| panic!("Unknown action {name}"))
                        .1
                        .iter(),
                );
            }
        }
    }
}
