pub fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.len() / 32 + 1);
    starts.push(0);
    starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
    starts
}

pub fn utf16_to_byte(line_text: &str, utf16: u32) -> usize {
    if line_text.is_ascii() {
        return (utf16 as usize).min(line_text.len());
    }
    let mut units = 0;
    for (i, c) in line_text.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16() as u32;
    }
    line_text.len()
}

pub fn offset_at(text: &str, starts: &[usize], line: u32, character: u32) -> usize {
    let Some(&start) = starts.get(line as usize) else { return text.len() };
    let end = starts.get(line as usize + 1).map(|e| e - 1).unwrap_or(text.len());
    start + utf16_to_byte(&text[start..end], character)
}

pub struct Edit<'a> {
    pub range: Option<(u32, u32, u32, u32)>,
    pub text: &'a str,
}

pub fn apply_edits(text: &str, edits: &[Edit<'_>]) -> String {
    let mut current = text.to_string();
    for edit in edits {
        match edit.range {
            None => current = edit.text.to_string(),
            Some((sl, sc, el, ec)) => {
                let starts = line_starts(&current);
                let start = offset_at(&current, &starts, sl, sc);
                let end = offset_at(&current, &starts, el, ec).max(start);
                current.replace_range(start..end, edit.text);
            }
        }
    }
    current
}

pub fn line_text(text: &str, line: u32) -> &str {
    text.split('\n').nth(line as usize).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_in_utf16_units() {
        let text = "say(\"雪🐈\")\nmove(1)";
        let out = apply_edits(text, &[Edit { range: Some((0, 6, 0, 8)), text: "x" }]);
        assert_eq!(out, "say(\"雪x\")\nmove(1)");
        let out = apply_edits(&out, &[Edit { range: Some((1, 5, 1, 6)), text: "25" }]);
        assert_eq!(out, "say(\"雪x\")\nmove(25)");
    }
}
