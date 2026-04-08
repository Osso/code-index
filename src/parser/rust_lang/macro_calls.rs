#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MacroCall {
    pub(super) name: String,
    pub(super) qualifier: Option<String>,
    pub(super) line_offset: usize,
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "static", "struct", "trait", "true", "type", "unsafe", "use", "where",
    "while",
];

pub(super) fn scan_macro_calls(text: &str) -> Vec<MacroCall> {
    let bytes = text.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if starts_line_comment(bytes, index) {
            index = skip_line_comment(bytes, index);
            continue;
        }
        if starts_block_comment(bytes, index) {
            index = skip_block_comment(bytes, index);
            continue;
        }
        if starts_quoted_literal(bytes, index) {
            index = skip_quoted_literal(bytes, index);
            continue;
        }
        if starts_raw_string(bytes, index) {
            index = skip_raw_string(bytes, index);
            continue;
        }
        if is_ident_start(bytes[index]) {
            index = next_scan_index(text, bytes, index, &mut calls);
            continue;
        }
        index += 1;
    }

    calls
}

pub(super) fn scan_bare_function_refs(text: &str) -> Vec<MacroCall> {
    let bytes = text.as_bytes();
    let mut refs = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if starts_line_comment(bytes, index) {
            index = skip_line_comment(bytes, index);
            continue;
        }
        if starts_block_comment(bytes, index) {
            index = skip_block_comment(bytes, index);
            continue;
        }
        if starts_quoted_literal(bytes, index) {
            index = skip_quoted_literal(bytes, index);
            continue;
        }
        if starts_raw_string(bytes, index) {
            index = skip_raw_string(bytes, index);
            continue;
        }
        if is_ident_start(bytes[index]) {
            index = next_bare_ref_scan_index(text, bytes, index, &mut refs);
            continue;
        }
        index += 1;
    }

    refs
}

fn next_scan_index(text: &str, bytes: &[u8], start: usize, calls: &mut Vec<MacroCall>) -> usize {
    let Some(candidate) = parse_call_candidate(text, bytes, start) else {
        return start + 1;
    };
    let next = skip_ascii_whitespace(bytes, candidate.next);
    if next >= bytes.len() || bytes[next] != b'(' || is_plain_keyword(&candidate.segments) {
        return next.max(start + 1);
    }

    calls.push(MacroCall {
        name: candidate.name,
        qualifier: candidate.qualifier,
        line_offset: count_newlines(&bytes[..start]),
    });
    next.max(start + 1)
}

fn next_bare_ref_scan_index(
    text: &str,
    bytes: &[u8],
    start: usize,
    refs: &mut Vec<MacroCall>,
) -> usize {
    let Some(candidate) = parse_call_candidate(text, bytes, start) else {
        return start + 1;
    };
    let next = skip_ascii_whitespace(bytes, candidate.next);
    if previous_non_whitespace(bytes, start) == Some(b'.')
        || next < bytes.len() && matches!(bytes[next], b'(' | b'!')
        || is_plain_keyword(&candidate.segments)
        || !looks_like_function_name(&candidate.name)
    {
        return next.max(start + 1);
    }

    refs.push(MacroCall {
        name: candidate.name,
        qualifier: candidate.qualifier,
        line_offset: count_newlines(&bytes[..start]),
    });
    next.max(start + 1)
}

struct CallCandidate {
    name: String,
    qualifier: Option<String>,
    next: usize,
    segments: Vec<String>,
}

fn parse_call_candidate(text: &str, bytes: &[u8], start: usize) -> Option<CallCandidate> {
    let mut cursor = start;
    let mut segments = Vec::new();

    let first_end = scan_identifier(bytes, cursor);
    segments.push(text[cursor..first_end].to_string());
    cursor = first_end;

    while let Some(next_segment) = scan_path_segment(text, bytes, cursor) {
        cursor = next_segment.1;
        segments.push(next_segment.0);
    }

    let name = segments.last()?.clone();
    let qualifier = qualifier_for_call(bytes, start, &segments);
    Some(CallCandidate {
        name,
        qualifier,
        next: cursor,
        segments,
    })
}

fn scan_path_segment(text: &str, bytes: &[u8], cursor: usize) -> Option<(String, usize)> {
    let separator = skip_ascii_whitespace(bytes, cursor);
    if !matches_bytes(bytes, separator, b"::") {
        return None;
    }

    let segment_start = skip_ascii_whitespace(bytes, separator + 2);
    if segment_start >= bytes.len() || !is_ident_start(bytes[segment_start]) {
        return None;
    }

    let segment_end = scan_identifier(bytes, segment_start);
    Some((text[segment_start..segment_end].to_string(), segment_end))
}

fn qualifier_for_call(bytes: &[u8], start: usize, segments: &[String]) -> Option<String> {
    if previous_non_whitespace(bytes, start) == Some(b'.') {
        return None;
    }
    (segments.len() > 1).then(|| segments[..segments.len() - 1].join("::"))
}

fn is_plain_keyword(segments: &[String]) -> bool {
    segments.len() == 1 && RUST_KEYWORDS.contains(&segments[0].as_str())
}

fn looks_like_function_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'a'..='z'))
        && chars.all(|ch| ch == '_' || ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn skip_ascii_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn previous_non_whitespace(bytes: &[u8], start: usize) -> Option<u8> {
    let mut index = start;
    while index > 0 {
        index -= 1;
        if !bytes[index].is_ascii_whitespace() {
            return Some(bytes[index]);
        }
    }
    None
}

fn scan_identifier(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && is_ident_continue(bytes[index]) {
        index += 1;
    }
    index
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn matches_bytes(bytes: &[u8], index: usize, pattern: &[u8]) -> bool {
    index + pattern.len() <= bytes.len() && &bytes[index..index + pattern.len()] == pattern
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| byte == b'\n').count()
}

fn starts_line_comment(bytes: &[u8], index: usize) -> bool {
    matches_bytes(bytes, index, b"//")
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn starts_block_comment(bytes: &[u8], index: usize) -> bool {
    matches_bytes(bytes, index, b"/*")
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 0;
    while index + 1 < bytes.len() {
        if matches_bytes(bytes, index, b"/*") {
            depth += 1;
            index += 2;
            continue;
        }
        if matches_bytes(bytes, index, b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
            continue;
        }
        index += 1;
    }
    bytes.len()
}

fn starts_quoted_literal(bytes: &[u8], index: usize) -> bool {
    matches!(bytes[index], b'"' | b'\'')
}

fn skip_quoted_literal(bytes: &[u8], index: usize) -> usize {
    let quote = bytes[index];
    let mut cursor = index + 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 2;
            continue;
        }
        cursor += 1;
        if bytes[cursor - 1] == quote {
            return cursor;
        }
    }

    bytes.len()
}

fn starts_raw_string(bytes: &[u8], index: usize) -> bool {
    if bytes[index] != b'r' {
        return false;
    }
    let mut cursor = index + 1;
    while cursor < bytes.len() && bytes[cursor] == b'#' {
        cursor += 1;
    }
    cursor < bytes.len() && bytes[cursor] == b'"'
}

fn skip_raw_string(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 1;
    let mut hash_count = 0;
    while cursor < bytes.len() && bytes[cursor] == b'#' {
        hash_count += 1;
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'"' {
        return index + 1;
    }
    cursor += 1;

    while cursor < bytes.len() {
        if bytes[cursor] != b'"' {
            cursor += 1;
            continue;
        }

        let mut closing = cursor + 1;
        let mut matched_hashes = 0;
        while closing < bytes.len() && matched_hashes < hash_count && bytes[closing] == b'#' {
            matched_hashes += 1;
            closing += 1;
        }
        if matched_hashes == hash_count {
            return closing;
        }
        cursor += 1;
    }

    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_macro_calls_skips_literals_and_comments() {
        let calls = scan_macro_calls(
            "(\"resolve_melee_outcome(\", /* should_evade(target) */ helper(), r#\"foo()\"#)",
        );
        assert_eq!(
            calls,
            vec![MacroCall {
                name: "helper".to_string(),
                qualifier: None,
                line_offset: 0,
            }]
        );
    }

    #[test]
    fn scan_macro_calls_keeps_scoped_qualifiers() {
        let calls = scan_macro_calls("(crate::combat::resolve_hit(target), Self::from_roll(7))");
        assert_eq!(
            calls,
            vec![
                MacroCall {
                    name: "resolve_hit".to_string(),
                    qualifier: Some("crate::combat".to_string()),
                    line_offset: 0,
                },
                MacroCall {
                    name: "from_roll".to_string(),
                    qualifier: Some("Self".to_string()),
                    line_offset: 0,
                },
            ]
        );
    }

    #[test]
    fn scan_bare_function_refs_keeps_scoped_system_names() {
        let refs = scan_bare_function_refs(
            "(Update, (process_login_requests, auth::process_register_requests.run_if(in_state(GameState::Playing))))",
        );
        assert_eq!(
            refs,
            vec![
                MacroCall {
                    name: "process_login_requests".to_string(),
                    qualifier: None,
                    line_offset: 0,
                },
                MacroCall {
                    name: "process_register_requests".to_string(),
                    qualifier: Some("auth".to_string()),
                    line_offset: 0,
                },
            ]
        );
    }
}
