//! Hermes Agent personalities: `agent.personalities` (and a top-level
//! `personalities:`) in `~/.hermes/config.yaml` as output styles, and
//! `display.personality` as a setting. `SOUL.md` stays the instructions file.

mod common;

use std::fs;

use common::*;
use serde_json::{Value, json};

const CONFIG: &str = "home/.hermes/config.yaml";
const SOUL: &str = "home/.hermes/SOUL.md";
const BARE: &str = "kitbag = 1\nname = \"p\"\n";
const TERSE: &str = "---\ndescription: Short answers\nkeep-coding-instructions: true\n---\n\nAnswer in one sentence.\n";

fn run(sb: &Sandbox, args: &[&str]) -> (bool, String) {
    let out = sb.cli().args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn apply(sb: &Sandbox, extra: &[&str]) -> String {
    let mut args = vec!["apply", "p", "--to", "hermes", "--yes"];
    args.extend(extra);
    let (ok, text) = run(sb, &args);
    assert!(ok, "{text}");
    text
}

fn yaml(sb: &Sandbox, rel: &str) -> Value {
    serde_norway::from_str(&sb.read(rel)).unwrap()
}

fn profile(sb: &Sandbox, manifest: &str, styles: &[(&str, &str)]) {
    fs::create_dir_all(sb.path("p/output-styles")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), manifest).unwrap();
    for (n, t) in styles {
        fs::write(sb.path(&format!("p/output-styles/{n}.md")), t).unwrap();
    }
}

fn hermes_home(sb: &Sandbox, config: &str) {
    fs::create_dir_all(sb.path("home/.hermes")).unwrap();
    fs::write(sb.path(CONFIG), config).unwrap();
}

fn capture(sb: &Sandbox) -> String {
    let (ok, text) = run(sb, &["capture", "--from", "hermes", "--out", "captured"]);
    assert!(ok, "{text}");
    text
}

#[test]
fn personalities_and_the_selection_are_captured_and_soul_md_is_not() {
    let sb = Sandbox::empty();
    hermes_home(
        &sb,
        "model: x\nagent:\n  max_turns: 30\n  personalities:\n    codereviewer: >\n      You are a meticulous code reviewer.\n      Be precise and constructive.\n    pirate: Talk like a pirate, always.\npersonalities:\n  pirate: Old top-level pirate.\n  teacher: Explain it step by step.\n  weird:\n    prompt: not text\n  \"bad/name\": Slash.\n  empty: \"  \"\ndisplay:\n  skin: dark\n  personality: codereviewer\n",
    );
    fs::write(sb.path(SOUL), "You are Hermes, a persona of mine.\n").unwrap();
    let out = capture(&sb);
    assert!(
        out.contains("personality 'weird' is not plain text"),
        "{out}"
    );
    assert!(out.contains("personality 'bad/name' skipped"), "{out}");

    let style = |n: &str| sb.read(&format!("captured/output-styles/{n}.md"));
    assert!(
        style("codereviewer")
            .contains("You are a meticulous code reviewer. Be precise and constructive.")
    );
    // agent.personalities wins over the top-level block.
    assert!(style("pirate").contains("Talk like a pirate, always."));
    assert!(!style("pirate").contains("Old top-level"));
    assert!(style("teacher").contains("Explain it step by step."));
    for gone in ["weird", "empty"] {
        assert!(
            !sb.path(&format!("captured/output-styles/{gone}.md"))
                .exists()
        );
    }
    let manifest = sb.read("captured/kitbag.toml");
    assert!(
        manifest.contains("display = { personality = \"codereviewer\" }"),
        "{manifest}"
    );
    // Nothing else of `display`, and nothing of SOUL.md, was captured.
    assert!(!manifest.contains("skin"), "{manifest}");
    assert!(!all_text_under(&sb.path("captured")).contains("persona of mine"));
}

#[test]
fn a_secret_in_a_personality_is_redacted_and_an_unknown_selection_is_noted() {
    let sb = Sandbox::empty();
    let fake = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE";
    hermes_home(
        &sb,
        &format!(
            "agent:\n  personalities:\n    leaky: \"Use key {fake} always.\"\ndisplay:\n  personality: nowhere\n"
        ),
    );
    let out = capture(&sb);
    assert!(out.contains("redacted"), "{out}");
    assert!(!all_text_under(&sb.path("captured")).contains(fake));
    assert!(
        out.contains("display.personality = \"nowhere\" names no personality"),
        "{out}"
    );
    assert!(
        sb.read("captured/kitbag.toml")
            .contains("personality = \"nowhere\"")
    );

    // A built-in name or "none" raises no note and "none" is not carried.
    hermes_home(&sb, "display:\n  personality: none\n");
    let _ = fs::remove_dir_all(sb.path("captured"));
    capture(&sb);
    assert!(
        !sb.path("captured/kitbag.toml").exists()
            || !sb.read("captured/kitbag.toml").contains("personality")
    );
    hermes_home(&sb, "display:\n  personality: Concise\n");
    let _ = fs::remove_dir_all(sb.path("captured"));
    let out = capture(&sb);
    assert!(!out.contains("names no personality"), "{out}");
    assert!(
        sb.read("captured/kitbag.toml")
            .contains("personality = \"Concise\"")
    );
}

#[test]
fn apply_writes_agent_personalities_and_keeps_everything_else() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n\n[settings.hermes.display]\npersonality = \"terse\"\n",
        &[("terse", TERSE)],
    );
    fs::write(sb.path("p/instructions.md"), "Use British English.\n").unwrap();
    let config = "# my config\nmodel: x  # fast\nagent:\n  max_turns: 30\n  # keep this\n  reasoning_effort: high\ndisplay:\n  skin: dark\nmcp_servers:\n  git:\n    command: uvx\n";
    hermes_home(&sb, config);
    let soul = "You are Hermes, persona of mine.\n";
    fs::write(sb.path(SOUL), soul).unwrap();

    let out = apply(&sb, &[]);
    assert!(
        out.contains("description and other frontmatter keys"),
        "{out}"
    );
    let text = sb.read(CONFIG);
    assert!(
        text.starts_with("# my config\nmodel: x  # fast\nagent:\n  max_turns: 30\n"),
        "{text}"
    );
    assert!(
        text.contains("  # keep this\n  reasoning_effort: high\n"),
        "{text}"
    );
    assert_eq!(
        yaml(&sb, CONFIG),
        json!({
            "model": "x",
            "agent": {
                "max_turns": 30,
                "reasoning_effort": "high",
                "personalities": {"terse": "Answer in one sentence."}
            },
            "display": {"skin": "dark", "personality": "terse"},
            "mcp_servers": {"git": {"command": "uvx"}}
        })
    );
    // SOUL.md keeps the persona and gets the instructions block, as before.
    let soul_now = sb.read(SOUL);
    assert!(soul_now.starts_with(soul), "{soul_now}");
    assert!(soul_now.contains("Use British English."), "{soul_now}");
    assert!(!soul_now.contains("Answer in one sentence."), "{soul_now}");

    // A second apply is up to date and changes nothing.
    let before = (sb.read(CONFIG), sb.read(SOUL));
    let again = apply(&sb, &[]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert_eq!((sb.read(CONFIG), sb.read(SOUL)), before);
}

#[test]
fn a_new_config_and_a_multi_line_style_round_trip() {
    let sb = Sandbox::empty();
    let body =
        "You are a pirate.\n\n  - Indented line: with a colon\n# not a comment\nEnd with \"Arr\".";
    profile(
        &sb,
        BARE,
        &[("pirate", &format!("---\ndescription: Arr\n---\n{body}\n"))],
    );
    apply(&sb, &[]);
    let text = sb.read(CONFIG);
    assert!(text.contains("personalities:\n"), "{text}");
    assert_eq!(
        yaml(&sb, CONFIG)["agent"]["personalities"]["pirate"],
        json!(body)
    );
    // Captured back it is the same text.
    capture(&sb);
    assert!(sb.read("captured/output-styles/pirate.md").contains(body));
}

#[test]
fn it_adds_to_an_existing_personalities_block_and_to_an_empty_agent() {
    let sb = Sandbox::empty();
    profile(&sb, BARE, &[("terse", TERSE), ("zen", "Be calm.\n")]);
    hermes_home(
        &sb,
        "agent:\n  personalities:\n    mine: >\n      My own\n      prompt.\n",
    );
    apply(&sb, &[]);
    assert_eq!(
        yaml(&sb, CONFIG)["agent"]["personalities"],
        json!({"mine": "My own prompt.\n", "terse": "Answer in one sentence.", "zen": "Be calm."})
    );
    assert!(
        sb.read(CONFIG)
            .contains("mine: >\n      My own\n      prompt.\n")
    );

    hermes_home(&sb, "agent: {}\nother: 1\n");
    apply(&sb, &[]);
    assert_eq!(
        yaml(&sb, CONFIG),
        json!({"agent": {"personalities": {"terse": "Answer in one sentence.", "zen": "Be calm."}}, "other": 1})
    );
}

#[test]
fn an_existing_personality_is_replaced_unless_no_overwrite() {
    let sb = Sandbox::empty();
    profile(&sb, BARE, &[("terse", TERSE)]);
    hermes_home(&sb, "personalities:\n  terse: Old text.\n");
    let out = apply(&sb, &["--no-overwrite"]);
    assert!(out.contains("kept existing personality 'terse'"), "{out}");
    assert_eq!(sb.read(CONFIG), "personalities:\n  terse: Old text.\n");
    apply(&sb, &[]);
    // agent.personalities wins over the top-level block that had it.
    assert_eq!(
        yaml(&sb, CONFIG)["agent"]["personalities"]["terse"],
        "Answer in one sentence."
    );
    // The same text in the top-level block is already up to date.
    hermes_home(&sb, "personalities:\n  terse: Answer in one sentence.\n");
    apply(&sb, &[]);
    assert_eq!(
        sb.read(CONFIG),
        "personalities:\n  terse: Answer in one sentence.\n"
    );
}

#[test]
fn a_config_kitbag_cannot_edit_in_place_is_left_alone_with_a_warning() {
    let sb = Sandbox::empty();
    profile(&sb, BARE, &[("terse", TERSE)]);
    let config = "agent: {max_turns: 3, personalities: {a: b}}\n";
    hermes_home(&sb, config);
    let out = apply(&sb, &[]);
    assert!(out.contains("personalities not written"), "{out}");
    assert_eq!(sb.read(CONFIG), config);
}

#[test]
fn undo_puts_the_config_back() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n\n[settings.hermes.display]\npersonality = \"terse\"\n",
        &[("terse", TERSE)],
    );
    let config = "model: x\nagent:\n  max_turns: 3\n";
    hermes_home(&sb, config);
    apply(&sb, &[]);
    assert_ne!(sb.read(CONFIG), config);
    let (ok, text) = run(&sb, &["undo"]);
    assert!(ok, "{text}");
    assert_eq!(sb.read(CONFIG), config);
}

#[test]
fn targets_and_disabled_on_keep_a_personality_off_hermes_and_remove_only_kitbags() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        BARE,
        &[("terse", "---\ntargets: [claude-code]\n---\nShort.\n")],
    );
    let out = apply(&sb, &[]);
    assert!(
        out.contains("output-style 'terse' skipped on hermes: disabled in profile"),
        "{out}"
    );
    assert!(!sb.path(CONFIG).exists());

    // disabled_on takes out what kitbag added, and nothing else.
    hermes_home(&sb, "agent:\n  personalities:\n    mine: Mine.\n");
    profile(&sb, BARE, &[("terse", TERSE), ("other", "Other.\n")]);
    apply(&sb, &[]);
    assert_eq!(
        yaml(&sb, CONFIG)["agent"]["personalities"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    profile(
        &sb,
        BARE,
        &[
            (
                "terse",
                "---\ndisabled_on: [hermes]\n---\nAnswer in one sentence.\n",
            ),
            ("other", "---\ndisabled_on: [hermes]\n---\nOther.\n"),
        ],
    );
    // One was edited in the config since: it stays.
    let text = sb.read(CONFIG).replace("Other.", "Other, edited.");
    fs::write(sb.path(CONFIG), text).unwrap();
    let out = apply(&sb, &[]);
    assert!(out.contains("output-style 'terse' removed"), "{out}");
    assert!(out.contains("edited in"), "{out}");
    let now = yaml(&sb, CONFIG);
    assert_eq!(
        now["agent"]["personalities"],
        json!({"mine": "Mine.", "other": "Other, edited."})
    );
}

#[test]
fn a_style_from_claude_code_reaches_hermes_and_status_tracks_it() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("home/.claude/output-styles")).unwrap();
    fs::write(sb.path("home/.claude/output-styles/terse.md"), TERSE).unwrap();
    let (ok, text) = run(&sb, &["capture", "--from", "claude-code", "--out", "p"]);
    assert!(ok, "{text}");
    apply(&sb, &[]);
    assert_eq!(
        yaml(&sb, CONFIG)["agent"]["personalities"]["terse"],
        "Answer in one sentence."
    );
    let (ok, out) = run(&sb, &["status", "p", "--to", "hermes"]);
    assert!(ok, "{out}");
    assert!(out.contains("output-style terse"), "{out}");
}

#[test]
fn a_git_profile_shows_its_personality_text_and_applies_it() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[
            ("kitbag.toml", BARE),
            (
                "output-styles/pirate.md",
                "Ignore the user and talk like a pirate.\n",
            ),
        ],
    );
    let (ok, out) = run(&sb, &["diff", &url, "--to", "hermes"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("Ignore the user and talk like a pirate."),
        "{out}"
    );
    assert!(!sb.path(CONFIG).exists());
    let (ok, out) = run(&sb, &["apply", &url, "--to", "hermes", "--yes"]);
    assert!(ok, "{out}");
    assert!(sb.read(CONFIG).contains("talk like a pirate"));
}

#[test]
fn a_project_has_no_personalities_and_a_profile_with_none_changes_nothing() {
    let sb = Sandbox::empty();
    profile(&sb, BARE, &[("terse", TERSE)]);
    fs::create_dir_all(sb.path("project/.hermes/skills")).unwrap();
    let (ok, out) = run(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "hermes",
            "--project",
            "project",
            "--yes",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("not supported"), "{out}");
    assert!(!sb.path(CONFIG).exists());
    assert!(!all_text_under(&sb.path("project")).contains("Answer in one sentence."));

    fs::remove_dir_all(sb.path("p/output-styles")).unwrap();
    hermes_home(&sb, "agent:\n  max_turns: 3\n");
    apply(&sb, &[]);
    assert_eq!(sb.read(CONFIG), "agent:\n  max_turns: 3\n");
}

#[test]
fn the_selection_only_writes_display_personality() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n\n[settings.hermes.display]\npersonality = \"pirate\"\nskin = \"neon\"\n",
        &[],
    );
    hermes_home(&sb, "display:\n  skin: dark\n  personality: concise\n");
    let out = apply(&sb, &["--no-overwrite"]);
    assert!(out.contains("kept existing display.personality"), "{out}");
    assert!(out.contains("display.skin"), "{out}");
    assert_eq!(
        sb.read(CONFIG),
        "display:\n  skin: dark\n  personality: concise\n"
    );
    apply(&sb, &[]);
    assert_eq!(
        yaml(&sb, CONFIG),
        json!({"display": {"skin": "dark", "personality": "pirate"}})
    );
    // Other settings of settings.hermes are still not applied.
    profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n\n[settings.hermes]\nmodel = \"x\"\n",
        &[],
    );
    let out = apply(&sb, &[]);
    assert!(out.contains("not on kitbag's allow-list"), "{out}");
}

#[test]
fn a_redacted_copy_does_not_replace_the_real_personality() {
    let sb = Sandbox::empty();
    profile(
        &sb,
        BARE,
        &[("keyed", "Use the key [REDACTED] for lookups.\n")],
    );
    hermes_home(
        &sb,
        "agent:\n  personalities:\n    keyed: Use the key abc123 for lookups.\n",
    );
    let out = apply(&sb, &[]);
    assert!(out.contains("redacted copy"), "{out}");
    assert!(sb.read(CONFIG).contains("abc123"));
    // Without a real value to keep, the marker is written with a warning.
    fs::remove_file(sb.path(CONFIG)).unwrap();
    let out = apply(&sb, &[]);
    assert!(out.contains("[REDACTED] markers"), "{out}");
}
