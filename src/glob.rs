//! Shell-style wildcards: `*` for any run of characters and `?` for exactly one.

pub fn matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut retry: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                retry = Some((p + 1, t));
                p += 1;
            }
            Some(c) if *c == '?' || *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match retry {
                // Let the most recent star swallow one more character and try again.
                Some((after_star, consumed)) => {
                    p = after_star;
                    t = consumed + 1;
                    retry = Some((after_star, consumed + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn literal_text() {
        assert!(matches("abc", "abc"));
        assert!(!matches("abc", "abd"));
        assert!(!matches("abc", "abcd"));
        assert!(!matches("abcd", "abc"));
        assert!(matches("", ""));
        assert!(!matches("", "a"));
    }

    #[test]
    fn stars() {
        assert!(matches("*", ""));
        assert!(matches("*", "anything"));
        assert!(matches("a*", "abc"));
        assert!(matches("*c", "abc"));
        assert!(matches("a*c", "ac"));
        assert!(matches("a*c*e", "abcde"));
        assert!(matches("**a**", "a"));
        assert!(!matches("a*c", "abd"));
    }

    #[test]
    fn a_star_gives_back_what_it_took() {
        assert!(matches("*ab", "aab"));
        assert!(matches("a*b*c", "aXbXbXc"));
        assert!(!matches("*ab", "aba"));
    }

    #[test]
    fn question_marks_take_one_character() {
        assert!(matches("a?c", "abc"));
        assert!(matches("?", "é"));
        assert!(!matches("a?c", "ac"));
        assert!(!matches("?", ""));
    }

    #[test]
    fn long_inputs_stay_fast() {
        let text = "a".repeat(5000);
        assert!(!matches("*a*a*a*a*a*b", &text));
    }
}
