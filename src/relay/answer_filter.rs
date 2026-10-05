//! Filtro de la primera frase de la respuesta hablada.
//!
//! Se retiene hasta el primer punto (o 120 caracteres). Si arranca con
//! "depende" o trae markdown, se descarta y se regenera una sola vez.
//! El resto del texto sale sin backticks, asteriscos ni almohadillas.

pub fn first_sentence(buffer: &str) -> Option<(usize, String)> {
    let mut count = 0usize;
    for (index, ch) in buffer.char_indices() {
        count += 1;
        if matches!(ch, '.' | '!' | '?') || count >= 120 {
            let end = index + ch.len_utf8();
            return Some((end, buffer[..end].to_string()));
        }
    }
    None
}

pub fn opening_rejected(sentence: &str) -> bool {
    let trimmed = sentence.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    lower.starts_with("depende")
        || trimmed.contains('`')
        || trimmed.contains('*')
        || trimmed.contains('#')
}

pub fn strip_markup(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '`' | '*' | '#'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_until_the_first_period_or_120_chars() {
        assert!(first_sentence("sin cierre").is_none());
        let (end, sentence) = first_sentence("Depende del caso. Luego sigo.").unwrap();
        assert_eq!(sentence, "Depende del caso.");
        assert_eq!(&"Depende del caso. Luego sigo."[end..], " Luego sigo.");

        let long = "a".repeat(130);
        let (_, sentence) = first_sentence(&long).unwrap();
        assert_eq!(sentence.chars().count(), 120);
    }

    #[test]
    fn rejects_depende_and_markdown_and_strips_the_rest() {
        assert!(opening_rejected("Depende de cómo esté armado."));
        assert!(opening_rejected("Usa `fetch` para eso."));
        assert!(opening_rejected("## título"));
        assert!(!opening_rejected("Lo haría con una cola."));
        assert_eq!(strip_markup("hola **mundo** `code` #"), "hola mundo code ");
    }
}
