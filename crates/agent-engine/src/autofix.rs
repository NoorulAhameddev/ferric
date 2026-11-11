use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    pub test_name: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub failures: Vec<TestFailure>,
    pub raw_output: String,
}

#[derive(Debug, Clone)]
pub struct TestRunner {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AutoFixReport {
    pub initial_response: String,
    pub attempts: usize,
    pub final_tests_passed: bool,
    pub history: Vec<String>,
}

/// Detects the standard test runner for the workspace root
pub fn detect_test_runner(root: &Path) -> Option<TestRunner> {
    if root.join("Cargo.toml").exists() {
        return Some(TestRunner {
            name: "Cargo (Rust)".into(),
            command: "cargo".into(),
            args: vec!["test".into(), "--".into(), "--nocapture".into()],
        });
    }
    if root.join("package.json").exists() {
        #[cfg(target_os = "windows")]
        let npm_cmd = "npm.cmd";
        #[cfg(not(target_os = "windows"))]
        let npm_cmd = "npm";

        return Some(TestRunner {
            name: "npm (Node.js)".into(),
            command: npm_cmd.into(),
            args: vec!["test".into()],
        });
    }
    if root.join("pytest.ini").exists()
        || root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("tests").is_dir()
    {
        return Some(TestRunner {
            name: "pytest (Python)".into(),
            command: "pytest".into(),
            args: vec!["-v".into()],
        });
    }
    if root.join("go.mod").exists() {
        return Some(TestRunner {
            name: "Go Test".into(),
            command: "go".into(),
            args: vec!["test".into(), "./...".into()],
        });
    }
    None
}

/// Parses custom test command string into program and arguments
pub fn parse_custom_test_cmd(cmd_str: &str) -> Option<TestRunner> {
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let command = parts[0].to_string();
    let args = parts[1..].iter().map(|s| s.to_string()).collect();
    Some(TestRunner {
        name: format!("Custom ({cmd_str})"),
        command,
        args,
    })
}

/// Executes the test runner in the workspace root
pub fn run_test_suite(runner: &TestRunner, root: &Path) -> Result<TestResult, String> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd.exe");
        c.arg("/C");
        c.arg(&runner.command);
        c.args(&runner.args);
        c
    };

    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new(&runner.command);
        c.args(&runner.args);
        c
    };

    cmd.current_dir(root);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute test runner '{}': {e}", runner.command))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    let success = output.status.success();
    let exit_code = output.status.code();

    let failures = if !success {
        parse_test_failures(&combined)
    } else {
        Vec::new()
    };

    Ok(TestResult {
        success,
        exit_code,
        failures,
        raw_output: combined,
    })
}

/// Extracts structured test failures from compiler and test runner outputs
pub fn parse_test_failures(output: &str) -> Vec<TestFailure> {
    let mut failures = Vec::new();
    let lines: Vec<&str> = output.lines().collect();

    // 1. Rust cargo test panic pattern:
    // ---- <test_name> stdout ----
    // thread '<thread>' panicked at <path>:<line>:<col>:
    // <message>
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("---- ") && line.ends_with(" stdout ----") {
            let test_name = line
                .trim_start_matches("---- ")
                .trim_end_matches(" stdout ----")
                .trim()
                .to_string();

            let mut file = None;
            let mut line_num = None;
            let mut message = Vec::new();
            i += 1;

            while i < lines.len()
                && !lines[i].starts_with("---- ")
                && !lines[i].starts_with("failures:")
            {
                let cur = lines[i];
                if cur.contains("panicked at ") {
                    let parts: Vec<&str> = cur.split("panicked at ").collect();
                    if parts.len() > 1 {
                        let loc_part = parts[1].trim().trim_end_matches(':');
                        let loc_sub: Vec<&str> = loc_part.split(':').collect();
                        if loc_sub.len() >= 2 {
                            file = Some(loc_sub[0].to_string());
                            if let Ok(num) = loc_sub[1].parse::<usize>() {
                                line_num = Some(num);
                            }
                        }
                    }
                } else if !cur.trim().is_empty() && !cur.starts_with("stack backtrace:") {
                    message.push(cur.trim());
                }
                i += 1;
            }

            failures.push(TestFailure {
                test_name,
                file,
                line: line_num,
                message: message.join("\n"),
            });
            continue;
        }

        // 2. Python pytest failure pattern:
        // FAILED tests/test_foo.py::test_bar - AssertionError: ...
        if line.contains("FAILED ") && line.contains("::") {
            let parts: Vec<&str> = line.split("FAILED ").collect();
            if parts.len() > 1 {
                let rest = parts[1].trim();
                let (loc, msg) = rest.split_once(" - ").unwrap_or((rest, "AssertionError"));
                let (path, fn_name) = loc.split_once("::").unwrap_or((loc, "test"));
                failures.push(TestFailure {
                    test_name: fn_name.to_string(),
                    file: Some(path.to_string()),
                    line: None,
                    message: msg.to_string(),
                });
            }
        }

        // 3. Jest / npm failure pattern:
        // ● <test suite> › <test name>
        if line.contains("● ") {
            let test_name = line.trim_start_matches("● ").trim().to_string();
            failures.push(TestFailure {
                test_name,
                file: None,
                line: None,
                message: "Test suite assertion failed".into(),
            });
        }

        i += 1;
    }

    // Fallback: If test failed with non-zero exit code but no specific test pattern matched,
    // emit a general compilation/execution failure entry
    if failures.is_empty() {
        let last_lines: Vec<&str> = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .copied()
            .rev()
            .take(15)
            .collect();
        let mut msg_lines = last_lines;
        msg_lines.reverse();
        failures.push(TestFailure {
            test_name: "test_suite_execution".into(),
            file: None,
            line: None,
            message: msg_lines.join("\n"),
        });
    }

    failures
}

/// Formats test failures into an actionable prompt for autonomous repair
pub fn format_repair_prompt(runner: &TestRunner, result: &TestResult, attempt: usize) -> String {
    let mut prompt = format!(
        "Autonomous Test Verification Attempt #{} failed for {}.\n\n",
        attempt, runner.name
    );

    prompt.push_str(&format!("Test Failures ({}):\n", result.failures.len()));
    for (idx, f) in result.failures.iter().enumerate() {
        let loc = match (&f.file, f.line) {
            (Some(file), Some(line)) => format!(" ({file}:{line})"),
            (Some(file), None) => format!(" ({file})"),
            _ => String::new(),
        };
        prompt.push_str(&format!("{}. `{}`{}\n", idx + 1, f.test_name, loc));
        if !f.message.trim().is_empty() {
            prompt.push_str(&format!("   Error:\n```\n{}\n```\n", f.message.trim()));
        }
    }

    prompt.push_str(
        "\nTask: Analyze the failure details above, inspect the relevant code, apply the necessary fix using FileEdit or PatchApply, and ensure all tests pass.\n"
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cargo_test_panic() {
        let output = r#"
running 1 test
test tests::test_calculator ... FAILED

failures:

---- tests::test_calculator stdout ----
thread 'tests::test_calculator' panicked at src/calc.rs:24:9:
assertion `left == right` failed
  left: 4
 right: 5

failures:
    tests::test_calculator

test result: FAILED. 0 passed; 1 failed; 0 ignored
"#;

        let failures = parse_test_failures(output);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].test_name, "tests::test_calculator");
        assert_eq!(failures[0].file.as_deref(), Some("src/calc.rs"));
        assert_eq!(failures[0].line, Some(24));
        assert!(failures[0]
            .message
            .contains("assertion `left == right` failed"));
    }

    #[test]
    fn test_parse_pytest_failure() {
        let output = r#"
============================= FAILURES =============================
____________________________ test_login ____________________________
FAILED tests/test_auth.py::test_login - AssertionError: assert False
===================== 1 failed in 0.12s =====================
"#;

        let failures = parse_test_failures(output);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].test_name, "test_login");
        assert_eq!(failures[0].file.as_deref(), Some("tests/test_auth.py"));
        assert!(failures[0].message.contains("AssertionError"));
    }

    #[test]
    fn test_custom_test_cmd_parsing() {
        let runner = parse_custom_test_cmd("cargo test --package agent-core").unwrap();
        assert_eq!(runner.command, "cargo");
        assert_eq!(runner.args, vec!["test", "--package", "agent-core"]);
    }
}
