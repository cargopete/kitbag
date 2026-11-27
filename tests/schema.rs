//! Per-tool config schemas (#30): capture records the schema a tool's files
//! use, apply converts between the schemas kitbag knows and refuses or skips
//! what it doesn't, and doctor reports the difference. Fixtures are in
//! `tests/fixtures/schema/`: opencode's two `mcp` layouts and Continue's
//! `config.yaml` with `schema: v1` and an unknown `schema: v2`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;

fn fixture(name: &str) -> String {
    fs::read_to_string(fixtures().join("schema").join(name)).unwrap()
}

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

const OPENCODE: &str = "home/.config/opencode/opencode.json";
const CONTINUE: &str = "home/.continue/config.yaml";

/// A profile directory `p` in the sandbox with the given manifest.
fn profile(sb: &Sandbox, manifest: &str) {
    put(sb, "p/kitbag.toml", manifest);
}

fn read_json(sb: &Sandbox, rel: &str) -> serde_json::Value {
    // The fixtures are JSONC; kitbag keeps the comments, so strip them here.
    let text = sb.read(rel);
    let body: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&body).unwrap()
}

// ---------------------------------------------------------------------------
// The manifest

#[test]
fn targets_round_trip_through_the_manifest() {
    let mut p = kitbag::model::Profile::new("x");
    p.schemas.insert("opencode".into(), "v2".into());
    let text = profile_io::manifest_toml(&p);
    assert!(
        text.contains("[targets.opencode]\nschema = \"v2\"\n"),
        "{text}"
    );
    let back = profile_io::parse_manifest(&text, std::path::Path::new("kitbag.toml")).unwrap();
    assert_eq!(back.schemas, p.schemas);
    // Without one, nothing is written and nothing changes for older readers.
    let plain = profile_io::manifest_toml(&kitbag::model::Profile::new("x"));
    assert!(!plain.contains("targets"), "{plain}");
}

#[test]
fn a_bad_targets_section_is_reported() {
    let parse = |extra: &str| {
        profile_io::parse_manifest(
            &format!("kitbag = 1\nname = \"x\"\n{extra}"),
            std::path::Path::new("kitbag.toml"),
        )
    };
    let issues = |extra: &str| parse(extra).unwrap_err().issues.join("\n");
    assert!(issues("[targets.opencode]\nschema = \"\"\n").contains("targets.opencode.schema"));
    assert!(issues("[targets.opencode]\nschema = \"v 2\"\n").contains("letters, digits"));
    // Both spellings of one tool.
    let both = "[targets.claude]\nschema = \"a\"\n[targets.claude-code]\nschema = \"b\"\n";
    assert!(issues(both).contains("second time"));
    // A key a newer kitbag may add is refused, not ignored.
    let e = parse("[targets.opencode]\nschema = \"v2\"\nversion = \"2.1\"\n").unwrap_err();
    assert!(e.issues.join("\n").contains("version"), "{e}");
    // An unknown tool loads, with a warning.
    let p = parse("[targets.vim]\nschema = \"v1\"\n").unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("targets.vim")));
    // An alias is stored under the tool's own id.
    let p = parse("[targets.claude]\nschema = \"v1\"\n").unwrap();
    assert!(p.schemas.contains_key("claude-code"));
}

// ---------------------------------------------------------------------------
// Capture

fn capture_manifest(sb: &Sandbox, from: &str) -> String {
    sb.cli()
        .args(["capture", "--from", from, "--out", "captured"])
        .assert()
        .success();
    sb.read("captured/kitbag.toml")
}

#[test]
fn capture_records_the_opencode_layout_it_read() {
    for (file, schema) in [("opencode_v1.json", "v1"), ("opencode_v2.json", "v2")] {
        let sb = Sandbox::empty();
        put(&sb, OPENCODE, &fixture(file));
        let manifest = capture_manifest(&sb, "opencode");
        assert!(
            manifest.contains(&format!("[targets.opencode]\nschema = \"{schema}\"\n")),
            "{file}: {manifest}"
        );
        // The same servers whichever layout they came from.
        let p = profile_io::load(&sb.path("captured")).unwrap();
        assert!(p.mcp_servers["tracker"].disabled, "{file}");
        assert_eq!(
            p.mcp_servers["docs"].url.as_deref(),
            Some("https://docs.example.com/mcp")
        );
        assert_eq!(p.schemas["opencode"], schema);
    }
}

#[test]
fn capture_records_continues_config_schema() {
    let sb = Sandbox::empty();
    put(&sb, CONTINUE, &fixture("continue_v1.yaml"));
    let manifest = capture_manifest(&sb, "continue");
    assert!(
        manifest.contains("[targets.continue]\nschema = \"v1\"\n"),
        "{manifest}"
    );
}

#[test]
fn capture_records_nothing_without_a_schema_to_go_by() {
    // An opencode config with no servers says nothing about its layout.
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, "{\"model\": \"x/y\"}\n");
    put(&sb, "home/.config/opencode/AGENTS.md", "Be brief.\n");
    let manifest = capture_manifest(&sb, "opencode");
    assert!(!manifest.contains("targets"), "{manifest}");
    // A tool that declares no schemas records none.
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.claude.json",
        "{\"mcpServers\":{\"a\":{\"command\":\"a\"}}}",
    );
    let manifest = capture_manifest(&sb, "claude-code");
    assert!(!manifest.contains("targets"), "{manifest}");
}

#[test]
fn a_schema_kitbag_does_not_know_is_recorded_so_apply_refuses_it_later() {
    let sb = Sandbox::empty();
    put(&sb, CONTINUE, &fixture("continue_v2.yaml"));
    let manifest = capture_manifest(&sb, "continue");
    assert!(manifest.contains("schema = \"v2\""), "{manifest}");
}

// ---------------------------------------------------------------------------
// Apply: converts between the schemas it knows

#[test]
fn a_profile_from_the_opencode_1_layout_is_written_in_the_2_layout() {
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, &fixture("opencode_v1.json"));
    capture_manifest(&sb, "opencode");
    let target = Sandbox::empty();
    copy_dir(&sb.path("captured"), &target.path("p"));
    put(
        &target,
        OPENCODE,
        &fixture("opencode_v2.json").replace("docs", "old"),
    );
    target
        .cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("captured from schema 'v1'").and(contains("written as 'v2'")));
    let doc = read_json(&target, OPENCODE);
    let servers = &doc["mcp"]["servers"];
    assert_eq!(servers["tracker"]["disabled"], true, "{doc}");
    assert!(servers["tracker"].get("enabled").is_none(), "{doc}");
    assert!(servers.get("docs").is_some() && servers.get("old").is_some());
    assert!(
        doc["mcp"].get("tracker").is_none(),
        "no v1 entry added: {doc}"
    );
}

#[test]
fn a_profile_from_the_opencode_2_layout_is_written_in_the_1_layout() {
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, &fixture("opencode_v2.json"));
    capture_manifest(&sb, "opencode");
    let target = Sandbox::empty();
    copy_dir(&sb.path("captured"), &target.path("p"));
    put(
        &target,
        OPENCODE,
        &fixture("opencode_v1.json").replace("docs", "old"),
    );
    target
        .cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("captured from schema 'v2'").and(contains("written as 'v1'")));
    let doc = read_json(&target, OPENCODE);
    assert_eq!(doc["mcp"]["tracker"]["enabled"], false, "{doc}");
    assert!(doc["mcp"].get("servers").is_none(), "{doc}");
    assert!(doc["mcp"].get("docs").is_some() && doc["mcp"].get("old").is_some());
}

#[test]
fn converting_keeps_references_and_never_writes_secret_values() {
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, &fixture("opencode_v1.json"));
    capture_manifest(&sb, "opencode");
    let target = Sandbox::empty();
    copy_dir(&sb.path("captured"), &target.path("p"));
    put(
        &target,
        OPENCODE,
        &fixture("opencode_v2.json").replace("docs", "old"),
    );
    target
        .cli()
        .env("TRACKER_TOKEN", "tok-fake-1234567890")
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .success();
    let text = target.read(OPENCODE);
    assert!(text.contains("{env:TRACKER_TOKEN}"), "{text}");
    assert!(!text.contains("tok-fake-1234567890"), "{text}");
    assert!(!target.read("p/kitbag.toml").contains("tok-fake"));
}

#[test]
fn matching_schemas_say_nothing() {
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, &fixture("opencode_v2.json"));
    capture_manifest(&sb, "opencode");
    let target = Sandbox::empty();
    copy_dir(&sb.path("captured"), &target.path("p"));
    put(
        &target,
        OPENCODE,
        &fixture("opencode_v2.json").replace("docs", "old"),
    );
    target
        .cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("captured from schema").not());
}

#[test]
fn a_profile_without_targets_applies_as_before() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\n",
    );
    put(&sb, OPENCODE, &fixture("opencode_v1.json"));
    sb.cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("captured from schema").not());
}

// ---------------------------------------------------------------------------
// Apply: refuses what it doesn't know, stops where it can't write

const CLAIMS_V3: &str = "kitbag = 1\nname = \"p\"\n[targets.opencode]\nschema = \"v3\"\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\n";

#[test]
fn a_newer_schema_is_refused_and_nothing_is_written() {
    let sb = Sandbox::empty();
    profile(&sb, CLAIMS_V3);
    put(&sb, OPENCODE, &fixture("opencode_v2.json"));
    let before = sb.read(OPENCODE);
    sb.cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .failure()
        .stderr(contains("schema 'v3'").and(contains("does not know")));
    assert_eq!(sb.read(OPENCODE), before);
    // It is refused with no config on the machine to compare to, too.
    let sb = Sandbox::empty();
    profile(&sb, CLAIMS_V3);
    sb.cli()
        .args(["apply", "p", "--to", "opencode", "--yes"])
        .assert()
        .failure();
    assert!(!sb.path(OPENCODE).exists());
}

#[test]
fn the_refusal_covers_every_target_of_the_apply() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        &format!("{CLAIMS_V3}[targets.codex]\nschema = \"x\"\n"),
    );
    // Targets the apply doesn't touch don't matter ...
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    // ... a tool that declares no schemas knows no schema to claim.
    sb.cli()
        .args(["apply", "p", "--to", "codex", "--yes"])
        .assert()
        .failure()
        .stderr(contains("schema 'x'"));
}

#[test]
fn a_git_source_claiming_a_newer_schema_is_refused_even_when_trusted() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile("repo", &[("kitbag.toml", CLAIMS_V3)]);
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "opencode",
            "--yes",
            "--trust-source",
            "--allow-git-mcp",
        ])
        .assert()
        .failure()
        .stderr(contains("schema 'v3'"));
    assert!(!sb.path(OPENCODE).exists());
}

#[test]
fn a_tool_whose_config_schema_is_unknown_is_skipped_and_left_alone() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n[targets.continue]\nschema = \"v1\"\n[mcp_servers.search]\nurl = \"https://search.example.com/mcp\"\n",
    );
    put(&sb, CONTINUE, &fixture("continue_v2.yaml"));
    let before = sb.read(CONTINUE);
    sb.cli()
        .args(["apply", "p", "--to", "continue", "--yes"])
        .assert()
        .success()
        .stdout(
            contains("nothing written to Continue")
                .and(contains("schema 'v2'"))
                .and(contains("it writes: v1")),
        );
    assert_eq!(sb.read(CONTINUE), before);
    // With the known schema it applies.
    put(&sb, CONTINUE, &fixture("continue_v1.yaml"));
    sb.cli()
        .args(["apply", "p", "--to", "continue", "--yes"])
        .assert()
        .success();
    assert!(sb.read(CONTINUE).contains("search"));
}

#[test]
fn skipping_one_tool_does_not_stop_the_others() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n[mcp_servers.search]\nurl = \"https://search.example.com/mcp\"\n",
    );
    put(&sb, CONTINUE, &fixture("continue_v2.yaml"));
    sb.cli()
        .args(["apply", "p", "--to", "continue,opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("nothing written to Continue"));
    assert!(sb.read(OPENCODE).contains("search"));
}

// ---------------------------------------------------------------------------
// Doctor

#[test]
fn doctor_shows_the_schema_each_tool_uses() {
    let sb = Sandbox::empty();
    put(&sb, OPENCODE, &fixture("opencode_v1.json"));
    put(&sb, CONTINUE, &fixture("continue_v2.yaml"));
    sb.cli()
        .args(["doctor"])
        .assert()
        .success()
        .stdout(contains("schema v1").and(contains("schema 'v2' is not one kitbag knows")));
}

#[test]
fn doctor_reports_a_schema_mismatch_and_a_refused_claim() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n[targets.opencode]\nschema = \"v2\"\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\n",
    );
    put(&sb, OPENCODE, &fixture("opencode_v1.json"));
    // Maps one to the other: reported, not a problem.
    sb.cli()
        .args(["doctor", "p", "--to", "opencode"])
        .assert()
        .success()
        .stdout(
            contains("opencode: captured from schema 'v2', the installed tool uses 'v1'")
                .and(contains("No problems found.")),
        );
    // A claim kitbag doesn't know: a problem.
    profile(&sb, CLAIMS_V3);
    sb.cli()
        .args(["doctor", "p", "--to", "opencode"])
        .assert()
        .code(1)
        .stdout(contains("opencode: schema 'v3' is unknown to this kitbag"));
    // Targets left out with --to are not checked.
    sb.cli()
        .args(["doctor", "p", "--to", "claude-code"])
        .assert()
        .success();
}

#[test]
fn doctor_reports_an_installed_schema_it_cannot_write() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n[targets.continue]\nschema = \"v1\"\n",
    );
    put(&sb, CONTINUE, &fixture("continue_v2.yaml"));
    sb.cli()
        .args(["doctor", "p", "--to", "continue"])
        .assert()
        .success()
        .stdout(contains(
            "the installed tool uses schema 'v2', which kitbag can't write",
        ));
}

// ---------------------------------------------------------------------------
// Library level

#[test]
fn plan_refuses_through_the_library_too() {
    let sb = Sandbox::empty();
    let mut p = kitbag::model::Profile::new("p");
    p.schemas.insert("opencode".into(), "v3".into());
    let env = sb.env(Scope::User);
    let err = kitbag::engine::plan(&p, &targets(&["opencode"]), &env, &opts(&p)).unwrap_err();
    assert!(format!("{err:#}").contains("schema 'v3'"), "{err:#}");
    // Naming the layout opencode really has is fine.
    p.schemas.insert("opencode".into(), "v1".into());
    kitbag::engine::plan(&p, &targets(&["opencode"]), &env, &opts(&p)).unwrap();
}

#[test]
fn adapters_declare_what_they_detect() {
    for a in kitbag::adapters::all() {
        // Whatever an adapter reports must be a valid id, and what it
        // declares must be writable or readable by something.
        for s in a.schemas() {
            assert!(kitbag::schema::id_issue(s.id).is_none(), "{}", a.id());
            assert!(s.read || s.write, "{}", a.id());
        }
    }
    let ids: Vec<&str> = kitbag::adapters::all()
        .iter()
        .filter(|a| !a.schemas().is_empty())
        .map(|a| a.id())
        .collect();
    assert_eq!(ids, ["opencode", "continue"]);
}
