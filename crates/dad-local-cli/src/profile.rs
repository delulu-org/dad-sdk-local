//! The mandated release profile, enforced.
//!
//! `dad-local build` and `dad-local test` refuse to proceed unless the
//! addon's own Cargo.toml declares exactly this shape — fastest Rust, no
//! exceptions, and `panic` stays `unwind` because the runtime's panic
//! containment (handler panic → `internal_error`) breaks under `abort`.

/// The exact `[profile.release]` block every addon must carry.
pub const MANDATED_PROFILE_SNIPPET: &str =
    "[profile.release]\nopt-level = 3\nlto = true\ncodegen-units = 1\nstrip = true\n";

/// Returns every violation of the mandated profile in the given addon
/// `Cargo.toml` text. Empty = compliant.
pub fn check_release_profile(cargo_toml: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let doc: toml::Value = match toml::from_str(cargo_toml) {
        Ok(doc) => doc,
        Err(e) => return vec![format!("Cargo.toml is not valid TOML: {e}")],
    };

    if doc.get("workspace").and_then(|w| w.get("members")).is_some() {
        errors.push(
            "the addon crate is a workspace ROOT with members - build each addon standalone. \
             An addon Cargo.toml must not govern other crates' profiles."
                .to_string(),
        );
    }

    let Some(profile) = doc.get("profile").and_then(|p| p.get("release")) else {
        errors.push(format!(
            "missing [profile.release] - add this block to Cargo.toml:\n\n{MANDATED_PROFILE_SNIPPET}"
        ));
        return errors;
    };

    let missing = |field: &str, expected: &str| {
        format!("'profile.release.{field}' is missing or wrong - set `{field} = {expected}`")
    };

    match profile.get("opt-level") {
        Some(toml::Value::Integer(3)) => {}
        _ => errors.push(missing("opt-level", "3")),
    }
    match profile.get("lto") {
        Some(toml::Value::Boolean(true)) => {}
        Some(toml::Value::String(s)) if s == "fat" => {}
        _ => errors.push(missing("lto", "true")),
    }
    match profile.get("codegen-units") {
        Some(toml::Value::Integer(1)) => {}
        _ => errors.push(missing("codegen-units", "1")),
    }
    match profile.get("strip") {
        Some(toml::Value::Boolean(true)) => {}
        Some(toml::Value::String(s)) if s == "symbols" => {}
        _ => errors.push(missing("strip", "true")),
    }
    // panic must be unwind (absent = unwind). `abort` would break the
    // runtime's catch_unwind containment, so it is a rejection, not a hint.
    match profile.get("panic") {
        None => {}
        Some(toml::Value::String(s)) if s == "unwind" => {}
        Some(toml::Value::String(s)) if s == "abort" => errors.push(
            "'profile.release.panic = \"abort\"' is FORBIDDEN - the runtime catches handler \
             panics and answers internal_error; abort would kill the protocol. Remove the \
             panic key (unwind is the default)."
                .to_string(),
        ),
        Some(other) => errors.push(format!(
            "'profile.release.panic' must be \"unwind\" or absent - got {other}"
        )),
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compliant() -> &'static str {
        "[package]\nname = \"x\"\n\n[profile.release]\nopt-level = 3\nlto = true\ncodegen-units = 1\nstrip = true\n"
    }

    #[test]
    fn accepts_the_mandated_profile() {
        assert!(check_release_profile(compliant()).is_empty());
        // lto = "fat" and strip = "symbols" are the same settings spelled out.
        let spelled = "[profile.release]\nopt-level = 3\nlto = \"fat\"\ncodegen-units = 1\nstrip = \"symbols\"\n";
        assert!(check_release_profile(spelled).is_empty());
        // Absent panic = unwind = fine.
    }

    #[test]
    fn rejects_missing_block_and_wrong_values() {
        assert!(check_release_profile("[package]\nname = \"x\"\n")
            .iter()
            .any(|e| e.contains("missing [profile.release]")));

        let weak = "[profile.release]\nopt-level = 2\nlto = false\ncodegen-units = 16\nstrip = false\n";
        let errors = check_release_profile(weak);
        assert_eq!(errors.len(), 4, "all four weak settings flagged: {errors:?}");

        let panic_abort = "[profile.release]\nopt-level = 3\nlto = true\ncodegen-units = 1\nstrip = true\npanic = \"abort\"\n";
        let errors = check_release_profile(panic_abort);
        assert!(errors.iter().any(|e| e.contains("FORBIDDEN")), "{errors:?}");
    }

    #[test]
    fn rejects_workspace_root_with_members() {
        let toml_text = "[package]\nname = \"x\"\n\n[workspace]\nmembers = [\"other\"]\n\n[profile.release]\nopt-level = 3\nlto = true\ncodegen-units = 1\nstrip = true\n";
        let errors = check_release_profile(toml_text);
        assert!(errors.iter().any(|e| e.contains("workspace ROOT")), "{errors:?}");
    }

    #[test]
    fn invalid_toml_is_a_clear_error() {
        assert!(check_release_profile("not toml {{{{").iter().any(|e| e.contains("TOML")));
    }
}
