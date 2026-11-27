//! Per-tool targeting of MCP servers (`targets` / `disabled_on`): servers are
//! only applied where the profile enables them, and a server kitbag applied
//! to a tool earlier is removed from it, or turned off with the tool's own flag
//! where it has one, once the profile disables it there.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const CURSOR: &str = "home/.cursor/mcp.json";
const CLAUDE: &str = "home/.claude.json";
const CODEX: &str = "home/.codex/config.toml";

fn profile(name: &str, heavy_extra: &str) -> String {
    format!(
        "kitbag = 1\nname = \"{name}\"\n\n[mcp_servers.heavy]\ncommand = \"heavy-mcp\"\nargs = [\"--big\"]\n{heavy_extra}\n[mcp_servers.light]\ncommand = \"light-mcp\"\n"
    )
}

fn write_profile(sb: &Sandbox, dir: &str, text: &str) {
    fs::create_dir_all(sb.path(dir)).unwrap();
    fs::write(sb.path(&format!("{dir}/kitbag.toml")), text).unwrap();
}

fn apply(sb: &Sandbox, src: &str, to: &str) -> String {
    let out = sb
        .cli()
        .args(["apply", src, "--to", to, "--yes"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn cursor_has(sb: &Sandbox, name: &str) -> bool {
    sb.json(CURSOR)["mcpServers"].get(name).is_some()
}

#[test]
fn servers_are_only_applied_where_enabled() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "p",
        &profile("shared", "targets = [\"claude-code\"]\n"),
    );
    sb.cli()
        .args(["diff", "p", "--to", "claude-code,cursor"])
        .assert()
        .success()
        .stdout(contains(
            "MCP server 'heavy' skipped on cursor: disabled in profile (not in its 'targets')",
        ));
    let out = apply(&sb, "p", "claude-code,cursor");
    assert!(
        out.contains("skipped on cursor: disabled in profile"),
        "{out}"
    );
    let claude = sb.json(CLAUDE);
    assert!(claude["mcpServers"].get("heavy").is_some());
    assert!(claude["mcpServers"].get("light").is_some());
    assert!(!cursor_has(&sb, "heavy"));
    assert!(cursor_has(&sb, "light"));

    // The deny-list form.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"codex\"]\n"));
    let out = apply(&sb, "p", "cursor,codex");
    assert!(
        out.contains(
            "MCP server 'heavy' skipped on codex: disabled in profile (listed in 'disabled_on')"
        ),
        "{out}"
    );
    assert!(cursor_has(&sb, "heavy"));
    let codex = sb.read(CODEX);
    assert!(!codex.contains("heavy"), "{codex}");
    assert!(codex.contains("[mcp_servers.light]"), "{codex}");
}

#[test]
fn bad_targeting_is_an_error() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "p",
        &profile(
            "shared",
            "targets = [\"cursor\"]\ndisabled_on = [\"codex\"]\n",
        ),
    );
    sb.cli()
        .args(["diff", "p", "--to", "cursor"])
        .assert()
        .failure()
        .stderr(contains("set either 'targets'").and(contains("not both")));

    // The older name and the new one for the same field.
    write_profile(
        &sb,
        "p",
        &profile("shared", "only = [\"cursor\"]\ntargets = [\"cursor\"]\n"),
    );
    sb.cli()
        .args(["diff", "p", "--to", "cursor"])
        .assert()
        .failure()
        .stderr(contains(
            "mcp_servers.heavy: both 'only' and 'targets' are set; 'only' is the older name of 'targets', keep one",
        ));
    write_profile(
        &sb,
        "p",
        &profile(
            "shared",
            "except = [\"cursor\"]\ndisabled_on = [\"codex\"]\n",
        ),
    );
    sb.cli()
        .args(["diff", "p", "--to", "cursor"])
        .assert()
        .failure()
        .stderr(contains("both 'except' and 'disabled_on' are set"));

    write_profile(&sb, "p", &profile("shared", "targets = []\n"));
    sb.cli()
        .args(["diff", "p", "--to", "cursor"])
        .assert()
        .failure()
        .stderr(contains("'targets' is empty"));
}

#[test]
fn older_and_newer_profiles_still_load() {
    // A target this kitbag doesn't know (from a newer one, say) is a warning
    // and matches nothing; the rest of the profile applies.
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "p",
        &profile("shared", "targets = [\"vscode\", \"cursor\"]\n"),
    );
    let out = sb
        .cli()
        .args(["apply", "p", "--to", "cursor,codex", "--yes"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "warning: mcp_servers.heavy: unknown target 'vscode' in 'targets' is ignored"
        ) && stderr.contains("claude-desktop, antigravity, opencode"),
        "{stderr}"
    );
    assert!(cursor_has(&sb, "heavy"));
    assert!(!sb.read(CODEX).contains("heavy"));
    sb.cli().args(["doctor", "p"]).assert().stdout(contains(
        "warn mcp_servers.heavy: unknown target 'vscode' in 'targets' is ignored",
    ));

    // An empty 'only' meant no restriction before 'targets', and still does.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", "only = []\n"));
    sb.cli()
        .args(["apply", "p", "--to", "cursor", "--yes"])
        .assert()
        .success()
        .stderr(contains(
            "warning: mcp_servers.heavy: 'only' is empty, which means no restriction",
        ));
    assert!(cursor_has(&sb, "heavy"));
}

#[test]
fn disabling_removes_or_turns_off_what_kitbag_applied_and_undo_restores_it() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "claude-code,cursor,codex");
    assert!(cursor_has(&sb, "heavy"));
    assert!(sb.read(CODEX).contains("[mcp_servers.heavy]"));
    let codex_before = sb.read(CODEX);
    let cursor_before = sb.read(CURSOR);

    write_profile(
        &sb,
        "p",
        &profile("shared", "disabled_on = [\"cursor\", \"codex\"]\n"),
    );
    // Cursor has no switch for a server, so its entry goes; Codex has
    // `enabled = false`, so its entry stays.
    sb.cli()
        .args(["diff", "p", "--to", "claude-code,cursor,codex"])
        .assert()
        .success()
        .stdout(
            contains("MCP servers removed (disabled for these tools in the profile")
                .and(contains("heavy  (cursor)"))
                .and(contains("MCP servers turned off (disabled for these tools"))
                .and(contains("heavy  (codex)")),
        );
    sb.cli()
        .args(["status", "p", "--to", "cursor"])
        .assert()
        .success()
        .stdout(contains("mcp heavy").and(contains("apply removes it")));
    sb.cli()
        .args(["status", "p", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("mcp heavy").and(contains("apply turns it off")));

    let out = apply(&sb, "p", "claude-code,cursor,codex");
    assert!(out.contains("MCP server 'heavy' removed from"), "{out}");
    assert!(out.contains("MCP server 'heavy' turned off in"), "{out}");
    assert!(!cursor_has(&sb, "heavy"));
    assert!(cursor_has(&sb, "light"));
    let codex = sb.read(CODEX);
    let table = codex.split("[mcp_servers.heavy]").nth(1).unwrap();
    let table = table.split("\n[").next().unwrap();
    assert!(table.contains("enabled = false"), "{codex}");
    assert!(codex.contains("[mcp_servers.light]"), "{codex}");
    assert!(sb.json(CLAUDE)["mcpServers"].get("heavy").is_some());

    // Applying again has nothing left to do, and status agrees.
    let out = apply(&sb, "p", "claude-code,cursor,codex");
    assert!(out.contains("Up to date"), "{out}");
    assert!(!out.contains("left in place"), "{out}");
    sb.cli()
        .args(["status", "p", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("apply turns it off").not());

    // Undo puts everything back, byte for byte.
    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read(CURSOR), cursor_before);
    assert_eq!(sb.read(CODEX), codex_before);

    // After the undo the earlier record stands again, so a re-apply does it again.
    apply(&sb, "p", "claude-code,cursor,codex");
    assert!(!cursor_has(&sb, "heavy"));
    assert!(sb.read(CODEX).contains("enabled = false"));

    // Enabling it again is an ordinary addition (Cursor) and clears the flag (Codex).
    write_profile(&sb, "p", &profile("shared", ""));
    let out = apply(&sb, "p", "cursor,codex");
    assert!(out.contains("MCP servers added or changed"), "{out}");
    assert!(cursor_has(&sb, "heavy"));
    assert!(!sb.read(CODEX).contains("enabled"), "{}", sb.read(CODEX));
    assert_eq!(sb.read(CODEX), codex_before);
}

#[test]
fn hand_added_servers_are_never_removed() {
    let sb = Sandbox::empty();
    let mine = "{\n  \"mcpServers\": {\n    \"heavy\": { \"command\": \"heavy-mcp\", \"args\": [\"--big\"] }\n  }\n}\n";
    fs::create_dir_all(sb.path("home/.cursor")).unwrap();
    fs::write(sb.path(CURSOR), mine).unwrap();
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"cursor\"]\n"));
    let out = apply(&sb, "p", "cursor");
    assert!(
        out.contains("kitbag has no record of adding it to"),
        "{out}"
    );
    assert!(cursor_has(&sb, "heavy"));
    assert!(cursor_has(&sb, "light"));
    sb.cli()
        .args(["status", "p", "--to", "cursor"])
        .assert()
        .success()
        .stdout(contains("apply leaves it"));
}

#[test]
fn edited_servers_and_other_profiles_servers_are_left_alone() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor,codex");

    // Edited in Cursor since kitbag wrote it.
    let edited = sb
        .read(CURSOR)
        .replace("\"--big\"", "\"--big\", \"--mine\"");
    fs::write(sb.path(CURSOR), &edited).unwrap();
    write_profile(
        &sb,
        "p",
        &profile("shared", "disabled_on = [\"cursor\", \"codex\"]\n"),
    );
    let out = apply(&sb, "p", "cursor,codex");
    assert!(
        out.contains("it was edited in") && out.contains("since kitbag added it"),
        "{out}"
    );
    assert!(cursor_has(&sb, "heavy"));
    assert!(
        sb.read(CODEX).contains("enabled = false"),
        "codex was unchanged, so it is turned off"
    );

    // A different profile that disables it doesn't remove what "shared" applied.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor");
    write_profile(&sb, "q", &profile("other", "disabled_on = [\"cursor\"]\n"));
    let out = apply(&sb, "q", "cursor");
    assert!(out.contains("profile 'shared' added it"), "{out}");
    assert!(cursor_has(&sb, "heavy"));
}

#[test]
fn no_overwrite_keeps_disabled_servers() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor");
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"cursor\"]\n"));
    sb.cli()
        .args(["apply", "p", "--to", "cursor", "--yes", "--no-overwrite"])
        .assert()
        .success()
        .stdout(contains("because --no-overwrite was given"));
    assert!(cursor_has(&sb, "heavy"));
}

#[test]
fn a_remote_profile_needs_the_opt_in_to_remove_or_re_enable_a_server() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    // Each version is a new commit in the same repository.
    let v1 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let out = apply_remote(&sb, &v1, &["--allow-git-mcp", "heavy,light"]);
    assert!(out.contains("Wrote"), "{out}");
    assert!(cursor_has(&sb, "heavy"));
    let rec = sb.read("state/applied-servers.json");
    assert!(rec.contains("\"source\""), "{rec}");

    // The repository now disables it for Cursor: removing it is held back
    // until --allow-git-mcp names it, like an addition.
    let v2 = sb.git_profile(
        "repo",
        &[(
            "kitbag.toml",
            &profile("shared", "disabled_on = [\"cursor\"]\n"),
        )],
    );
    let out = apply_remote(&sb, &v2, &[]);
    assert!(
        out.contains("MCP servers not removed (disabled for these tools in the profile, which is from a git source)")
            && out.contains("heavy  (cursor)")
            && out.contains("--allow-git-mcp heavy"),
        "{out}"
    );
    assert!(!out.contains("Up to date"), "{out}");
    assert!(cursor_has(&sb, "heavy"));
    let out = apply_remote(&sb, &v2, &["--allow-git-mcp", "heavy"]);
    assert!(out.contains("MCP server 'heavy' removed from"), "{out}");
    assert!(!cursor_has(&sb, "heavy"));

    // Enabling it again is an addition, held back like any other.
    let v3 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let out = apply_remote(&sb, &v3, &[]);
    assert!(
        out.contains("MCP servers not applied (from a git source)") && out.contains("heavy"),
        "{out}"
    );
    assert!(!cursor_has(&sb, "heavy"));
    let out = apply_remote(&sb, &v3, &["--allow-git-mcp", "heavy"]);
    assert!(
        out.contains("Review the MCP servers it adds or changes"),
        "{out}"
    );
    assert!(cursor_has(&sb, "heavy"));
}

#[test]
fn only_the_same_source_removes_a_server() {
    // A git profile with the same name as a local one can't remove the
    // servers the local one applied, even with the opt-in.
    if has_git() {
        let sb = Sandbox::empty();
        write_profile(&sb, "p", &profile("shared", ""));
        apply(&sb, "p", "cursor");
        let url = sb.git_profile(
            "r",
            &[(
                "kitbag.toml",
                &profile("shared", "disabled_on = [\"cursor\"]\n"),
            )],
        );
        let out = apply_remote(&sb, &url, &["--trust-source"]);
        assert!(
            out.contains("but a profile 'shared' from") && out.contains("left in place"),
            "{out}"
        );
        assert!(cursor_has(&sb, "heavy"));
    }

    // Nor can another local directory whose profile has the same name.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor");
    write_profile(&sb, "q", &profile("shared", "disabled_on = [\"cursor\"]\n"));
    let out = apply(&sb, "q", "cursor");
    assert!(out.contains("but a profile 'shared' from"), "{out}");
    assert!(cursor_has(&sb, "heavy"));

    // Records from before kitbag recorded the source still match by name
    // for a local profile.
    let rec = sb.path("state/applied-servers.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rec).unwrap()).unwrap();
    for r in v.as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("source");
    }
    fs::write(&rec, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let out = apply(&sb, "q", "cursor");
    assert!(out.contains("MCP server 'heavy' removed from ~"), "{out}");
    assert!(!cursor_has(&sb, "heavy"));
}

#[test]
fn unreadable_records_are_a_warning() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor");
    fs::write(sb.path("state/applied-servers.json"), "not json").unwrap();
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"cursor\"]\n"));
    let out = apply(&sb, "p", "cursor");
    assert!(
        out.contains("applied-servers.json (") && out.contains("so no MCP server is removed"),
        "{out}"
    );
    assert!(out.contains("kitbag has no record of adding it"), "{out}");
    assert!(cursor_has(&sb, "heavy"));
    assert!(!sb.path("state/applied-servers.json.bak").exists());

    // The next apply that writes a server starts the records afresh, and
    // keeps the unreadable file next to them.
    let changed = profile("shared", "disabled_on = [\"cursor\"]\n")
        .replace("command = \"light-mcp\"", "command = \"light-mcp-2\"");
    write_profile(&sb, "p", &changed);
    let out = apply(&sb, "p", "cursor");
    assert!(out.contains("Wrote"), "{out}");
    assert_eq!(sb.read("state/applied-servers.json.bak"), "not json");
    let rec: serde_json::Value = sb.json("state/applied-servers.json");
    assert!(
        rec.as_array()
            .is_some_and(|a| a.iter().any(|r| r["name"] == "light")),
        "{rec}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("state/applied-servers.json.bak"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn up_to_date_says_when_disabled_servers_were_left_in_place() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "cursor");
    let out = apply(&sb, "p", "cursor");
    assert!(
        out.contains("Up to date: every target already matches the profile.")
            && !out.contains("left in place"),
        "{out}"
    );

    // Another profile disables it: nothing is written, and the summary
    // doesn't claim Cursor matches the profile outright.
    write_profile(&sb, "q", &profile("other", "disabled_on = [\"cursor\"]\n"));
    let out = apply(&sb, "q", "cursor");
    assert!(
        out.contains(
            "Up to date: every target already matches the profile, except for MCP servers it disables that were left in place (see below)."
        ) && out.contains("profile 'shared' added it"),
        "{out}"
    );
    assert!(cursor_has(&sb, "heavy"));
}

#[test]
fn antigravity_servers_can_be_disabled_and_undone() {
    const AG: &str = "home/.gemini/config/mcp_config.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "antigravity,cursor");
    assert!(sb.json(AG)["mcpServers"].get("heavy").is_some());
    let before = sb.read(AG);

    write_profile(
        &sb,
        "p",
        &profile("shared", "disabled_on = [\"antigravity\"]\n"),
    );
    let out = apply(&sb, "p", "antigravity,cursor");
    assert!(
        out.contains("MCP server 'heavy' turned off in ~")
            && out.contains("disabled for antigravity in the profile"),
        "{out}"
    );
    let ag = sb.json(AG);
    assert_eq!(ag["mcpServers"]["heavy"]["disabled"], true, "{ag}");
    assert!(ag["mcpServers"].get("light").is_some(), "{ag}");
    assert!(cursor_has(&sb, "heavy"));

    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read(AG), before);

    // The allow-list form keeps it out of Antigravity from the start.
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "p",
        &profile("shared", "targets = [\"antigravity\"]\n"),
    );
    apply(&sb, "p", "antigravity,cursor");
    assert!(sb.json(AG)["mcpServers"].get("heavy").is_some());
    assert!(!cursor_has(&sb, "heavy"));
}

fn apply_remote(sb: &Sandbox, url: &str, extra: &[&str]) -> String {
    let out = sb
        .cli()
        .args(["apply", url, "--to", "cursor", "--yes"])
        .args(extra)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn recapture_keeps_targeting() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "p",
        &profile("shared", "targets = [\"claude-code\", \"cursor\"]\n"),
    );
    apply(&sb, "p", "claude-code");
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "p", "--force"])
        .assert()
        .success()
        .stdout(contains("kept 'targets' / 'disabled_on'").and(contains("heavy")));
    let text = sb.read("p/kitbag.toml");
    assert!(
        text.contains("targets = [\"claude-code\", \"cursor\"]"),
        "{text}"
    );
    // Servers without targeting are written as before.
    let light = text.split("[mcp_servers.light]").nth(1).unwrap();
    assert!(!light.contains("targets"), "{text}");
}

#[test]
fn recapture_keeps_servers_no_tool_has_unless_pruned() {
    let sb = Sandbox::empty();
    // `heavy` is kept off the only tool applied to, so capture can't find it.
    write_profile(
        &sb,
        "p",
        &profile("shared", "disabled_on = [\"claude-code\"]\n"),
    );
    apply(&sb, "p", "claude-code");
    assert!(sb.json(CLAUDE)["mcpServers"].get("heavy").is_none());

    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "p", "--force"])
        .assert()
        .success()
        .stdout(
            contains("kept MCP server(s) of the existing profile that no tool has")
                .and(contains("heavy; pass --prune")),
        );
    let text = sb.read("p/kitbag.toml");
    let heavy = text.split("[mcp_servers.heavy]").nth(1).unwrap();
    assert!(heavy.contains("command = \"heavy-mcp\""), "{text}");
    assert!(heavy.contains("disabled_on = [\"claude-code\"]"), "{text}");
    assert!(text.contains("[mcp_servers.light]"), "{text}");

    // A dry run says what --prune would drop and writes nothing.
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "-o",
            "p",
            "--force",
            "--prune",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(contains("dropped MCP server(s)").and(contains("(--prune): heavy")));
    assert_eq!(sb.read("p/kitbag.toml"), text);

    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "-o",
            "p",
            "--force",
            "--prune",
        ])
        .assert()
        .success()
        .stdout(contains("(--prune): heavy").and(contains("kept MCP server(s)").not()));
    let text = sb.read("p/kitbag.toml");
    assert!(!text.contains("heavy"), "{text}");
    assert!(text.contains("[mcp_servers.light]"), "{text}");
}

#[test]
fn recapture_keeps_servers_for_tools_not_installed() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", "targets = [\"cursor\"]\n"));
    apply(&sb, "p", "claude-code");
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "p", "--force"])
        .assert()
        .success()
        .stdout(contains("no tool has").and(contains("heavy")));
    let text = sb.read("p/kitbag.toml");
    assert!(text.contains("targets = [\"cursor\"]"), "{text}");
}

#[test]
fn sync_removes_disabled_servers_too() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "p", "claude-code,cursor");
    write_profile(
        &sb,
        "p",
        &profile("shared", "targets = [\"claude-code\"]\n"),
    );
    sb.cli()
        .args(["sync", "p", "--to", "claude-code,cursor", "--yes"])
        .assert()
        .success()
        .stdout(contains("MCP server 'heavy' removed from"));
    assert!(!cursor_has(&sb, "heavy"));
    assert!(sb.json(CLAUDE)["mcpServers"].get("heavy").is_some());
}
