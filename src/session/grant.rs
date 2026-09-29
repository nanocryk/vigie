//! What an allow can grant beyond the call itself: the CLI's `setMode acceptEdits`
//! suggestion, its folder suggestion moved to a folder picked among the
//! suggested one and its ancestors, and the ask's file.

use std::fmt;
use std::path::Path;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::claude::protocol::{CanUseTool, Extra, PermissionRule, PermissionUpdate};
use crate::claude::tools;
use crate::session::danger;

const ACCEPT_EDITS: &str = "acceptEdits";
const SESSION: &str = "session";
const ALLOW: &str = "allow";

/// First-level folders too broad to grant, on any drive or under `/`.
const BROAD_TOP: [&str; 10] = [
    "Users",
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "home",
    "etc",
    "usr",
    "var",
    "opt",
];
/// Folder names too broad to grant wherever they sit.
const BROAD_NAMES: [&str; 1] = ["Documents"];
/// Folders under the home folder too broad to grant.
const BROAD_IN_HOME: [&[&str]; 8] = [
    &[".claude"],
    &[".ssh"],
    &[".config"],
    &["Desktop"],
    &["Downloads"],
    &["AppData"],
    &["AppData", "Local"],
    &["AppData", "Roaming"],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// A `Read` rule on the folder, as suggested for Read, Glob, Grep and Bash.
    Read,
    /// An added working folder, as suggested for Write.
    Directory,
}

/// The folder an ask suggests granting for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderOffer {
    pub kind: GrantKind,
    /// `C:\a\b` on a drive, `/a/b` otherwise.
    pub folder: String,
    /// The folder and those of its ancestors too broad to grant, in the same form.
    pub broad: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Read,
    /// Any edit tool: an Edit rule covers them all.
    Edit,
}

/// The file of a Read or edit-tool ask, which an allow can grant until the child stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    pub kind: FileKind,
    /// Absolute: `C:\a\b.md` on a drive, `/a/b.md` otherwise.
    pub path: String,
}

impl FileOffer {
    /// Relative tool paths resolve against `cwd`, the session folder.
    pub fn of(ask: &CanUseTool, cwd: &Path) -> Option<Self> {
        let kind = match ask.tool_name.as_str() {
            tools::READ => FileKind::Read,
            tool if tools::is_edit(tool) => FileKind::Edit,
            _ => return None,
        };
        let target = tools::file_target(ask)?;
        let file = Folder::parse(danger::absolute(target, cwd).to_str()?)?;
        Some(Self {
            kind,
            path: file.to_string(),
        })
    }

    /// Same kind and same file, compared as Windows compares paths.
    pub fn same(&self, other: &FileOffer) -> bool {
        self.kind == other.kind
            && Folder::parse(&self.path)
                .zip(Folder::parse(&other.path))
                .is_some_and(|(mine, theirs)| mine.same(&theirs))
    }

    /// The session rule allowing this file: `Read(//c/a/b.md)` or `Edit(//c/a/b.md)`.
    fn rule(&self) -> Option<PermissionUpdate> {
        let file = Folder::parse(&self.path)?;
        let tool = match self.kind {
            FileKind::Read => tools::READ,
            FileKind::Edit => tools::EDIT,
        };
        Some(PermissionUpdate::AddRules {
            rules: vec![PermissionRule {
                tool_name: tool.to_string(),
                rule_content: Some(file.rule_path()),
                extra: Extra::new(),
            }],
            behavior: ALLOW.to_string(),
            destination: SESSION.to_string(),
            extra: Extra::new(),
        })
    }
}

/// Whether the ask is an edit carrying the CLI's `setMode acceptEdits` suggestion.
pub fn offers_accept_edits(ask: &CanUseTool) -> bool {
    accept_edits_suggestion(ask, &ask.suggestions()).is_some()
}

/// `home` is the user's home folder, whose broad subfolders are flagged.
pub fn folder_offer(ask: &CanUseTool, home: Option<&Path>) -> Option<FolderOffer> {
    let suggestion = FolderSuggestion::find(&ask.suggestions())?;
    let home = home.and_then(Path::to_str).and_then(Folder::parse);
    Some(FolderOffer {
        kind: suggestion.kind,
        folder: suggestion.folder.to_string(),
        broad: suggestion
            .folder
            .ancestors()
            .filter(|folder| folder.is_broad(home.as_ref()))
            .map(|folder| folder.to_string())
            .collect(),
    })
}

/// The updates an allow carries: the CLI's `setMode acceptEdits` suggestion when
/// `accept_edits`, and its folder suggestion moved to `folder`, which must be the
/// suggested folder or one of its ancestors, and a session rule for `file` unless
/// the ask is a safety check, which ignores rules. The Bash command rule is never sent.
pub fn updates(
    ask: &CanUseTool,
    accept_edits: bool,
    folder: Option<&str>,
    file: Option<&FileOffer>,
) -> Result<Vec<PermissionUpdate>> {
    let suggestions = ask.suggestions();
    let mut updates = Vec::new();
    if let Some(picked) = folder {
        let suggestion = FolderSuggestion::find(&suggestions)
            .ok_or_else(|| anyhow!("{} suggests no folder", ask.tool_name))?;
        let target = Folder::parse(picked)
            .and_then(|picked| {
                suggestion
                    .folder
                    .ancestors()
                    .find(|folder| folder.same(&picked))
            })
            .ok_or_else(|| {
                anyhow!(
                    "{picked} is not {} or one of its parents",
                    suggestion.folder
                )
            })?;
        updates.push(suggestion.moved_to(&target));
    }
    if accept_edits {
        let mode = accept_edits_suggestion(ask, &suggestions)
            .ok_or_else(|| anyhow!("{} offers no accept edits", ask.tool_name))?;
        updates.push(mode.clone());
    }
    if let Some(rule) = file
        .filter(|_| !danger::is_safety_check(ask))
        .and_then(FileOffer::rule)
    {
        updates.push(rule);
    }
    Ok(updates)
}

fn accept_edits_suggestion<'a>(
    ask: &CanUseTool,
    suggestions: &'a [PermissionUpdate],
) -> Option<&'a PermissionUpdate> {
    if !tools::is_edit(&ask.tool_name) {
        return None;
    }
    suggestions.iter().find(
        |update| matches!(update, PermissionUpdate::SetMode { mode, .. } if mode == ACCEPT_EDITS),
    )
}

struct FolderSuggestion {
    update: PermissionUpdate,
    kind: GrantKind,
    folder: Folder,
}

impl FolderSuggestion {
    /// The first session-scoped suggestion of a single `Read` rule on a folder or a
    /// single added directory.
    fn find(suggestions: &[PermissionUpdate]) -> Option<Self> {
        suggestions.iter().find_map(|update| {
            let (kind, folder) = match update {
                PermissionUpdate::AddRules {
                    rules,
                    behavior,
                    destination,
                    ..
                } if destination == SESSION && behavior == ALLOW => match rules.as_slice() {
                    [rule] if rule.tool_name == tools::READ => (
                        GrantKind::Read,
                        Folder::from_read_rule(rule.rule_content.as_deref()?)?,
                    ),
                    _ => return None,
                },
                PermissionUpdate::AddDirectories {
                    directories,
                    destination,
                    ..
                } if destination == SESSION => match directories.as_slice() {
                    [directory] => (GrantKind::Directory, Folder::parse(directory)?),
                    _ => return None,
                },
                _ => return None,
            };
            Some(Self {
                update: update.clone(),
                kind,
                folder,
            })
        })
    }

    /// The same suggestion, on `target` instead.
    fn moved_to(&self, target: &Folder) -> PermissionUpdate {
        let mut update = self.update.clone();
        match &mut update {
            PermissionUpdate::AddRules { rules, .. } => {
                for rule in rules {
                    rule.rule_content = Some(target.read_rule());
                }
            }
            PermissionUpdate::AddDirectories { directories, .. } => {
                *directories = vec![target.to_string()];
            }
            PermissionUpdate::SetMode { .. } | PermissionUpdate::Other(_) => {}
        }
        update
    }
}

/// Whether the absolute `file` sits directly in `folder`, ignoring case.
pub fn directly_in(file: &str, folder: &str) -> bool {
    Folder::parse(file)
        .zip(Folder::parse(folder))
        .is_some_and(|(file, folder)| {
            file.parts.len() == folder.parts.len() + 1 && file.below(&folder).is_some()
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// Uppercase.
    Drive(char),
    Slash,
}

/// An absolute folder (or file) path without `.` or `..` parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Folder {
    root: Root,
    parts: Vec<String>,
}

impl Folder {
    /// `C:\a\b`, `c:/a/b` or `/a/b`.
    fn parse(path: &str) -> Option<Self> {
        match path.as_bytes() {
            [letter, b':', rest @ ..]
                if letter.is_ascii_alphabetic()
                    && matches!(rest.first(), None | Some(b'\\' | b'/')) =>
            {
                Self::from_parts(Root::Drive(letter.to_ascii_uppercase().into()), &path[2..])
            }
            [b'/', ..] => Self::from_parts(Root::Slash, path),
            _ => None,
        }
    }

    /// `//c/a/b/**` (drive `c`) or `//a/b/**`; a one-letter first part is a drive.
    fn from_read_rule(rule: &str) -> Option<Self> {
        let body = rule.strip_prefix("//")?.strip_suffix("/**")?;
        match body.split_once('/').unwrap_or((body, "")) {
            (drive, rest) if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() => {
                Self::from_parts(
                    Root::Drive(drive.as_bytes()[0].to_ascii_uppercase().into()),
                    rest,
                )
            }
            _ => Self::from_parts(Root::Slash, body),
        }
    }

    fn from_parts(root: Root, rest: &str) -> Option<Self> {
        let parts: Vec<String> = rest
            .split(['\\', '/'])
            .filter(|part| !part.is_empty())
            .map(String::from)
            .collect();
        if parts.iter().any(|part| part == "." || part == "..") {
            return None;
        }
        Some(Self { root, parts })
    }

    fn read_rule(&self) -> String {
        format!("{}/**", self.rule_path())
    }

    /// `//c/a/b` (drive `c`) or `//a/b`, as rules name paths.
    fn rule_path(&self) -> String {
        let mut path = String::from("/");
        if let Root::Drive(letter) = self.root {
            path.push('/');
            path.push(letter.to_ascii_lowercase());
        }
        for part in &self.parts {
            path.push('/');
            path.push_str(part);
        }
        path
    }

    /// The root first, the folder itself last.
    fn ancestors(&self) -> impl Iterator<Item = Folder> + '_ {
        (0..=self.parts.len()).map(|depth| Folder {
            root: self.root,
            parts: self.parts[..depth].to_vec(),
        })
    }

    /// Case-insensitive, as Windows compares paths.
    fn same(&self, other: &Folder) -> bool {
        self.root == other.root && same_parts(&self.parts, &other.parts)
    }

    /// The parts of `self` below `base`, when `base` contains it.
    fn below(&self, base: &Folder) -> Option<&[String]> {
        (self.root == base.root
            && self.parts.len() >= base.parts.len()
            && same_parts(&self.parts[..base.parts.len()], &base.parts))
        .then(|| &self.parts[base.parts.len()..])
    }

    fn is_broad(&self, home: Option<&Folder>) -> bool {
        let top_level = match self.parts.as_slice() {
            [] => true,
            [name] => BROAD_TOP
                .iter()
                .any(|broad| broad.eq_ignore_ascii_case(name)),
            _ => false,
        };
        let broad_name = self.parts.last().is_some_and(|name| {
            BROAD_NAMES
                .iter()
                .any(|broad| broad.eq_ignore_ascii_case(name))
        });
        let in_home = home.is_some_and(|home| {
            let parent_of_home =
                home.parts.len() == self.parts.len() + 1 && home.below(self).is_some();
            parent_of_home
                || self.below(home).is_some_and(|rest| {
                    rest.is_empty() || BROAD_IN_HOME.iter().any(|broad| same_parts(broad, rest))
                })
        });
        top_level || broad_name || in_home
    }
}

fn same_parts(left: &[impl AsRef<str>], right: &[impl AsRef<str>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.as_ref().to_lowercase() == right.as_ref().to_lowercase())
}

impl fmt::Display for Folder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.root {
            Root::Drive(letter) => write!(formatter, "{letter}:\\{}", self.parts.join("\\")),
            Root::Slash => write!(formatter, "/{}", self.parts.join("/")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const OUTSIDE: &str = r"C:\Users\user\AppData\Local\Temp\vigie2-spike\outside";

    fn ask(tool_name: &str, suggestions: Value) -> CanUseTool {
        serde_json::from_value(json!({"tool_name": tool_name, "input": {},
            "tool_use_id": "t1", "permission_suggestions": suggestions}))
        .unwrap()
    }

    fn read_rule(content: &str) -> Value {
        json!({"type": "addRules", "rules": [{"toolName": "Read", "ruleContent": content}],
            "behavior": "allow", "destination": "session"})
    }

    fn directory(path: &str) -> Value {
        json!({"type": "addDirectories", "directories": [path], "destination": "session"})
    }

    fn accept_edits() -> Value {
        json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"})
    }

    fn bash_rule() -> Value {
        json!({"type": "addRules", "rules": [{"toolName": "Bash",
            "ruleContent": format!("cat \"{OUTSIDE}\\other.txt\"")}],
            "behavior": "allow", "destination": "localSettings"})
    }

    fn sent(updates: Vec<PermissionUpdate>) -> Value {
        serde_json::to_value(updates).unwrap()
    }

    fn home() -> Option<&'static Path> {
        Some(Path::new(r"C:\Users\user"))
    }

    #[test]
    fn read_rules_and_folders_convert_both_ways() {
        let folder =
            Folder::from_read_rule("//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/**")
                .unwrap();
        assert_eq!(folder.to_string(), OUTSIDE);
        assert_eq!(Folder::parse(OUTSIDE), Some(folder.clone()));
        assert_eq!(
            folder.read_rule(),
            "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/**"
        );
        assert_eq!(Folder::parse("c:/a/b").unwrap().to_string(), r"C:\a\b");
        assert_eq!(Folder::parse(r"D:\").unwrap().read_rule(), "//d/**");
        assert_eq!(Folder::parse(r"D:\").unwrap().to_string(), r"D:\");
        let posix = Folder::from_read_rule("//home/me/src/**").unwrap();
        assert_eq!(posix.to_string(), "/home/me/src");
        assert_eq!(posix.read_rule(), "//home/me/src/**");
        for bad in ["relative", r"C:relative", r"C:\a\..\b", "/a/./b"] {
            assert_eq!(Folder::parse(bad), None, "{bad}");
        }
        for bad in ["/c/a/**", "//c/a", "//c/a/*"] {
            assert_eq!(Folder::from_read_rule(bad), None, "{bad}");
        }
    }

    #[test]
    fn broad_picks_are_flagged() {
        let home = Folder::parse(r"C:\Users\user").unwrap();
        for broad in [
            r"C:\",
            r"E:\",
            "/",
            r"C:\Users",
            r"C:\Users\user",
            r"c:\users\USER",
            r"C:\Users\user\.claude",
            r"C:\Users\user\Documents",
            r"C:\Users\user\Desktop",
            r"C:\Users\user\Downloads",
            r"C:\Users\user\AppData",
            r"C:\Users\user\AppData\Local",
            r"C:\Users\user\AppData\Roaming",
            r"C:\Users\user\.ssh",
            r"C:\Windows",
            r"C:\Program Files",
            r"E:\Libraries\Documents",
            "/home",
            "/etc",
        ] {
            let folder = Folder::parse(broad).unwrap();
            assert!(folder.is_broad(Some(&home)), "{broad}");
        }
        for fine in [
            r"C:\Users\user\.claude\plans",
            r"C:\Users\user\AppData\Local\Temp",
            r"C:\Users\user\Documents\project",
            r"C:\Users\other",
            r"E:\Libraries",
            r"E:\Libraries\Documents\vigie workspace",
            r"C:\Windows\Temp\x",
            "/home/me/src",
        ] {
            let folder = Folder::parse(fine).unwrap();
            assert!(!folder.is_broad(Some(&home)), "{fine}");
        }
        assert!(!Folder::parse("/srv/me").unwrap().is_broad(None));
        assert!(Folder::parse("/usr").unwrap().is_broad(None));
    }

    #[test]
    fn a_read_suggestion_offers_its_folder_and_flags_broad_ancestors() {
        let read = ask(
            "Read",
            json!([read_rule(
                "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/**"
            )]),
        );
        assert_eq!(
            folder_offer(&read, home()),
            Some(FolderOffer {
                kind: GrantKind::Read,
                folder: OUTSIDE.into(),
                broad: vec![
                    r"C:\".into(),
                    r"C:\Users".into(),
                    r"C:\Users\user".into(),
                    r"C:\Users\user\AppData".into(),
                    r"C:\Users\user\AppData\Local".into(),
                ],
            })
        );
        assert!(!offers_accept_edits(&read));
    }

    #[test]
    fn bash_offers_only_its_read_folder() {
        let bash = ask(
            "Bash",
            json!([
                bash_rule(),
                read_rule("//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/**")
            ]),
        );
        let offer = folder_offer(&bash, home()).unwrap();
        assert_eq!(offer.kind, GrantKind::Read);
        assert_eq!(offer.folder, OUTSIDE);
        assert_eq!(
            sent(
                updates(
                    &bash,
                    false,
                    Some(r"C:\Users\user\AppData\Local\Temp"),
                    None
                )
                .unwrap()
            ),
            json!([read_rule("//c/Users/user/AppData/Local/Temp/**")]),
        );
        assert_eq!(sent(updates(&bash, false, None, None).unwrap()), json!([]));
        let only_command = ask("Bash", json!([bash_rule()]));
        assert_eq!(folder_offer(&only_command, home()), None);
        assert!(updates(&only_command, false, Some(r"C:\Users"), None).is_err());
    }

    #[test]
    fn a_read_grant_moves_to_the_picked_parent() {
        let read = ask(
            "Read",
            json!([read_rule(
                "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/a/b/**"
            )]),
        );
        assert_eq!(
            sent(updates(&read, false, Some(&format!("{OUTSIDE}\\a")), None).unwrap()),
            json!([read_rule(
                "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/a/**"
            )]),
        );
        let lowercase = format!("{}\\A\\B", OUTSIDE.to_lowercase());
        assert_eq!(
            sent(updates(&read, false, Some(&lowercase), None).unwrap()),
            json!([read_rule(
                "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/a/b/**"
            )]),
            "the suggested spelling wins"
        );
        for outside in [&format!("{OUTSIDE}\\c") as &str, r"D:\", "relative"] {
            assert!(
                updates(&read, false, Some(outside), None).is_err(),
                "{outside}"
            );
        }
        assert!(
            updates(&read, true, None, None).is_err(),
            "Read offers no accept edits"
        );
    }

    #[test]
    fn a_directory_grant_moves_to_the_picked_parent() {
        let write = ask("Write", json!([directory(&format!("{OUTSIDE}\\w"))]));
        let offer = folder_offer(&write, home()).unwrap();
        assert_eq!(offer.kind, GrantKind::Directory);
        assert_eq!(offer.folder, format!("{OUTSIDE}\\w"));
        assert_eq!(
            sent(updates(&write, false, Some(OUTSIDE), None).unwrap()),
            json!([directory(OUTSIDE)]),
        );
        assert_eq!(
            sent(updates(&write, false, Some(r"C:\"), None).unwrap()),
            json!([directory(r"C:\")]),
        );
    }

    #[test]
    fn accept_edits_passes_the_suggestion_through() {
        let write = ask("Write", json!([accept_edits()]));
        assert!(offers_accept_edits(&write));
        assert_eq!(folder_offer(&write, home()), None);
        assert_eq!(
            sent(updates(&write, true, None, None).unwrap()),
            json!([accept_edits()])
        );
        assert_eq!(sent(updates(&write, false, None, None).unwrap()), json!([]));

        let both = ask(
            "Edit",
            json!([accept_edits(), directory(&format!("{OUTSIDE}\\w"))]),
        );
        assert_eq!(
            sent(updates(&both, true, Some(OUTSIDE), None).unwrap()),
            json!([directory(OUTSIDE), accept_edits()]),
        );

        let bash = ask("Bash", json!([accept_edits()]));
        assert!(!offers_accept_edits(&bash), "only edit tools offer it");
        assert!(!offers_accept_edits(&ask("Write", json!([]))));
        assert!(!offers_accept_edits(&ask("Write", json!("odd"))));
    }

    fn tool(tool_name: &str, input: Value, reason_type: Option<&str>) -> CanUseTool {
        serde_json::from_value(json!({"tool_name": tool_name, "input": input,
            "tool_use_id": "t1", "decision_reason_type": reason_type}))
        .unwrap()
    }

    fn edit_rule(tool_name: &str, content: &str) -> Value {
        json!({"type": "addRules", "rules": [{"toolName": tool_name, "ruleContent": content}],
            "behavior": "allow", "destination": "session"})
    }

    #[cfg(windows)]
    #[test]
    fn read_and_edit_asks_offer_their_file() {
        let cwd = Path::new(r"C:\work\project");
        let offer = |name: &str, input: Value| FileOffer::of(&tool(name, input, None), cwd);
        let plan = r"C:\Users\user\.claude\plans\x.md";
        assert_eq!(
            offer("Read", json!({"file_path": plan})),
            Some(FileOffer {
                kind: FileKind::Read,
                path: plan.into()
            })
        );
        for (name, key) in [
            ("Edit", "file_path"),
            ("Write", "file_path"),
            ("MultiEdit", "file_path"),
            ("NotebookEdit", "notebook_path"),
        ] {
            assert_eq!(
                offer(name, json!({key: "src/../notes/a.md"})),
                Some(FileOffer {
                    kind: FileKind::Edit,
                    path: r"C:\work\project\notes\a.md".into()
                }),
                "{name}"
            );
        }
        assert_eq!(offer("Bash", json!({"command": "cat a"})), None);
        assert_eq!(offer("Glob", json!({"pattern": "*.md"})), None);
        assert_eq!(offer("Edit", json!({})), None);
    }

    #[test]
    fn file_offers_match_by_kind_and_path_without_case() {
        let edit = FileOffer {
            kind: FileKind::Edit,
            path: r"C:\Users\user\.claude\plans\x.md".into(),
        };
        let same = FileOffer {
            path: r"c:\users\USER\.claude\plans\X.md".into(),
            ..edit.clone()
        };
        assert!(edit.same(&same));
        assert!(!edit.same(&FileOffer {
            kind: FileKind::Read,
            ..edit.clone()
        }));
        assert!(!edit.same(&FileOffer {
            path: r"C:\Users\user\.claude\plans\y.md".into(),
            ..edit.clone()
        }));
    }

    #[test]
    fn a_file_grant_sends_its_rule_unless_a_safety_check_asked() {
        let file = FileOffer {
            kind: FileKind::Edit,
            path: format!("{OUTSIDE}\\note.md"),
        };
        let outside = tool("Edit", json!({}), Some("workingDir"));
        assert_eq!(
            sent(updates(&outside, false, None, Some(&file)).unwrap()),
            json!([edit_rule(
                "Edit",
                "//c/Users/user/AppData/Local/Temp/vigie2-spike/outside/note.md"
            )]),
        );
        let read = FileOffer {
            kind: FileKind::Read,
            path: "/home/me/a.md".into(),
        };
        assert_eq!(
            sent(updates(&outside, false, None, Some(&read)).unwrap()),
            json!([edit_rule("Read", "//home/me/a.md")]),
        );
        let sensitive = tool("Edit", json!({}), Some("safetyCheck"));
        assert_eq!(
            sent(updates(&sensitive, false, None, Some(&file)).unwrap()),
            json!([])
        );
    }
}
