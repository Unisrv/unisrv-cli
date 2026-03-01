use anyhow::Result;
use uuid::Uuid;

/// Trait for resources that can be resolved by UUID, name, or UUID prefix.
pub trait Identifiable {
    fn id(&self) -> Uuid;
    fn name(&self) -> Option<&str>;
}

impl<T: Identifiable> Identifiable for &T {
    fn id(&self) -> Uuid {
        (*self).id()
    }
    fn name(&self) -> Option<&str> {
        (*self).name()
    }
}

/// Resolve a user-provided identifier (full UUID, name, or UUID prefix) to a UUID.
///
/// The resolution strategy:
/// 1. Try parsing as a full UUID
/// 2. Try exact name match (if the resource has names)
/// 3. Try UUID prefix match (must be unique)
pub fn resolve_id<T: Identifiable>(input: &str, items: &[T], entity_name: &str) -> Result<Uuid> {
    // Try exact UUID parse
    if let Ok(parsed) = Uuid::parse_str(input) {
        return Ok(parsed);
    }

    // Try exact name match
    for item in items {
        if item.name().is_some_and(|name| name == input) {
            return Ok(item.id());
        }
    }

    // Try UUID prefix match
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let matches: Vec<_> = items
            .iter()
            .filter(|item| item.id().to_string().starts_with(input))
            .collect();

        match matches.len() {
            1 => return Ok(matches[0].id()),
            0 => {
                return Err(anyhow::anyhow!(
                    "No {} found matching '{}'",
                    entity_name,
                    input
                ));
            }
            n => {
                return Err(anyhow::anyhow!(
                    "Ambiguous: {} {}s match prefix '{}'. Be more specific.",
                    n,
                    entity_name,
                    input
                ));
            }
        }
    }

    Err(anyhow::anyhow!(
        "No {} found with name or UUID '{}'",
        entity_name,
        input
    ))
}
