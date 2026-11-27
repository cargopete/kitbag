//! Which version of a tool's config format a profile came from, and which one
//! the installed tool uses.
//!
//! A profile is written in kitbag's own neutral shape, so most of a tool's
//! format changes never reach it: kitbag reads the old and the new layout and
//! writes the one the tool's file already uses. What the profile does not
//! say is which layout it was captured from, so a profile that is newer than
//! this kitbag, or aimed at a tool whose config kitbag cannot write, could be
//! applied to the wrong place or in the wrong shape.
//!
//! This module holds what is recorded and the decision about it:
//!
//! * an adapter declares the schemas it knows ([`SchemaSpec`]: readable,
//!   writable) and, where the tool's own files show it cheaply, detects the
//!   one in use ([`crate::adapters::Adapter::detect_schema`]). Nothing is run
//!   to find out: only files that are read anyway.
//! * `capture` records the detected schema in `kitbag.toml`:
//!   `[targets.<id>] schema = "..."`.
//! * [`judge`] compares the recorded schema with the installed one. `apply`
//!   and `doctor` both use it.
//!
//! Only tools whose docs and files give a schema are declared: opencode
//! (its `mcp` layout, `v1` or `v2`) and Continue (`config.yaml`'s `schema`).
//! A tool that declares none records none.

use crate::adapters::Adapter;

/// One config schema of a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaSpec {
    /// The id written to `kitbag.toml` (`v2`).
    pub id: &'static str,
    /// One line, for `doctor`: where the tool keeps its config in this schema.
    pub summary: &'static str,
    /// kitbag can capture from it and take a profile captured from it.
    pub read: bool,
    /// kitbag can write the tool's config in it.
    pub write: bool,
}

/// What to do about one target, from the recorded and the installed schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing recorded or detected to compare, or they agree.
    Fine,
    /// They differ, and kitbag maps one to the other: the profile's
    /// content is applied in the installed tool's schema.
    Converts { from: String, to: String },
    /// The profile names a schema this kitbag does not know for the tool
    /// (a newer kitbag's, or a typo). Refused.
    UnknownRecorded { schema: String },
    /// The profile's schema is one kitbag knows but cannot read from. Refused.
    UnreadableRecorded { schema: String },
    /// The installed tool uses a schema kitbag cannot write (unknown to it,
    /// or write-less). Nothing is written to the tool.
    UnwritableInstalled { schema: String },
}

impl Verdict {
    /// Is the profile itself the problem (as opposed to the installed tool)?
    pub fn refuses_profile(&self) -> bool {
        matches!(
            self,
            Verdict::UnknownRecorded { .. } | Verdict::UnreadableRecorded { .. }
        )
    }

    /// Does the verdict stop kitbag from writing to the tool?
    pub fn stops(&self) -> bool {
        self.refuses_profile() || matches!(self, Verdict::UnwritableInstalled { .. })
    }
}

/// Decide from the adapter's declared `specs`, the schema the profile records
/// for it and the one detected on the machine. The profile is checked first:
/// a claim kitbag cannot understand is refused whatever is installed.
pub fn judge(specs: &[SchemaSpec], recorded: Option<&str>, installed: Option<&str>) -> Verdict {
    let find = |id: &str| specs.iter().find(|s| s.id == id);
    if let Some(r) = recorded {
        match find(r) {
            None => {
                return Verdict::UnknownRecorded {
                    schema: r.to_string(),
                };
            }
            Some(s) if !s.read => {
                return Verdict::UnreadableRecorded {
                    schema: r.to_string(),
                };
            }
            Some(_) => {}
        }
    }
    if let Some(i) = installed
        && !find(i).is_some_and(|s| s.write)
    {
        return Verdict::UnwritableInstalled {
            schema: i.to_string(),
        };
    }
    match (recorded, installed) {
        (Some(r), Some(i)) if r != i => Verdict::Converts {
            from: r.to_string(),
            to: i.to_string(),
        },
        _ => Verdict::Fine,
    }
}

/// [`judge`] for an adapter on this machine.
pub fn verdict(
    a: &dyn Adapter,
    recorded: Option<&str>,
    env: &crate::adapters::Env,
) -> (Verdict, Option<String>) {
    let installed = a.detect_schema(env);
    (
        judge(a.schemas(), recorded, installed.as_deref()),
        installed,
    )
}

/// What is wrong with a schema id in a manifest, if anything.
pub fn id_issue(id: &str) -> Option<String> {
    if id.is_empty() || id.len() > 64 {
        return Some("a schema is 1 to 64 characters".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Some("a schema may only contain letters, digits, '.', '_' and '-'".to_string());
    }
    None
}

/// The refusal for a profile whose recorded schema `v` says it cannot be
/// applied, for `target` (`name`). `None` when the profile is not the problem.
pub fn refusal(target: &str, name: &str, v: &Verdict) -> Option<String> {
    let (schema, why) = match v {
        Verdict::UnknownRecorded { schema } => (
            schema,
            "kitbag does not know that schema (the profile may come from a newer kitbag, or be wrong)",
        ),
        Verdict::UnreadableRecorded { schema } => {
            (schema, "kitbag knows that schema but cannot read from it")
        }
        _ => return None,
    };
    Some(format!(
        "the profile says its {target} settings are in schema '{schema}', and {why}; \
         not applying it to {name}. Update kitbag, or recapture the {target} part of the profile with this kitbag"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: &[SchemaSpec] = &[
        SchemaSpec {
            id: "v1",
            summary: "old",
            read: true,
            write: true,
        },
        SchemaSpec {
            id: "v2",
            summary: "new",
            read: true,
            write: true,
        },
        SchemaSpec {
            id: "ro",
            summary: "read only",
            read: true,
            write: false,
        },
        SchemaSpec {
            id: "wo",
            summary: "write only",
            read: false,
            write: true,
        },
    ];

    #[test]
    fn nothing_to_compare_is_fine() {
        assert_eq!(judge(SPECS, None, None), Verdict::Fine);
        assert_eq!(judge(SPECS, Some("v1"), None), Verdict::Fine);
        assert_eq!(judge(SPECS, None, Some("v2")), Verdict::Fine);
        assert_eq!(judge(SPECS, Some("v2"), Some("v2")), Verdict::Fine);
        assert_eq!(judge(&[], None, None), Verdict::Fine);
    }

    #[test]
    fn known_schemas_convert() {
        assert_eq!(
            judge(SPECS, Some("v1"), Some("v2")),
            Verdict::Converts {
                from: "v1".into(),
                to: "v2".into()
            }
        );
        assert!(!judge(SPECS, Some("v2"), Some("v1")).stops());
    }

    #[test]
    fn an_unknown_or_unreadable_claim_is_refused_whatever_is_installed() {
        for installed in [None, Some("v2")] {
            let v = judge(SPECS, Some("v3"), installed);
            assert_eq!(
                v,
                Verdict::UnknownRecorded {
                    schema: "v3".into()
                }
            );
            assert!(v.refuses_profile() && v.stops());
        }
        let v = judge(SPECS, Some("wo"), Some("v1"));
        assert_eq!(
            v,
            Verdict::UnreadableRecorded {
                schema: "wo".into()
            }
        );
        // An adapter that declares nothing knows no schema to claim.
        assert!(judge(&[], Some("v1"), None).refuses_profile());
    }

    #[test]
    fn an_installed_schema_kitbag_cannot_write_stops_that_tool() {
        for installed in ["ro", "v9"] {
            let v = judge(SPECS, Some("v1"), Some(installed));
            assert_eq!(
                v,
                Verdict::UnwritableInstalled {
                    schema: installed.into()
                }
            );
            assert!(v.stops() && !v.refuses_profile());
        }
        // Even with nothing recorded.
        assert!(judge(SPECS, None, Some("v9")).stops());
    }

    #[test]
    fn ids_are_plain() {
        assert!(id_issue("v2").is_none());
        assert!(id_issue("config.yaml-v1").is_none());
        assert!(id_issue("").is_some());
        assert!(id_issue("v 2").is_some());
        assert!(id_issue("v2\n").is_some());
        assert!(id_issue(&"a".repeat(65)).is_some());
    }
}
