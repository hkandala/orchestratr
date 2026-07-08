use std::path::PathBuf;

const PREAMBLE_PREFIX: &str =
    "When you are completely finished, write your full final answer as markdown to the file: ";
const PREAMBLE_SUFFIX: &str = ". Do not consider the task done until that file is written.";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MockDirectives {
    pub sleep_ms: Option<u64>,
    pub ignore_out: bool,
    pub block: bool,
    pub exit: bool,
}

pub fn extract_response_path(text: &str) -> Option<PathBuf> {
    let start = text.find(PREAMBLE_PREFIX)? + PREAMBLE_PREFIX.len();
    let rest = &text[start..];
    let end = rest.find(PREAMBLE_SUFFIX)?;
    Some(PathBuf::from(&rest[..end]))
}

pub fn parse_directives(text: &str) -> MockDirectives {
    let mut directives = MockDirectives {
        ignore_out: text.contains("[[ignore-out]]"),
        block: text.contains("[[block]]"),
        exit: text.contains("[[exit]]"),
        sleep_ms: None,
    };

    let mut remaining = text;
    while let Some(start) = remaining.find("[[sleep:") {
        let after_start = &remaining[start + "[[sleep:".len()..];
        let Some(end) = after_start.find("]]") else {
            break;
        };
        if let Ok(ms) = after_start[..end].parse::<u64>() {
            directives.sleep_ms = Some(ms);
        }
        remaining = &after_start[end + 2..];
    }

    directives
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_response_path_from_preamble() {
        let path = extract_response_path(
            "prompt\nWhen you are completely finished, write your full final answer as markdown to the file: /tmp/run/001-response.md. Do not consider the task done until that file is written.",
        );
        assert_eq!(path, Some(PathBuf::from("/tmp/run/001-response.md")));
    }

    #[test]
    fn missing_preamble_returns_none() {
        assert_eq!(extract_response_path("no path"), None);
    }

    #[test]
    fn parses_directives() {
        let directives = parse_directives("a [[sleep:250]] b [[ignore-out]] [[block]] [[exit]]");
        assert_eq!(
            directives,
            MockDirectives {
                sleep_ms: Some(250),
                ignore_out: true,
                block: true,
                exit: true,
            }
        );
    }

    #[test]
    fn later_sleep_directive_wins() {
        assert_eq!(
            parse_directives("[[sleep:1]] [[sleep:2]]").sleep_ms,
            Some(2)
        );
    }
}
