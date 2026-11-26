//! Reading and writing profile directories.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::edit::toml::{json_to_table, json_to_value};
use crate::frontmatter::{self, glob_list, opt, string};
use crate::model::{
    Command, FORMAT_VERSION, McpServer, Profile, Rule, Skill, SkillFile, is_valid_name,
};
use crate::secrets::{
    redact_server, refs_in, sensitive_file, strong_secret_kind, text_secret_kind,
};

pub const MANIFEST: &str = "kitbag.toml";
/// Most files a skill may have. Capture and loading use the same limit.
pub const MAX_SKILL_FILES: usize = 200;
/// Largest file a profile may contain. Capture skips anything bigger, so it
/// never writes a profile that can't be loaded.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// `MAX_FILE_BYTES` for messages.
pub fn max_file_size() -> String {
    format!("{} MiB", MAX_FILE_BYTES / (1024 * 1024))
}

/// A profile that failed validation, with every problem found.
#[derive(Debug, thiserror::Error)]
#[error("{message}{}", issues.iter().map(|i| format!("\n  - {i}")).collect::<String>())]
pub struct ProfileError {
    pub message: String,
    pub issues: Vec<String>,
}

impl ProfileError {
    fn new(message: impl Into<String>, issues: Vec<String>) -> Self {
        ProfileError {
            message: message.into(),
            issues,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    kitbag: u32,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServer>,
    #[serde(default)]
    settings: BTreeMap<String, Map<String, Value>>,
}

/// Parse and validate a `kitbag.toml` manifest into a profile without file-based parts.
pub fn parse_manifest(text: &str, file: &Path) -> Result<Profile, ProfileError> {
    let m: Manifest = toml::from_str(text).map_err(|e| {
        ProfileError::new(
            format!("{} is not a valid kitbag manifest", file.display()),
            vec![crate::edit::safe_parse_error(
                text,
                e.span().map(|s| s.start),
                e.message(),
            )],
        )
    })?;
    let mut issues = Vec::new();
    if m.kitbag != FORMAT_VERSION {
        issues.push(format!(
            "'kitbag' must be {FORMAT_VERSION} (the profile format version), got {}; upgrade kitbag to read newer profiles",
            m.kitbag
        ));
    }
    if m.name.trim().is_empty() {
        issues.push("'name' must not be empty".into());
    }
    for (name, s) in &m.mcp_servers {
        if !is_valid_name(name) {
            issues.push(format!(
                "mcp_servers.{name}: names may only contain letters, digits, '.', '_' and '-'"
            ));
        }
        for i in s.validate() {
            issues.push(format!("mcp_servers.{name}: {i}"));
        }
    }
    if !issues.is_empty() {
        return Err(ProfileError::new(
            format!("{} is not a valid kitbag profile", file.display()),
            issues,
        ));
    }
    Ok(Profile {
        name: m.name.trim().to_string(),
        description: m.description.filter(|d| !d.trim().is_empty()),
        mcp_servers: m.mcp_servers,
        settings: m.settings,
        ..Profile::default()
    })
}

/// Read a file from an (untrusted) profile directory: no symlinks, bounded size.
fn read_profile_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if meta.file_type().is_symlink() {
        // A profile may come from someone else's repository; following a link
        // could copy arbitrary local files (keys, tokens) into a tool's config.
        anyhow::bail!(
            "symbolic links are not allowed in a profile: {}",
            path.display()
        );
    }
    if meta.len() > MAX_FILE_BYTES {
        anyhow::bail!("{} is larger than {}", path.display(), max_file_size());
    }
    Ok(Some(
        fs::read(path).with_context(|| format!("reading {}", path.display()))?,
    ))
}

fn read_profile_text(path: &Path) -> Result<Option<String>> {
    match read_profile_file(path)? {
        None => Ok(None),
        Some(b) => {
            Ok(Some(String::from_utf8(b).with_context(|| {
                format!("{} is not valid UTF-8", path.display())
            })?))
        }
    }
}

/// Directory entries `(name, is_dir)`, sorted; rejects symlinks. Missing dir -> empty.
fn list_profile_dir(dir: &Path) -> Result<Vec<(String, bool)>> {
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        anyhow::bail!(
            "symbolic links are not allowed in a profile: {}",
            dir.display()
        );
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_symlink() {
            anyhow::bail!(
                "symbolic links are not allowed in a profile: {}",
                e.path().display()
            );
        }
        out.push((e.file_name().to_string_lossy().into_owned(), ft.is_dir()));
    }
    out.sort();
    Ok(out)
}

/// Parse a rule markdown file (profile format; also accepts `paths` / `applyTo` aliases).
pub fn rule_from_markdown(name: &str, text: &str) -> Rule {
    let doc = frontmatter::parse(text);
    let d = &doc.data;
    let globs = ["globs", "paths", "applyTo"]
        .iter()
        .map(|k| glob_list(d.get(*k)))
        .find(|g| !g.is_empty())
        .unwrap_or_default();
    Rule {
        name: name.to_string(),
        description: string(d.get("description")),
        globs,
        always_apply: d.get("alwaysApply").and_then(Value::as_bool),
        body: doc.body.trim().to_string(),
    }
}

pub fn rule_to_markdown(r: &Rule) -> String {
    frontmatter::render(
        &[
            ("description", opt(&r.description)),
            ("globs", Value::from(r.globs.clone())),
            (
                "alwaysApply",
                r.always_apply.map_or(Value::Null, Value::Bool),
            ),
        ],
        &r.body,
    )
}

const COMMAND_KNOWN: &[&str] = &["description", "argument-hint", "argumentHint"];

pub fn command_from_markdown(name: &str, text: &str) -> Command {
    let doc = frontmatter::parse(text);
    let d = &doc.data;
    Command {
        name: name.to_string(),
        description: string(d.get("description")),
        argument_hint: string(d.get("argument-hint")).or_else(|| string(d.get("argumentHint"))),
        extra: d
            .iter()
            .filter(|(k, v)| !COMMAND_KNOWN.contains(&k.as_str()) && !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        body: doc.body.trim().to_string(),
    }
}

/// Command markdown; `with_extra` includes tool-specific frontmatter keys.
pub fn command_to_markdown(c: &Command, with_extra: bool) -> String {
    let mut entries: Vec<(&str, Value)> = vec![
        ("description", opt(&c.description)),
        ("argument-hint", opt(&c.argument_hint)),
    ];
    if with_extra {
        entries.extend(c.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    }
    frontmatter::render(&entries, &c.body)
}

pub fn skill_description(files: &[SkillFile]) -> Option<String> {
    let main = files.iter().find(|f| f.path == "SKILL.md")?;
    let text = String::from_utf8_lossy(&main.content);
    string(frontmatter::parse(&text).data.get("description"))
}

fn read_skill_dir(dir: &Path, prefix: &str, out: &mut Vec<SkillFile>) -> Result<()> {
    for (name, is_dir) in list_profile_dir(dir)? {
        if name == ".git" || name == ".DS_Store" || name == "node_modules" {
            continue;
        }
        if out.len() >= MAX_SKILL_FILES {
            anyhow::bail!("{} has more than {MAX_SKILL_FILES} files", dir.display());
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let abs = dir.join(&name);
        if is_dir {
            read_skill_dir(&abs, &rel, out)?;
        } else if let Some(content) = read_profile_file(&abs)? {
            out.push(SkillFile {
                path: rel,
                content,
                executable: crate::adapters::common::skill_file_executable(&abs),
            });
        }
    }
    Ok(())
}

/// Load a profile directory.
pub fn load(dir: &Path) -> Result<Profile> {
    let manifest = dir.join(MANIFEST);
    let text = read_profile_text(&manifest)?.ok_or_else(|| {
        ProfileError::new(
            format!(
                "no {MANIFEST} found in {} (create one with 'kitbag init')",
                dir.display()
            ),
            vec![],
        )
    })?;
    let mut p = parse_manifest(&text, &manifest)?;
    if let Some(i) = read_profile_text(&dir.join("instructions.md"))?
        && !i.trim().is_empty()
    {
        p.instructions = Some(i.trim().to_string());
    }
    let mut issues = Vec::new();
    for (file, is_dir) in list_profile_dir(&dir.join("rules"))? {
        let Some(stem) = file
            .strip_suffix(".md")
            .or_else(|| file.strip_suffix(".mdc"))
        else {
            continue;
        };
        if is_dir {
            continue;
        }
        if !is_valid_name(stem) {
            issues.push(format!("rules/{file}: invalid file name"));
            continue;
        }
        let text = read_profile_text(&dir.join("rules").join(&file))?.unwrap_or_default();
        p.rules.push(rule_from_markdown(stem, &text));
    }
    for (file, is_dir) in list_profile_dir(&dir.join("commands"))? {
        let Some(stem) = file.strip_suffix(".md") else {
            continue;
        };
        if is_dir {
            continue;
        }
        if !is_valid_name(stem) {
            issues.push(format!("commands/{file}: invalid file name"));
            continue;
        }
        let text = read_profile_text(&dir.join("commands").join(&file))?.unwrap_or_default();
        p.commands.push(command_from_markdown(stem, &text));
    }
    for (name, is_dir) in list_profile_dir(&dir.join("skills"))? {
        if !is_dir {
            continue;
        }
        if !is_valid_name(&name) {
            issues.push(format!("skills/{name}: invalid directory name"));
            continue;
        }
        let mut files = Vec::new();
        read_skill_dir(&dir.join("skills").join(&name), "", &mut files)?;
        if !files.iter().any(|f| f.path == "SKILL.md") {
            issues.push(format!("skills/{name}: missing SKILL.md"));
            continue;
        }
        p.skills.push(Skill {
            description: skill_description(&files),
            name,
            files,
        });
    }
    if !issues.is_empty() {
        return Err(ProfileError::new(
            format!("{} is not a valid kitbag profile", dir.display()),
            issues,
        )
        .into());
    }
    p.sort();
    Ok(p)
}

/// Render the manifest for `p`.
pub fn manifest_toml(p: &Profile) -> String {
    let mut doc = DocumentMut::new();
    doc.decor_mut().set_prefix(
        "# kitbag profile: https://github.com/cargopete/kitbag\n\
         # Secrets are never stored here. Use ${ENV_VAR} references; they are\n\
         # resolved from your environment (or by the tool itself) when you apply.\n",
    );
    doc["kitbag"] = value(i64::from(FORMAT_VERSION));
    doc["name"] = value(p.name.as_str());
    if let Some(d) = &p.description {
        doc["description"] = value(d.as_str());
    }
    if !p.mcp_servers.is_empty() {
        let mut servers = Table::new();
        servers.set_implicit(true);
        for (name, s) in &p.mcp_servers {
            let v = serde_json::to_value(s).expect("serializable");
            servers.insert(
                name,
                Item::Table(json_to_table(v.as_object().expect("object"))),
            );
        }
        doc["mcp_servers"] = Item::Table(servers);
    }
    if !p.settings.is_empty() {
        let mut settings = Table::new();
        settings.set_implicit(true);
        for (target, m) in &p.settings {
            let mut t = Table::new();
            for (k, v) in m {
                if let Some(val) = json_to_value(v) {
                    t.insert(k, Item::Value(val));
                }
            }
            settings.insert(target, Item::Table(t));
        }
        doc["settings"] = Item::Table(settings);
    }
    doc.to_string()
}

/// Every file making up the profile on disk, as relative path -> content.
pub fn profile_files(p: &Profile) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    out.insert(MANIFEST.to_string(), manifest_toml(p).into_bytes());
    if let Some(i) = &p.instructions {
        out.insert(
            "instructions.md".into(),
            format!("{}\n", i.trim()).into_bytes(),
        );
    }
    for r in &p.rules {
        out.insert(
            format!("rules/{}.md", r.name),
            rule_to_markdown(r).into_bytes(),
        );
    }
    for c in &p.commands {
        out.insert(
            format!("commands/{}.md", c.name),
            command_to_markdown(c, true).into_bytes(),
        );
    }
    for s in &p.skills {
        for f in &s.files {
            out.insert(format!("skills/{}/{}", s.name, f.path), f.content.clone());
        }
    }
    out
}

/// Write a profile directory. Refuses to replace an existing profile unless `force`,
/// and refuses outright if anything still looks like a credential.
pub fn save(p: &Profile, dir: &Path, force: bool) -> Result<Vec<PathBuf>> {
    let manifest = dir.join(MANIFEST);
    let leaks = scan_for_secrets(p);
    if !leaks.is_empty() {
        return Err(ProfileError::new(
            "refusing to write a profile that appears to contain secrets",
            leaks,
        )
        .into());
    }
    let new_files = profile_files(p);
    let oversize: Vec<String> = new_files
        .iter()
        .filter(|(_, c)| c.len() as u64 > MAX_FILE_BYTES)
        .map(|(rel, c)| format!("{rel}: {} KiB", c.len() / 1024))
        .collect();
    if !oversize.is_empty() {
        return Err(ProfileError::new(
            format!(
                "refusing to write a profile that couldn't be loaded: files are limited to {}",
                max_file_size()
            ),
            oversize,
        )
        .into());
    }
    if manifest.exists() {
        if !force {
            return Err(ProfileError::new(
                format!(
                    "{} already contains a profile; pass --force to replace it",
                    dir.display()
                ),
                vec![],
            )
            .into());
        }
        // Remove only files that belonged to the previous profile and are no
        // longer part of it; anything else in the directory (README, .git) stays.
        let old = load(dir).with_context(|| {
            format!("the existing profile in {} can't be read, so it won't be replaced; fix or remove it first", dir.display())
        })?;
        // `profile_files` names every rule `<name>.md`, but a profile may hold it
        // as `<name>.mdc`: look for that twin too, so it is neither missed when
        // removing nor left behind next to a rewritten `.md`.
        let mut stale: Vec<String> = Vec::new();
        for rel in profile_files(&old).keys() {
            let is_rule = rel.starts_with("rules/") && rel.ends_with(".md");
            if !new_files.contains_key(rel) {
                stale.push(rel.clone());
            }
            if is_rule {
                stale.push(format!("{rel}c"));
            }
        }
        for rel in &stale {
            let abs = crate::plan_fs::join_rel(dir, rel);
            if abs.symlink_metadata().is_err() {
                continue;
            }
            fs::remove_file(&abs).with_context(|| format!("removing {}", abs.display()))?;
            // Tidy now-empty directories up to the profile root.
            let mut d = abs.parent().map(Path::to_path_buf);
            while let Some(cur) = d {
                if cur == dir || fs::remove_dir(&cur).is_err() {
                    break;
                }
                d = cur.parent().map(Path::to_path_buf);
            }
        }
    } else if fs::read_dir(dir).is_ok_and(|rd| {
        // A fresh repository (.git, README, LICENSE) is fine to capture into.
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            !(n.starts_with('.') || n.starts_with("readme") || n.starts_with("license"))
        })
    }) {
        return Err(ProfileError::new(
            format!(
                "{} is not empty and is not a kitbag profile; choose a new or empty directory",
                dir.display()
            ),
            vec![],
        )
        .into());
    }
    // Skill scripts that were executable stay so (Unix).
    #[cfg(unix)]
    let exec: std::collections::BTreeSet<String> = p
        .skills
        .iter()
        .flat_map(|s| {
            s.files
                .iter()
                .filter(|f| f.executable)
                .map(|f| format!("skills/{}/{}", s.name, f.path))
        })
        .collect();
    let mut written = Vec::new();
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (rel, content) in new_files {
        // Links in the profile are replaced, never written through.
        crate::plan_fs::write_owned_file(dir, &rel, &content)?;
        #[cfg(unix)]
        if exec.contains(&rel) {
            use std::os::unix::fs::PermissionsExt;
            let path = crate::plan_fs::join_rel(dir, &rel);
            let mode = fs::metadata(&path)?.permissions().mode();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o111))?;
        }
        written.push(crate::plan_fs::join_rel(dir, &rel));
    }
    Ok(written)
}

/// Last line of defence: anything in the profile that still looks like a credential.
pub fn scan_for_secrets(p: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    let mut out_extra = Vec::new();
    let mut check = |where_: String, v: &str, free_text: bool| {
        let kind = if free_text {
            text_secret_kind(v)
        } else {
            strong_secret_kind(v)
        };
        if let Some(k) = kind {
            out.push(format!("{where_}: looks like a {k}"));
        }
    };
    for (n, s) in &p.mcp_servers {
        for (loc, v) in s.strings() {
            check(format!("mcp_servers.{n}.{loc}"), v, false);
        }
        for (loc, v) in s.strings() {
            for (var, _) in crate::secrets::secret_fallbacks(v) {
                out_extra.push(format!(
                    "mcp_servers.{n}.{loc}: the ${{{var}:-…}} fallback looks like a secret"
                ));
            }
        }
        // Anything capture-time redaction would still replace is a secret too.
        // (Secret fallbacks are reported above, with a clearer message.)
        for r in redact_server(n, s).1 {
            if r.reason != "secret-fallback" {
                out_extra.push(format!("{}: looks like a {}", r.location, r.reason));
            }
        }
    }
    for (t, m) in &p.settings {
        let all = Value::Object(m.clone());
        check(format!("settings.{t}"), &all.to_string(), false);
        for s in crate::secrets::json_strings(&all) {
            check(format!("settings.{t}"), s, true);
        }
    }
    if let Some(i) = &p.instructions {
        check("instructions.md".into(), i, true);
    }
    for r in &p.rules {
        let at = format!("rules/{}.md", r.name);
        check(at.clone(), &r.body, true);
        if let Some(d) = &r.description {
            check(at, d, true);
        }
    }
    for c in &p.commands {
        let at = format!("commands/{}.md", c.name);
        check(at.clone(), &c.body, true);
        for v in [&c.description, &c.argument_hint].into_iter().flatten() {
            check(at.clone(), v, true);
        }
        for v in c.extra.values() {
            for s in crate::secrets::json_strings(v) {
                check(at.clone(), s, true);
            }
        }
    }
    for s in &p.skills {
        for f in &s.files {
            if let Ok(t) = std::str::from_utf8(&f.content) {
                check(format!("skills/{}/{}", s.name, f.path), t, true);
            }
            if let Some(why) = sensitive_file(&f.path, &f.content) {
                out_extra.push(format!("skills/{}/{}: {why}", s.name, f.path));
            }
        }
    }
    out.extend(out_extra);
    out.dedup();
    out
}

/// Every env var the profile's MCP servers reference, with where it is used.
pub fn required_env(p: &Profile) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (n, s) in &p.mcp_servers {
        for (loc, v) in s.strings() {
            for name in refs_in(v) {
                out.entry(name)
                    .or_default()
                    .push(format!("mcp_servers.{n}.{loc}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        let mut p = Profile::new("demo");
        p.description = Some("Demo profile".into());
        p.mcp_servers.insert(
            "github".into(),
            McpServer {
                command: Some("npx".into()),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        );
        p.settings.insert(
            "claude-code".into(),
            serde_json::json!({"model": "opus", "permissions": {"allow": ["Bash(npm test:*)"]}})
                .as_object()
                .cloned()
                .unwrap(),
        );
        p.instructions = Some("Be concise.".into());
        p.rules.push(Rule {
            name: "ts".into(),
            globs: vec!["src/**/*.ts".into()],
            body: "Strict.".into(),
            ..Default::default()
        });
        p.commands.push(Command {
            name: "review".into(),
            description: Some("Review".into()),
            argument_hint: Some("[focus]".into()),
            extra: serde_json::json!({"allowed-tools": "Bash(git diff:*)"})
                .as_object()
                .cloned()
                .unwrap(),
            body: "Review $ARGUMENTS".into(),
        });
        p.skills.push(Skill {
            name: "pdf".into(),
            description: Some("PDFs".into()),
            files: vec![
                SkillFile {
                    path: "SKILL.md".into(),
                    content: b"---\nname: pdf\ndescription: PDFs\n---\nUse it.\n".to_vec(),
                    executable: false,
                },
                SkillFile {
                    path: "bin/blob.bin".into(),
                    content: vec![0, 159, 146, 150],
                    executable: false,
                },
            ],
        });
        p
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = sample();
        save(&p, dir.path(), false).unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, p);
        // Refuses to clobber without force.
        assert!(save(&p, dir.path(), false).is_err());
        save(&p, dir.path(), true).unwrap();
    }

    #[test]
    fn force_only_replaces_profile_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = sample();
        save(&p, dir.path(), false).unwrap();
        fs::write(dir.path().join("README.md"), "mine").unwrap();
        fs::write(dir.path().join("commands/notes.txt"), "mine too").unwrap();
        p.commands.clear();
        p.rules.clear();
        save(&p, dir.path(), true).unwrap();
        assert!(!dir.path().join("commands/review.md").exists());
        assert!(
            !dir.path().join("rules").exists(),
            "emptied directories are removed"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "mine"
        );
        assert!(dir.path().join("commands/notes.txt").exists());
        assert_eq!(load(dir.path()).unwrap(), p);
    }

    #[test]
    fn captures_into_a_fresh_repository() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("README.md"), "# dotagents").unwrap();
        save(&sample(), dir.path(), false).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "# dotagents"
        );
    }

    #[test]
    fn refuses_non_empty_non_profile_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::write(dir.path().join("commands/deploy.sh"), "echo").unwrap();
        for force in [false, true] {
            let e = save(&sample(), dir.path(), force).unwrap_err();
            assert!(e.to_string().contains("not a kitbag profile"), "{e}");
        }
        assert!(dir.path().join("commands/deploy.sh").exists());
    }

    #[test]
    fn manifest_is_readable_toml() {
        let text = manifest_toml(&sample());
        assert!(text.contains("[mcp_servers.github]\ncommand = \"npx\""));
        assert!(text.contains("[settings.claude-code]\nmodel = \"opus\""));
    }

    #[test]
    fn validation_reports_every_issue() {
        let text = "kitbag = 1\nname = \"x\"\n[mcp_servers.bad]\nargs = [\"x\"]\n[mcp_servers.remote]\nurl = \"https://x\"\ncommand = \"y\"\ntransport = \"http\"\n";
        let e = parse_manifest(text, Path::new("kitbag.toml")).unwrap_err();
        assert_eq!(e.issues.len(), 2, "{e}");
        let e =
            parse_manifest("kitbag = 1\nname = \"x\"\nsurprise = 1\n", Path::new("k")).unwrap_err();
        assert!(e.to_string().contains("surprise"));
        let e = parse_manifest("kitbag = 9\nname = \"x\"\n", Path::new("k")).unwrap_err();
        assert!(e.to_string().contains("upgrade kitbag"));
    }

    #[test]
    fn refuses_to_save_secrets() {
        let mut p = Profile::new("x");
        p.instructions = Some("token ghp_FAKE0123456789abcdefghijklmnopqrstuv".into());
        let dir = tempfile::tempdir().unwrap();
        let e = save(&p, dir.path(), false).unwrap_err();
        assert!(
            e.to_string()
                .contains("instructions.md: looks like a github-token")
        );
        assert!(!dir.path().join(MANIFEST).exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(MANIFEST), "kitbag = 1\nname = \"x\"\n").unwrap();
        fs::create_dir_all(dir.path().join("skills/evil")).unwrap();
        fs::write(
            dir.path().join("skills/evil/SKILL.md"),
            "---\nname: evil\n---\n",
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("id_rsa"), "PRIVATE").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("id_rsa"),
            dir.path().join("skills/evil/key"),
        )
        .unwrap();
        let e = load(dir.path()).unwrap_err();
        assert!(
            e.to_string().contains("symbolic links are not allowed"),
            "{e}"
        );
    }

    #[test]
    fn lists_required_env() {
        let req = required_env(&sample());
        assert_eq!(
            req["GITHUB_TOKEN"],
            vec!["mcp_servers.github.env.GITHUB_TOKEN"]
        );
    }
}
