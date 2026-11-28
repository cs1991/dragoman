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

/// Return the canonical model a request most likely meant to name.
///
/// OpenAI-compatible clients send their own model ids (`gpt-4o`,
/// `gpt-5-codex`) and those keep falling back to the default model. A name one
/// or two edits away from one of our own ids is a different case: it is almost
/// always a typo such as `gemini-4-carton`, and silently answering with
/// Hydrogen would hide the mistake. Only those near misses are reported.
///
/// `resolve` stays unchanged, so request handling can still fall back for
/// foreign names.
pub fn typo_suggestion(requested: &str) -> Option<ModelSpec> {
    let core = core_token(requested);
    if core.is_empty() {
        return None;
    }
    const CANONICAL: [(&str, ModelSpec); 4] = [
        ("auto", AUTO),
        ("hydrogen", HYDROGEN),
        ("neon", NEON),
        ("carbon", CARBON),
    ];
    if CANONICAL.iter().any(|(name, _)| *name == core) {
        return None;
    }
    if matches!(core.as_str(), "argon" | "argon-thinking" | "thinking") {
        return None;
    }
    let limit = if core.chars().count() >= 6 { 2 } else { 1 };
    let mut best: Option<(usize, ModelSpec)> = None;
    for (name, spec) in CANONICAL {
        let distance = edit_distance(&core, name);
        if distance <= limit && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, spec));
        }
    }
    best.map(|(_, spec)| spec)
}

/// Strip the prefixes clients add in front of the model id.
fn core_token(requested: &str) -> String {
    let mut name = requested.trim().to_ascii_lowercase();
    loop {
        let stripped = ["models/", "gemini-4-", "gemini-"]
            .iter()
            .find_map(|prefix| name.strip_prefix(prefix).map(str::to_string));
        match stripped {
            Some(rest) => name = rest,
            None => break,
        }
    }
    name
}

/// Levenshtein distance over characters, used only for typo detection.
fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
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

    #[test]
    fn typo_suggestion_flags_our_own_names_only() {
        assert_eq!(
            typo_suggestion("gemini-4-carton").map(|model| model.name),
            Some("gemini-4-carbon")
        );
        assert_eq!(
            typo_suggestion("carbonn").map(|model| model.name),
            Some("gemini-4-carbon")
        );
        assert_eq!(
            typo_suggestion("gemini-4-neonn").map(|model| model.name),
            Some("gemini-4-neon")
        );
        assert!(typo_suggestion("gemini-4-carbon").is_none());
        assert!(typo_suggestion("gemini-4-hydrogen").is_none());
        assert!(typo_suggestion("gemini-4-argon").is_none());
        assert!(typo_suggestion("gpt-4o").is_none());
        assert!(typo_suggestion("gpt-5-codex").is_none());
        assert!(typo_suggestion("gemini-1.5-pro").is_none());
        assert!(typo_suggestion("claude-3-5-sonnet").is_none());
    }
}
