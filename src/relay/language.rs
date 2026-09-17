//! Normalización de idioma y detección ligera desde la transcripción.

/// Etiqueta legible para prompts (`en` → `English`, `español` → `Spanish`, etc.).
pub fn normalize_language_label(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "" => "English".into(),
        "en" | "english" | "inglés" | "ingles" | "eng" | "en-us" | "en-gb" => "English".into(),
        "es" | "español" | "espanol" | "spanish" | "castellano" | "es-mx" | "es-es" | "es-pe" => {
            "Spanish".into()
        }
        "pt" | "português" | "portugues" | "portuguese" | "pt-br" => "Portuguese".into(),
        other => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                "English".into()
            } else {
                trimmed.to_string()
            }
        }
    }
}

/// Pista heurística del idioma de la última pregunta (STT).
pub fn detect_question_language(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.trim().is_empty() {
        return None;
    }

    let spanish_markers = [
        "¿",
        " qué ",
        " cuál",
        " cuáles",
        " cómo ",
        " cuándo ",
        " dónde ",
        " por qué ",
        " explica",
        " dime",
        " cuéntame",
        " cuentame",
        " diferencias",
        " entre ",
        " cuál es",
        " cuáles son",
        " habl",
        " cuént",
        " mira,",
        " a ver,",
        " bueno,",
        " son las",
        " es la",
        " es el",
    ];
    let english_markers = [
        " what ",
        " how ",
        " explain",
        " tell me",
        " difference",
        " differences",
        " between ",
        " when ",
        " why ",
        " which ",
        " describe ",
        " compare ",
        " can you ",
        " could you ",
        " walk me ",
        " talk about ",
        " what are ",
        " what is ",
        " how do ",
        " how would ",
    ];

    let es_score = spanish_markers.iter().filter(|m| t.contains(*m)).count();
    let en_score = english_markers.iter().filter(|m| t.contains(*m)).count();

    if es_score > en_score && es_score > 0 {
        Some("Spanish")
    } else if en_score > es_score && en_score > 0 {
        Some("English")
    } else {
        None
    }
}

/// Bloque de instrucciones de idioma para system prompts (relay / visión).
pub fn response_language_instructions(configured: &str, transcript: &str) -> String {
    let default = normalize_language_label(configured);
    let detected = detect_question_language(transcript).unwrap_or(&default);

    format!(
        "IDIOMA OBLIGATORIO PARA ESTA RESPUESTA: {detected}\n\
         IDIOMA POR DEFECTO (configuración del cliente): {default}\n\
         REGLAS DE IDIOMA:\n\
         - Tu respuesta completa debe estar EXCLUSIVAMENTE en {detected}.\n\
         - Muletillas, transiciones y todo el texto en {detected}.\n\
         - Si la pregunta está en inglés: responde 100% en inglés, sin palabras como 'Bueno', 'Mira', 'Fíjate', etc.\n\
         - Si la pregunta está en español: responde 100% en español.\n\
         - SOLO términos técnicos (RxJS, API, REST, DDD) pueden quedar en inglés si son nombres propios.\n\
         - Si no logras detectar el idioma de la pregunta con claridad, usa {default}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_language_codes() {
        assert_eq!(normalize_language_label("en"), "English");
        assert_eq!(normalize_language_label("english"), "English");
        assert_eq!(normalize_language_label("es"), "Spanish");
        assert_eq!(normalize_language_label("español"), "Spanish");
        assert_eq!(normalize_language_label("Peruano"), "Peruano");
    }

    #[test]
    fn detects_spanish_question() {
        assert_eq!(
            detect_question_language("¿Cuáles son las diferencias entre RxJS y las promesas?"),
            Some("Spanish")
        );
    }

    #[test]
    fn detects_english_question() {
        assert_eq!(
            detect_question_language("What are the differences between RxJS and Promises?"),
            Some("English")
        );
    }
}
