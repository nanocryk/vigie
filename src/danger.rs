//! Flags asks that deserve a second look before approval: what the CLI marks
//! risky, plus destructive shell commands and writes outside the session folder.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::CanUseTool;

const SAFETY_CHECK: &str = "safetyCheck";
/// Words that run the command after them.
const PREFIXES: [&str; 12] = [
    "sudo", "doas", "command", "exec", "nohup", "time", "env", "xargs", "!", "{", "then", "do",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Danger {
    pub dangerous: bool,
    /// One line per rule that matched, in rule order.
    pub reasons: Vec<String>,
}

impl Danger {
    pub fn from_reasons(reasons: Vec<String>) -> Self {
        let mut unique: Vec<String> = Vec::new();
        for reason in reasons {
            if !unique.contains(&reason) {
                unique.push(reason);
            }
        }
        Self {
            dangerous: !unique.is_empty(),
            reasons: unique,
        }
    }
}

/// `cwd` is the session folder; relative tool paths resolve against it.
pub fn classify(ask: &CanUseTool, cwd: &Path) -> Danger {
    let mut reasons = Vec::new();
    if ask.default_to_no == Some(Value::Bool(true)) {
        reasons.push("Claude Code defaults this request to no".to_string());
    }
    if ask.decision_reason_type.as_ref().and_then(Value::as_str) == Some(SAFETY_CHECK) {
        match ask.decision_reason.as_ref().and_then(Value::as_str) {
            Some(reason) => reasons.push(format!("Claude Code safety check: {reason}")),
            None => reasons.push("Claude Code safety check".to_string()),
        }
    }
    if ask.classifier_approvable == Some(Value::Bool(false)) {
        reasons.push("The permission classifier cannot approve this".to_string());
    }
    match ask.tool_name.as_str() {
        "Bash" => {
            if let Some(command) = ask.input.get("command").and_then(Value::as_str) {
                reasons.extend(command_reasons(command));
            }
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let key = if ask.tool_name == "NotebookEdit" {
                "notebook_path"
            } else {
                "file_path"
            };
            let target = ask
                .input
                .get(key)
                .and_then(Value::as_str)
                .or_else(|| ask.blocked_path.as_ref().and_then(Value::as_str));
            if let Some(target) = target
                && !is_inside(target, cwd)
            {
                reasons.push(format!("Writes outside the session folder: {target}"));
            }
        }
        _ => {}
    }
    Danger::from_reasons(reasons)
}

/// Reasons from each simple command of a shell command line.
pub fn command_reasons(command: &str) -> Vec<String> {
    simple_commands(command)
        .iter()
        .filter_map(|words| command_reason(words))
        .collect()
}

fn command_reason(words: &[String]) -> Option<String> {
    let start = command_start(words)?;
    let name = program_name(&words[start]);
    let args = &words[start + 1..];
    let text = words[start..].join(" ");
    match name.as_str() {
        "rm" if is_recursive_rm(args) => Some(format!("Recursive delete: {text}")),
        "git" => match git_subcommand(args) {
            Some(("push", rest)) if is_force_push(rest) => Some(format!("Force push: {text}")),
            Some(("reset", rest)) if rest.iter().any(|arg| arg == "--hard") => {
                Some(format!("Hard reset: {text}"))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Index of the program word, past prefixes such as `sudo`, their options and
/// variable assignments.
fn command_start(words: &[String]) -> Option<usize> {
    let mut after_prefix = false;
    for (index, word) in words.iter().enumerate() {
        if PREFIXES.contains(&word.as_str()) {
            after_prefix = true;
            continue;
        }
        if (after_prefix && word.starts_with('-')) || is_assignment(word) {
            continue;
        }
        return Some(index);
    }
    None
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `/usr/bin/rm` and `rm.exe` both name `rm`.
fn program_name(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn is_recursive_rm(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| {
            arg == "--recursive"
                || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(['r', 'R']))
        })
}

/// The subcommand and its arguments, past git's own options.
fn git_subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if matches!(
            arg.as_str(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            index += 2;
        } else if arg.starts_with('-') {
            index += 1;
        } else {
            return Some((arg.as_str(), &args[index + 1..]));
        }
    }
    None
}

fn is_force_push(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--force"
            || arg.starts_with("--force-with-lease")
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('f'))
    })
}

/// Splits a command line into simple commands at `&&`, `||`, `;`, `|`, `&`,
/// parentheses, backquotes and newlines outside quotes; quotes are removed.
fn simple_commands(command: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if open == '"' && c == '\\' {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                in_word = true;
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(next) => {
                    word.push(next);
                    in_word = true;
                }
            },
            ' ' | '\t' | '\r' => end_word(&mut words, &mut word, &mut in_word),
            '\n' | ';' | '&' | '|' | '(' | ')' | '`' => {
                end_word(&mut words, &mut word, &mut in_word);
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            _ => {
                word.push(c);
                in_word = true;
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

fn end_word(words: &mut Vec<String>, word: &mut String, in_word: &mut bool) {
    if *in_word {
        words.push(std::mem::take(word));
        *in_word = false;
    }
}

/// Whether `target`, resolved against `cwd` and normalized, lies inside `cwd`.
pub fn is_inside(target: &str, cwd: &Path) -> bool {
    let target = Path::new(target);
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd.join(target)
    };
    let base = normalize(cwd);
    let path = normalize(&absolute);
    let mut path_components = path.components();
    base.components().all(|component| {
        path_components
            .next()
            .is_some_and(|other| same_component(component, other))
    })
}

/// Resolves `.` and `..` without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn same_component(left: Component<'_>, right: Component<'_>) -> bool {
    let left = left.as_os_str().to_string_lossy();
    let right = right.as_os_str().to_string_lossy();
    if cfg!(windows) {
        left.to_lowercase() == right.to_lowercase()
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ask(fields: Value) -> CanUseTool {
        let mut base =
            json!({"tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"});
        for (key, value) in fields.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn cwd() -> PathBuf {
        std::env::temp_dir().join("vigie2-danger").join("project")
    }

    fn bash(command: &str) -> Danger {
        classify(&ask(json!({"input": {"command": command}})), &cwd())
    }

    #[test]
    fn a_plain_ask_is_safe() {
        assert_eq!(bash("ls -la"), Danger::default());
    }

    #[test]
    fn cli_flags_mark_danger() {
        let danger = classify(&ask(json!({"default_to_no": true})), &cwd());
        assert!(danger.dangerous);
        assert_eq!(danger.reasons.len(), 1);

        let danger = classify(
            &ask(json!({"decision_reason_type": "safetyCheck",
                "decision_reason": "Touches .git"})),
            &cwd(),
        );
        assert_eq!(danger.reasons, ["Claude Code safety check: Touches .git"]);

        let danger = classify(&ask(json!({"classifier_approvable": false})), &cwd());
        assert!(danger.dangerous);

        let calm = classify(
            &ask(
                json!({"default_to_no": false, "classifier_approvable": true,
                "decision_reason_type": "rule"}),
            ),
            &cwd(),
        );
        assert!(!calm.dangerous);
    }

    #[test]
    fn recursive_rm_is_dangerous() {
        for command in [
            "rm -r build",
            "rm -R build",
            "rm -rf build",
            "rm -fr build",
            "rm --recursive build",
            "rm -v -r build",
            "/bin/rm -rf build",
            "sudo rm -rf /",
        ] {
            assert!(bash(command).dangerous, "{command}");
        }
    }

    #[test]
    fn plain_rm_and_other_recursive_flags_are_safe() {
        for command in [
            "rm file",
            "rm -f file",
            "rm -- -r",
            "grep -r foo .",
            "cp -r a b",
        ] {
            assert!(!bash(command).dangerous, "{command}");
        }
    }

    #[test]
    fn force_push_is_dangerous() {
        for command in [
            "git push --force",
            "git push -f origin main",
            "git push --force-with-lease",
            "git push --force-with-lease=main:abc origin main",
            "git -C repo push -uf origin main",
        ] {
            assert!(bash(command).dangerous, "{command}");
        }
        for command in ["git push", "git push -u origin main", "git fetch --force"] {
            assert!(!bash(command).dangerous, "{command}");
        }
    }

    #[test]
    fn hard_reset_is_dangerous() {
        assert!(bash("git reset --hard HEAD~1").dangerous);
        assert!(!bash("git reset --soft HEAD~1").dangerous);
        assert!(!bash("git reset").dangerous);
    }

    #[test]
    fn chained_commands_are_each_checked() {
        let danger = bash("cd build && rm -rf out; git status | git reset --hard");
        assert_eq!(
            danger.reasons,
            [
                "Recursive delete: rm -rf out",
                "Hard reset: git reset --hard"
            ]
        );
        assert!(bash("make || rm -r dist").dangerous);
        assert!(bash("(cd x; rm -rf y)").dangerous);
        assert!(bash("FOO=1 git push --force").dangerous);
        assert!(bash("find . -name '*.tmp' | xargs rm -rf").dangerous);
    }

    #[test]
    fn quoted_operators_do_not_split() {
        assert!(!bash("echo 'a && rm -rf /'").dangerous);
        assert!(!bash("git commit -m \"fix; rm -rf note\"").dangerous);
    }

    fn write(tool: &str, key: &str, target: &str) -> Danger {
        classify(
            &ask(json!({"tool_name": tool, "input": {key: target}})),
            &cwd(),
        )
    }

    #[test]
    fn writes_inside_the_session_folder_are_safe() {
        let inside = cwd().join("src").join("main.rs");
        assert!(!write("Write", "file_path", inside.to_str().unwrap()).dangerous);
        assert!(!write("Edit", "file_path", "src/lib.rs").dangerous);
        assert!(!write("MultiEdit", "file_path", "a/../b.rs").dangerous);
        assert!(!write("NotebookEdit", "notebook_path", "nb.ipynb").dangerous);
    }

    #[test]
    fn writes_outside_the_session_folder_are_dangerous() {
        let sibling = cwd().join("..").join("other").join("x.txt");
        for (tool, key, target) in [
            ("Write", "file_path", sibling.to_str().unwrap()),
            ("Edit", "file_path", "../x.txt"),
            ("MultiEdit", "file_path", "sub/../../x.txt"),
            ("NotebookEdit", "notebook_path", "../nb.ipynb"),
        ] {
            let danger = write(tool, key, target);
            assert_eq!(
                danger.reasons,
                [format!("Writes outside the session folder: {target}")],
                "{tool} {target}"
            );
        }
        let prefix_only = format!("{}-other/x.txt", cwd().display());
        assert!(write("Write", "file_path", &prefix_only).dangerous);
        assert!(
            !classify(
                &ask(json!({"tool_name": "Read", "input": {"file_path": "../x"}})),
                &cwd()
            )
            .dangerous
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_without_case() {
        let upper = cwd().to_string_lossy().to_uppercase();
        assert!(!write("Write", "file_path", &format!("{upper}\\a.txt")).dangerous);
        assert!(!write("Write", "file_path", "sub/a.txt").dangerous);
    }
}
