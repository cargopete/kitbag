//! Round trips: capture from tool A -> profile on disk -> apply to tool B ->
//! capture B again, and check nothing portable was lost.

mod common;

use common::*;
use kitbag::adapters::Scope;
use kitbag::profile_io;

const ALL: &[&str] = &[
    "claude-code",
    "cursor",
    "codex",
    "gemini-cli",
    "claude-desktop",
    "antigravity",
    "opencode",
    "copilot",
    "grok-build",
    "windsurf",
    "zed",
    "hermes",
    "amp",
    "cline",
    "roo",
    "continue",
];

/// Capture `from` in a fixture sandbox, save + reload the profile, apply it to `to`
/// in a fresh sandbox, then capture `to` back.
fn round_trip(
    from: &str,
    to: &str,
    scope: Scope,
) -> (kitbag::model::Profile, kitbag::model::Profile) {
    let src = Sandbox::fixtures();
    let captured = capture(&[from], &src.env(scope)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &[to], &dst.env(scope));
    let back = capture(&[to], &dst.env(scope)).profile;
    (loaded, back)
}

#[test]
fn claude_code_to_codex() {
    let (a, b) = round_trip("claude-code", "codex", Scope::User);
    for (name, s) in &a.mcp_servers {
        assert_eq!(&b.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(b.instructions, a.instructions);
    // Rules fold into AGENTS.md and come back out intact.
    assert_eq!(b.rules, a.rules);
    // Commands become custom prompts (tool-specific frontmatter like allowed-tools is dropped).
    assert_eq!(b.commands[0].body, a.commands[0].body);
    assert_eq!(b.commands[0].description, a.commands[0].description);
    assert_eq!(b.skills.len(), 1);
    assert_eq!(
        b.settings.get("codex"),
        None,
        "claude settings don't become codex settings"
    );
}

#[test]
fn codex_to_claude_code() {
    let (a, b) = round_trip("codex", "claude-code", Scope::User);
    for (name, s) in &a.mcp_servers {
        if s.disabled {
            assert!(
                !b.mcp_servers.contains_key(name),
                "disabled servers are not written to Claude Code"
            );
            continue;
        }
        assert_eq!(&b.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(b.instructions, a.instructions);
    assert_eq!(b.commands, a.commands);
}

#[test]
fn cursor_project_to_claude_code_project() {
    let (a, b) = round_trip("cursor", "claude-code", Scope::Project);
    assert_eq!(a.rules.len(), 2);
    for r in &a.rules {
        let got = b.rules.iter().find(|x| x.name == r.name).unwrap();
        assert_eq!(got.globs, r.globs);
        assert_eq!(got.body, r.body);
    }
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn claude_code_project_to_cursor_project() {
    let (a, b) = round_trip("claude-code", "cursor", Scope::Project);
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.instructions, a.instructions);
    assert_eq!(b.commands[0].body, a.commands[0].body);
}

#[test]
fn gemini_to_cursor_user() {
    let (a, b) = round_trip("gemini-cli", "cursor", Scope::User);
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.commands[0].body, a.commands[0].body);
}

#[test]
fn antigravity_to_claude_code_user() {
    let (a, b) = round_trip("antigravity", "claude-code", Scope::User);
    for (name, s) in &a.mcp_servers {
        if s.disabled {
            assert!(!b.mcp_servers.contains_key(name), "disabled: {name}");
            continue;
        }
        assert_eq!(&b.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn claude_code_to_antigravity_user() {
    let (a, b) = round_trip("claude-code", "antigravity", Scope::User);
    // Unset env vars are written as ${VAR} placeholders and read back as such.
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.instructions, a.instructions);
    assert!(!a.rules.is_empty() && !a.skills.is_empty());
    assert_eq!(b.rules, a.rules);
    assert_eq!(b.skills.len(), a.skills.len());
    for (x, y) in a.skills.iter().zip(&b.skills) {
        assert_eq!(x.name, y.name);
        assert_eq!(x.files.len(), y.files.len());
    }
}

#[test]
fn antigravity_project_to_cursor_project() {
    let (a, b) = round_trip("antigravity", "cursor", Scope::Project);
    let mut want = a.mcp_servers.clone();
    // Cursor has no cwd.
    want.get_mut("chrome-devtools").unwrap().cwd = None;
    assert_eq!(b.mcp_servers, want);
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn opencode_to_claude_code_user() {
    let (a, b) = round_trip("opencode", "claude-code", Scope::User);
    assert!(a.mcp_servers.contains_key("context7"));
    for (name, s) in &a.mcp_servers {
        if s.disabled {
            assert!(!b.mcp_servers.contains_key(name), "disabled: {name}");
            continue;
        }
        assert_eq!(&b.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn claude_code_to_opencode_user() {
    let (a, b) = round_trip("claude-code", "opencode", Scope::User);
    // ${VAR} references go out as {env:VAR} and come back unchanged.
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.instructions, a.instructions);
    // Skills carry over whole; rules come back out of the folded AGENTS.md;
    // commands keep their prompt (opencode has no `allowed-tools` or `argument-hint`).
    assert!(!a.skills.is_empty() && !a.rules.is_empty() && !a.commands.is_empty());
    assert_eq!(b.skills, a.skills);
    assert_eq!(b.rules, a.rules);
    let names = |p: &kitbag::model::Profile| -> Vec<String> {
        p.commands.iter().map(|c| c.name.clone()).collect()
    };
    assert_eq!(names(&b), names(&a));
    for (x, y) in a.commands.iter().zip(&b.commands) {
        assert_eq!((&x.description, &x.body), (&y.description, &y.body));
    }
}

#[test]
fn opencode_project_to_cursor_project() {
    let (a, b) = round_trip("opencode", "cursor", Scope::Project);
    let mut want = a.mcp_servers.clone();
    // Cursor has no cwd.
    want.get_mut("playwright").unwrap().cwd = None;
    assert_eq!(b.mcp_servers, want);
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn cursor_project_to_opencode_project() {
    let (a, b) = round_trip("cursor", "opencode", Scope::Project);
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.instructions, a.instructions);
}

#[test]
fn every_pair_is_stable_on_reapply() {
    // Apply twice: the second plan must be empty (idempotent) for every target.
    let src = Sandbox::fixtures();
    let profile = capture(ALL, &src.env(Scope::User)).profile;
    for to in ALL {
        let dst = Sandbox::empty();
        let env = dst.env_with(
            Scope::User,
            &[("GITHUB_PERSONAL_ACCESS_TOKEN", "x-test-token")],
        );
        apply(&dst, &profile, &[to], &env);
        let again = plan(&profile, &[to], &env);
        assert!(
            again.changes.is_empty(),
            "{to}: second apply should be a no-op, got {:?}",
            again.changes.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
    }
}

#[test]
fn applying_everywhere_then_capturing_everything_is_a_fixed_point() {
    let src = Sandbox::fixtures();
    let profile = capture(ALL, &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    let env = dst.env(Scope::User);
    apply(&dst, &profile, ALL, &env);
    let back = capture(ALL, &env).profile;
    assert_eq!(back.mcp_servers, profile.mcp_servers);
    assert_eq!(back.instructions, profile.instructions);
    assert_eq!(back.rules, profile.rules);
    // Amp has no commands (it dropped them for skills), so the commands applied
    // to it come back from its skills folder as skills too.
    let mut want: Vec<&String> = profile
        .skills
        .iter()
        .map(|s| &s.name)
        .chain(profile.commands.iter().map(|c| &c.name))
        .collect();
    want.sort();
    want.dedup();
    let mut got: Vec<&String> = back.skills.iter().map(|s| &s.name).collect();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn copilot_project_to_copilot_project() {
    let (a, b) = round_trip("copilot", "copilot", Scope::Project);
    // Prompt files keep their VS Code keys (agent, tools).
    assert_eq!(b.commands, a.commands);
    assert_eq!(b.skills, a.skills);
    assert_eq!(b.rules, a.rules);
}

#[test]
fn claude_code_project_to_copilot_project() {
    let (a, b) = round_trip("claude-code", "copilot", Scope::Project);
    assert_eq!(b.commands[0].name, a.commands[0].name);
    assert_eq!(b.commands[0].body, a.commands[0].body);
    assert_eq!(b.commands[0].description, a.commands[0].description);
}

#[test]
fn claude_code_to_copilot_user_skills() {
    let (a, b) = round_trip("claude-code", "copilot", Scope::User);
    assert!(!a.skills.is_empty());
    assert_eq!(
        b.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
        a.skills.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert!(b.commands.is_empty(), "no user prompt files");
}

#[test]
fn claude_code_to_grok_build() {
    let (a, b) = round_trip("claude-code", "grok-build", Scope::User);
    for (name, s) in &a.mcp_servers {
        assert_eq!(&b.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(b.instructions, a.instructions);
    // Rules get files of their own and come back with their globs.
    assert_eq!(b.rules, a.rules);
    // Commands keep their frontmatter (allowed-tools included).
    assert_eq!(b.commands, a.commands);
    assert_eq!(b.skills, a.skills);
}

#[test]
fn grok_build_to_grok_build_project() {
    let (a, b) = round_trip("grok-build", "grok-build", Scope::Project);
    assert_eq!(b.mcp_servers, a.mcp_servers);
    assert_eq!(b.rules, a.rules);
    assert_eq!(b.skills, a.skills);
}
