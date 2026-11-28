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
    /// Only an Ultra subscription can run this model. The proxy refuses the
    /// request when no Ultra account is loaded instead of sending a Pro session
    /// that would receive the Google AI Ultra upsell sheet.
    pub requires_ultra: bool,
}

pub const AUTO: ModelSpec = ModelSpec {
    name: "gemini-4-auto",
    label: "Auto",
    model_id: "8c46e95b1a07cecc",
    number: 6,
    available: true,
    requires_ultra: false,
};

pub const HYDROGEN: ModelSpec = ModelSpec {
    name: "gemini-4-hydrogen",
    label: "4 Hydrogen",
    model_id: "56fdd199312815e2",
    number: 1,
    available: true,
    requires_ultra: false,
};

pub const NEON: ModelSpec = ModelSpec {
    name: "gemini-4-neon",
    label: "Neon",
    model_id: "e6fa609c3fa255c0",
    number: 3,
    available: true,
    requires_ultra: false,
};

pub const CARBON: ModelSpec = ModelSpec {
    name: "gemini-4-carbon",
    label: "Carbon",
    model_id: "95a4c0abcba24364",
    number: 8,
    available: true,
    requires_ultra: true,
};

/// Models advertised on `/v1/models` and accepted by `/v1/chat/completions`.
pub const ADVERTISED: [ModelSpec; 3] = [AUTO, HYDROGEN, NEON];

/// Models to advertise for the current account pool. Carbon only appears when
/// an Ultra account is loaded, because every other pool answers it with an
/// error.
pub fn advertised_for_pool(has_ultra: bool) -> Vec<ModelSpec> {
    let mut models = ADVERTISED.to_vec();
    if has_ultra {
        models.push(CARBON);
    }
    models
}

/// Context window advertised to OpenAI-compatible clients.
///
/// A 180000 character prompt round-trips through StreamGenerate, so 128000
/// tokens is a conservative budget that keeps clients from trimming history
/// themselves while staying below what the web endpoint accepts.
pub const CONTEXT_WINDOW: u32 = 128_000;

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
        assert!(
            advertised_for_pool(false)
                .iter()
                .all(|model| model.name != CARBON.name)
        );
        assert!(
            advertised_for_pool(true)
                .iter()
                .any(|model| model.name == CARBON.name)
        );
    }

    #[test]
    fn carbon_requires_an_ultra_account() {
        let carbon = resolve("gemini-4-carbon");
        assert!(carbon.available);
        assert!(carbon.requires_ultra);
        let others: Vec<ModelSpec> = ["auto", "hydrogen", "neon"]
            .iter()
            .map(|name| resolve(name))
            .collect();
        assert!(others.iter().all(|model| !model.requires_ultra));
    }
}
