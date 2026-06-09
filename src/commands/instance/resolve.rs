//! Resolve a user-supplied instance reference to a concrete instance.
//!
//! A `<ref>` may be a full UUID, an exact instance name, or a unique UUID
//! prefix, tried in that order. Resolution is scoped to the instances of the
//! already-selected environment, so a name need only be unique within that env.
//! Ambiguity (a name shared by replicas, or a prefix matching several ids) is an
//! error that lists the candidates rather than a silent pick.

use anyhow::{Result, anyhow, bail};
use unisrv_api::models::InstanceListEntry;
use uuid::Uuid;

/// Resolve `input` against `instances`, returning the matched instance.
pub fn resolve_instance<'a>(
    input: &str,
    instances: &'a [InstanceListEntry],
) -> Result<&'a InstanceListEntry> {
    // Trim once so a clipboard-pasted id with a trailing newline still parses,
    // and a blank reference can't vacuously match every instance below.
    let input = input.trim();
    if input.is_empty() {
        bail!("no instance reference given");
    }

    if let Ok(id) = Uuid::parse_str(input) {
        return instances
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| anyhow!("no instance with id {id} in this environment"));
    }

    let by_name: Vec<&InstanceListEntry> = instances
        .iter()
        .filter(|i| i.name.as_deref() == Some(input))
        .collect();
    match by_name.as_slice() {
        [only] => return Ok(only),
        many if many.len() >= 2 => {
            let listed = many
                .iter()
                .map(|i| describe(i))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "multiple instances are named {input:?}: [{listed}]. Use a UUID or UUID prefix to disambiguate."
            );
        }
        _ => {}
    }

    // A name typo shouldn't be reported as a failed UUID-prefix match, so only
    // attempt prefix resolution when the input could plausibly be one.
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        // UUID strings render lowercase; match case-insensitively so an
        // uppercase-hex prefix resolves like the case-insensitive full-UUID parse.
        let needle = input.to_ascii_lowercase();
        let by_prefix: Vec<&InstanceListEntry> = instances
            .iter()
            .filter(|i| i.id.to_string().starts_with(&needle))
            .collect();
        match by_prefix.as_slice() {
            [only] => return Ok(only),
            [] => bail!("no instance found matching {input:?}"),
            many => {
                let listed = many
                    .iter()
                    .map(|i| describe(i))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "{} instances match the prefix {input:?}: [{listed}]. Use a longer prefix or the full UUID.",
                    many.len()
                );
            }
        }
    }

    bail!("no instance found matching {input:?}")
}

/// A short, human-scannable description of an instance for ambiguity errors:
/// `<short-id> (<name>, <state>)`.
fn describe(instance: &InstanceListEntry) -> String {
    let short = &instance.id.to_string()[..8];
    let name = instance.name.as_deref().unwrap_or("<unnamed>");
    format!("{short} ({name}, {})", instance.state.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use unisrv_api::models::InstanceState;

    fn instance(id: Uuid, name: Option<&str>, state: &str) -> InstanceListEntry {
        InstanceListEntry {
            id,
            name: name.map(String::from),
            state: InstanceState(state.to_string()),
            container_image: "nginx:latest".to_string(),
            created_at: NaiveDateTime::default(),
            deployment: None,
        }
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn resolves_a_full_uuid_present_in_the_list() {
        let target = uuid(0xA1);
        let instances = vec![
            instance(uuid(0xB2), Some("web"), "running"),
            instance(target, Some("api"), "running"),
        ];

        let got = resolve_instance(&target.to_string(), &instances).unwrap();
        assert_eq!(got.id, target);
    }

    #[test]
    fn resolves_a_unique_exact_name() {
        let instances = vec![
            instance(uuid(0xB2), Some("web"), "running"),
            instance(uuid(0xA1), Some("api"), "running"),
        ];

        let got = resolve_instance("api", &instances).unwrap();
        assert_eq!(got.id, uuid(0xA1));
    }

    #[test]
    fn resolves_a_unique_uuid_prefix() {
        let a = Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000000").unwrap();
        let b = Uuid::parse_str("bbbbbbbb-0000-0000-0000-000000000000").unwrap();
        let instances = vec![
            instance(a, Some("web"), "running"),
            instance(b, Some("api"), "running"),
        ];

        let got = resolve_instance("aaaa", &instances).unwrap();
        assert_eq!(got.id, a);
    }

    #[test]
    fn ambiguous_name_errors_and_lists_candidates() {
        // Deployment replicas commonly share a name; resolving such a name must
        // refuse rather than silently pick, and show ids+states to disambiguate.
        let a = uuid(0xA1);
        let b = uuid(0xB2);
        let instances = vec![
            instance(a, Some("worker"), "running"),
            instance(b, Some("worker"), "exited"),
        ];

        let err = resolve_instance("worker", &instances).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("worker"), "names the ref: {msg}");
        assert!(msg.contains(&a.to_string()[..8]), "lists first id: {msg}");
        assert!(msg.contains(&b.to_string()[..8]), "lists second id: {msg}");
        assert!(msg.contains("exited"), "shows state to disambiguate: {msg}");
    }

    #[test]
    fn ambiguous_prefix_errors() {
        let a = Uuid::parse_str("aaaaaaaa-1111-0000-0000-000000000000").unwrap();
        let b = Uuid::parse_str("aaaaaaaa-2222-0000-0000-000000000000").unwrap();
        let instances = vec![
            instance(a, Some("web"), "running"),
            instance(b, Some("api"), "running"),
        ];

        let err = resolve_instance("aaaaaaaa", &instances).unwrap_err();
        assert!(format!("{err:#}").contains("prefix"), "{err:#}");
    }

    #[test]
    fn unknown_ref_errors() {
        let instances = vec![instance(uuid(0xA1), Some("web"), "running")];
        let err = resolve_instance("nope", &instances).unwrap_err();
        assert!(format!("{err:#}").contains("nope"));
    }

    #[test]
    fn blank_input_is_rejected_not_matched_as_a_prefix() {
        // An empty/whitespace ref must error rather than vacuously match every
        // instance via starts_with("") and silently pick one.
        let instances = vec![instance(uuid(0xA1), Some("web"), "running")];
        let err = resolve_instance("   ", &instances).unwrap_err();
        assert!(
            format!("{err:#}").contains("no instance reference"),
            "{err:#}"
        );
    }

    #[test]
    fn uppercase_uuid_prefix_resolves() {
        let a = Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000000").unwrap();
        let instances = vec![instance(a, Some("web"), "running")];
        let got = resolve_instance("AAAA", &instances).unwrap();
        assert_eq!(
            got.id, a,
            "an uppercase-hex prefix should resolve like lowercase"
        );
    }

    #[test]
    fn whitespace_around_a_full_uuid_is_trimmed() {
        let a = uuid(0xA1);
        let instances = vec![instance(a, Some("web"), "running")];
        let got = resolve_instance(&format!("  {a}\n"), &instances).unwrap();
        assert_eq!(got.id, a);
    }

    #[test]
    fn full_uuid_absent_from_env_errors() {
        // logs is environment-scoped: a real UUID that isn't in this env's list
        // must error clearly rather than be forwarded to a 404.
        let instances = vec![instance(uuid(0xA1), Some("web"), "running")];
        let absent = uuid(0xDEAD);
        let err = resolve_instance(&absent.to_string(), &instances).unwrap_err();
        assert!(format!("{err:#}").contains(&absent.to_string()));
    }
}
