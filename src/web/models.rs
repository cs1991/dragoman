/// Model identity for the Gemini Web StreamGenerate transport.
///
/// The web client selects a model with the `x-goog-ext-525001261-jspb`
/// header instead of a payload slot. Each entry carries the internal model
/// id returned by `otAQ7b` and the model number mirrored into payload slot
/// 79.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub model_id: &'static str,
    pub number: u32,
    pub available: bool,
}

pub const AUTO: ModelSpec = ModelSpec {
    name: "gemini-4-auto",
    label: "Auto",
    model_id: "8c46e95b1a07cecc",
    number: 6,
    available: true,
};

pub const HYDROGEN: ModelSpec = ModelSpec {
    name: "gemini-4-hydrogen",
    label: "4 Hydrogen",
    model_id: "56fdd199312815e2",
    number: 1,
    available: true,
};

pub const NEON: ModelSpec = ModelSpec {
    name: "gemini-4-neon",
    label: "Neon",
    model_id: "e6fa609c3fa255c0",
    number: 3,
    available: true,
};

pub const CARBON: ModelSpec = ModelSpec {
    name: "gemini-4-carbon",
    label: "Carbon",
    model_id: "95a4c0abcba24364",
    number: 8,
    available: false,
};

/// Models advertised on `/v1/models` and accepted by `/v1/chat/completions`.
pub const ADVERTISED: [ModelSpec; 3] = [AUTO, HYDROGEN, NEON];

pub fn default_model() -> ModelSpec {
    HYDROGEN
}

/// Map a requested model name onto a `ModelSpec`.
///
/// Legacy names resolve to the model that currently backs them. The old
/// `gemini-4-argon` label names the default everyday model, and
/// `gemini-4-argon-thinking` names the deeper analysis model.
pub fn resolve(requested: &str) -> ModelSpec {
    let name = requested.trim().to_ascii_lowercase();
    if name.contains("carbon") {
        return CARBON;
    }
    if name.contains("neon") || name.contains("thinking") {
        return NEON;
    }
    if name.contains("auto") {
        return AUTO;
    }
    HYDROGEN
}

/// Tier capacity written into the model header.
///
/// A Pro session reports tier flags `[8,0,38]`, which maps to capacity 2.
/// `DRAGOMAN_MODEL_CAPACITY` overrides the value for other tiers.
pub fn capacity() -> u32 {
    std::env::var("DRAGOMAN_MODEL_CAPACITY")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(2)
}

/// Build the `x-goog-ext-525001261-jspb` header value.
///
/// Layout observed from the web client:
/// `[1,null,null,null,"<id>",null,null,0,[4,5,6,8],null,null,<capacity>,
/// null,null,<number>,<thinking>,"<session>"]`
pub fn model_header(spec: ModelSpec, capacity: u32, thinking: bool, session: &str) -> String {
    let thinking_flag = if thinking { 2 } else { 1 };
    format!(
        "[1,null,null,null,\"{}\",null,null,0,[4,5,6,8],null,null,{},null,null,{},{},\"{}\"]",
        spec.model_id, capacity, spec.number, thinking_flag, session
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_canonical_names() {
        assert_eq!(resolve("gemini-4-auto"), AUTO);
        assert_eq!(resolve("gemini-4-hydrogen"), HYDROGEN);
        assert_eq!(resolve("gemini-4-neon"), NEON);
        assert_eq!(resolve("gemini-4-carbon"), CARBON);
    }

    #[test]
    fn resolves_legacy_and_short_names() {
        assert_eq!(resolve("gemini-4-argon"), HYDROGEN);
        assert_eq!(resolve("gemini-4-argon-thinking"), NEON);
        assert_eq!(resolve("Neon"), NEON);
        assert_eq!(resolve("auto"), AUTO);
        assert_eq!(resolve("unknown-model"), HYDROGEN);
    }

    #[test]
    fn builds_neon_header() {
        let header = model_header(NEON, 2, false, "UUID");
        assert_eq!(
            header,
            "[1,null,null,null,\"e6fa609c3fa255c0\",null,null,0,[4,5,6,8],null,null,2,null,null,3,1,\"UUID\"]"
        );
    }

    #[test]
    fn carbon_is_not_advertised() {
        assert!(ADVERTISED.iter().all(|model| model.name != CARBON.name));
    }
}
