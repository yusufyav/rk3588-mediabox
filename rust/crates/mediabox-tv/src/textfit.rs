//! Breaking text into lines here, for the places the reference sets text with
//! more room between the lines than Slint's `Text` can: Slint 1.17 has no
//! line-height, so a paragraph that must read as the reference's is drawn a
//! line at a time.
//!
//! Widths are estimated, not measured: Inter's advance averages about half an
//! em in mixed text, and the estimate is on the wide side, so a line breaks a
//! little early rather than running off the end.

/// The estimated width of one character, in ems: measured off the reference's
/// Discover summary, 0.47 em a character; a little over, so a line breaks
/// early rather than running off the end.
const EM_PER_CHAR: f32 = 0.48;

fn em_width(text: &str) -> f32 {
    text.chars().count() as f32 * EM_PER_CHAR
}

/// Words laid into lines of at most `line_em` ems, at most `max_lines` of
/// them; what does not fit ends the last line with an ellipsis.
pub fn wrap_words(text: &str, line_em: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if em_width(&candidate) <= line_em || line.is_empty() {
            line = candidate;
        } else {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            last.push('…');
        }
    }
    lines
}

/// Chips laid into lines as the reference's wrap: as many as fit, each as
/// wide as its words plus `pad_em`, `gap_em` apart.
pub fn chip_lines(items: &[String], line_em: f32, pad_em: f32, gap_em: f32) -> Vec<Vec<String>> {
    let mut lines: Vec<Vec<String>> = Vec::new();
    let mut used = 0.0f32;
    for item in items {
        let width = em_width(item) + pad_em;
        match lines.last_mut() {
            Some(line) if used + gap_em + width <= line_em => {
                line.push(item.clone());
                used += gap_em + width;
            }
            _ => {
                lines.push(vec![item.clone()]);
                used = width;
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_breaks_between_words_and_ends_with_an_ellipsis_when_cut() {
        let lines = wrap_words("one two three four five six", 4.5, 2);
        assert_eq!(lines, vec!["one two".to_string(), "three…".to_string()]);
        assert!(lines.iter().all(|l| em_width(l.trim_end_matches('…')) <= 4.5));
    }

    /// The capture's own: three genres on one line, the third name of the
    /// cast on a line of its own.
    #[test]
    fn chips_fill_a_line_before_starting_the_next() {
        let genres: Vec<String> = ["Aksiyon", "Macera", "Gizem"].map(String::from).to_vec();
        assert_eq!(chip_lines(&genres, 23.8, 2.3, 0.6).len(), 1);
        let cast: Vec<String> = ["Anne Hathaway", "Ewan McGregor", "Maisy Stella"].map(String::from).to_vec();
        let lines = chip_lines(&cast, 23.8, 2.3, 0.6);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], vec!["Maisy Stella".to_string()]);
    }
}
