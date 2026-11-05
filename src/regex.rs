//! A small regular expression engine for the `~` operator, written as a Thompson NFA simulation.
//! Every pattern runs in time proportional to pattern size times text length, at the price of
//! leaving out backreferences and lookaround. There are no captures, because all a query asks
//! is whether the pattern occurs in the text.
//!
//! Supported: literals, `.`, `[...]` and `[^...]` with ranges, `\d \w \s \D \W \S \b`, `^ $`,
//! `* + ?`, `{n} {n,} {n,m}`, `|`, `(...)`, `(?:...)` and a leading `(?i)`.

use std::fmt;

const MAX_PROGRAM: usize = 20_000;
const MAX_NESTING: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    pub position: usize,
    pub message: String,
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at character {}", self.message, self.position)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CharSet {
    ranges: Vec<(char, char)>,
    negated: bool,
}

impl CharSet {
    fn contains(&self, c: char) -> bool {
        self.ranges.iter().any(|(low, high)| *low <= c && c <= *high) != self.negated
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Empty,
    Char(char),
    Any,
    Set(CharSet),
    Start,
    End,
    WordBoundary,
    Concat(Vec<Node>),
    Alternate(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32> },
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn error<T>(&self, message: &str) -> Result<T, RegexError> {
        Err(RegexError { position: self.at, message: message.to_string() })
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn alternation(&mut self, depth: usize) -> Result<Node, RegexError> {
        if depth > MAX_NESTING {
            return self.error("groups nested too deeply");
        }
        let mut branches = vec![self.concat(depth)?];
        while self.eat('|') {
            branches.push(self.concat(depth)?);
        }
        Ok(if branches.len() == 1 { branches.remove(0) } else { Node::Alternate(branches) })
    }

    fn concat(&mut self, depth: usize) -> Result<Node, RegexError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom(depth)?;
            parts.push(self.repeat(atom)?);
        }
        Ok(match parts.len() {
            0 => Node::Empty,
            1 => parts.remove(0),
            _ => Node::Concat(parts),
        })
    }

    fn repeat(&mut self, mut atom: Node) -> Result<Node, RegexError> {
        while let Some(c) = self.peek() {
            let (min, max) = match c {
                '*' => (0, None),
                '+' => (1, None),
                '?' => (0, Some(1)),
                '{' => match self.counted()? {
                    Some(bounds) => bounds,
                    None => break,
                },
                _ => break,
            };
            if c != '{' {
                self.at += 1;
            }
            if matches!(atom, Node::Start | Node::End | Node::WordBoundary) {
                return self.error("nothing to repeat");
            }
            // Greediness makes no difference to whether a match exists, so `*?` means `*`.
            self.eat('?');
            atom = Node::Repeat { node: Box::new(atom), min, max };
        }
        Ok(atom)
    }

    /// Reads `{n}`, `{n,}` or `{n,m}`. Anything else starting with a brace is a literal brace,
    /// which the caller will come back to, so nothing is consumed then.
    fn counted(&mut self) -> Result<Option<(u32, Option<u32>)>, RegexError> {
        let close = match self.chars[self.at..].iter().position(|c| *c == '}') {
            Some(close) => self.at + close,
            None => return Ok(None),
        };
        let inside: String = self.chars[self.at + 1..close].iter().collect();
        let number = |text: &str| -> Option<u32> {
            if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) { text.parse().ok() } else { None }
        };
        let bounds = match inside.split_once(',') {
            None => number(&inside).map(|n| (n, Some(n))),
            Some((low, "")) => number(low).map(|n| (n, None)),
            Some((low, high)) => number(low).zip(number(high)).map(|(n, m)| (n, Some(m))),
        };
        let Some(bounds) = bounds else { return Ok(None) };
        if bounds.1.is_some_and(|max| max < bounds.0) {
            return self.error("repeat bounds are the wrong way round");
        }
        self.at = close + 1;
        Ok(Some(bounds))
    }

    fn atom(&mut self, depth: usize) -> Result<Node, RegexError> {
        let c = self.peek().expect("caller checked");
        self.at += 1;
        match c {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '[' => self.class(),
            '(' => {
                if self.eat('?') {
                    if !self.eat(':') {
                        return self.error("only (?:...) groups and a leading (?i) are supported");
                    }
                }
                let inner = self.alternation(depth + 1)?;
                if !self.eat(')') {
                    return self.error("missing )");
                }
                Ok(inner)
            }
            '*' | '+' | '?' => {
                self.at -= 1;
                self.error("nothing to repeat")
            }
            '\\' => self.escape(),
            literal => Ok(Node::Char(literal)),
        }
    }

    fn escape(&mut self) -> Result<Node, RegexError> {
        let Some(c) = self.peek() else {
            return self.error("pattern ends with a backslash");
        };
        self.at += 1;
        if c == 'b' {
            return Ok(Node::WordBoundary);
        }
        if let Some(set) = shorthand(c) {
            return Ok(Node::Set(set));
        }
        match escaped_char(c) {
            Some(literal) => Ok(Node::Char(literal)),
            None => self.error("unknown escape"),
        }
    }

    fn class(&mut self) -> Result<Node, RegexError> {
        let negated = self.eat('^');
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return self.error("missing ]");
            };
            self.at += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let low = match self.class_item(c)? {
                ClassItem::Char(low) => low,
                ClassItem::Set(set) => {
                    if set.negated {
                        ranges.extend(complement(&set.ranges));
                    } else {
                        ranges.extend(set.ranges);
                    }
                    continue;
                }
            };
            let is_range = self.peek() == Some('-') && self.chars.get(self.at + 1).is_some_and(|next| *next != ']');
            if !is_range {
                ranges.push((low, low));
                continue;
            }
            self.at += 1;
            let next = self.peek().expect("checked above");
            self.at += 1;
            match self.class_item(next)? {
                ClassItem::Char(high) if high >= low => ranges.push((low, high)),
                ClassItem::Char(_) => return self.error("range is the wrong way round"),
                ClassItem::Set(_) => return self.error("a range can't end in a character set"),
            }
        }
        Ok(Node::Set(CharSet { ranges, negated }))
    }

    fn class_item(&mut self, c: char) -> Result<ClassItem, RegexError> {
        if c != '\\' {
            return Ok(ClassItem::Char(c));
        }
        let Some(escaped) = self.peek() else {
            return self.error("missing ]");
        };
        self.at += 1;
        if let Some(set) = shorthand(escaped) {
            return Ok(ClassItem::Set(set));
        }
        match escaped_char(escaped) {
            Some(literal) => Ok(ClassItem::Char(literal)),
            None => self.error("unknown escape"),
        }
    }
}

enum ClassItem {
    Char(char),
    Set(CharSet),
}

fn escaped_char(c: char) -> Option<char> {
    match c {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        c if !c.is_alphanumeric() => Some(c),
        _ => None,
    }
}

fn shorthand(c: char) -> Option<CharSet> {
    let (ranges, negated) = match c {
        'd' => (vec![('0', '9')], false),
        'D' => (vec![('0', '9')], true),
        'w' => (word_ranges(), false),
        'W' => (word_ranges(), true),
        's' => (space_ranges(), false),
        'S' => (space_ranges(), true),
        _ => return None,
    };
    Some(CharSet { ranges, negated })
}

fn word_ranges() -> Vec<(char, char)> {
    vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')]
}

fn space_ranges() -> Vec<(char, char)> {
    vec![(' ', ' '), ('\t', '\r')]
}

/// The characters outside sorted, or at least sortable, ranges.
fn complement(ranges: &[(char, char)]) -> Vec<(char, char)> {
    let mut sorted = ranges.to_vec();
    sorted.sort();
    let mut out = Vec::new();
    let mut next = 0u32;
    let mut push_until = |end: u32, next: u32| {
        if next < end {
            // Skip the surrogate gap, where char::from_u32 has nothing to give.
            let low = char::from_u32(next).or(char::from_u32(0xE000));
            let high = char::from_u32(end - 1).or(char::from_u32(0xD7FF));
            if let (Some(low), Some(high)) = (low, high) {
                if low <= high {
                    out.push((low, high));
                }
            }
        }
    };
    for (low, high) in sorted {
        push_until(low as u32, next);
        next = next.max(high as u32 + 1);
    }
    push_until(char::MAX as u32 + 1, next);
    out
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Set(CharSet),
    Start,
    End,
    WordBoundary,
    Split(usize, usize),
    Jump(usize),
    Match,
}

struct Compiler {
    program: Vec<Inst>,
}

impl Compiler {
    fn push(&mut self, inst: Inst) -> Result<usize, RegexError> {
        if self.program.len() >= MAX_PROGRAM {
            return Err(RegexError { position: 0, message: "pattern is too large".to_string() });
        }
        self.program.push(inst);
        Ok(self.program.len() - 1)
    }

    fn node(&mut self, node: &Node) -> Result<(), RegexError> {
        match node {
            Node::Empty => {}
            Node::Char(c) => {
                self.push(Inst::Char(*c))?;
            }
            Node::Any => {
                self.push(Inst::Any)?;
            }
            Node::Set(set) => {
                self.push(Inst::Set(set.clone()))?;
            }
            Node::Start => {
                self.push(Inst::Start)?;
            }
            Node::End => {
                self.push(Inst::End)?;
            }
            Node::WordBoundary => {
                self.push(Inst::WordBoundary)?;
            }
            Node::Concat(parts) => {
                for part in parts {
                    self.node(part)?;
                }
            }
            Node::Alternate(branches) => self.alternate(branches)?,
            Node::Repeat { node, min, max } => self.repeat(node, *min, *max)?,
        }
        Ok(())
    }

    fn alternate(&mut self, branches: &[Node]) -> Result<(), RegexError> {
        let (last, rest) = branches.split_last().expect("at least two branches");
        let mut jumps = Vec::new();
        for branch in rest {
            let split = self.push(Inst::Split(0, 0))?;
            self.node(branch)?;
            jumps.push(self.push(Inst::Jump(0))?);
            self.program[split] = Inst::Split(split + 1, self.program.len());
        }
        self.node(last)?;
        let end = self.program.len();
        for jump in jumps {
            self.program[jump] = Inst::Jump(end);
        }
        Ok(())
    }

    fn repeat(&mut self, node: &Node, min: u32, max: Option<u32>) -> Result<(), RegexError> {
        for _ in 0..min {
            self.node(node)?;
        }
        match max {
            None => {
                let split = self.push(Inst::Split(0, 0))?;
                self.node(node)?;
                self.push(Inst::Jump(split))?;
                self.program[split] = Inst::Split(split + 1, self.program.len());
            }
            Some(max) => {
                let mut splits = Vec::new();
                for _ in min..max {
                    splits.push(self.push(Inst::Split(0, 0))?);
                    self.node(node)?;
                }
                let end = self.program.len();
                for split in splits {
                    self.program[split] = Inst::Split(split + 1, end);
                }
            }
        }
        Ok(())
    }
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    program: Vec<Inst>,
    ignore_case: bool,
    source: String,
}

impl PartialEq for Regex {
    fn eq(&self, other: &Regex) -> bool {
        self.source == other.source
    }
}

/// The threads alive at one position, each listed once, which is what bounds the running time.
struct ThreadList {
    threads: Vec<usize>,
    seen: Vec<u64>,
    generation: u64,
}

impl ThreadList {
    fn new(size: usize) -> ThreadList {
        ThreadList { threads: Vec::with_capacity(size), seen: vec![0; size], generation: 0 }
    }

    fn reset(&mut self) {
        self.threads.clear();
        self.generation += 1;
    }
}

struct Place {
    previous: Option<char>,
    next: Option<char>,
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, RegexError> {
        let (ignore_case, body) = match pattern.strip_prefix("(?i)") {
            Some(body) => (true, body),
            None => (false, pattern),
        };
        let mut parser = Parser { chars: body.chars().collect(), at: 0 };
        let tree = parser.alternation(0)?;
        if parser.peek().is_some() {
            return parser.error("unmatched )");
        }
        let mut compiler = Compiler { program: Vec::new() };
        compiler.node(&tree)?;
        compiler.push(Inst::Match)?;
        Ok(Regex { program: compiler.program, ignore_case, source: pattern.to_string() })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    fn char_matches(&self, expected: char, actual: char) -> bool {
        expected == actual || (self.ignore_case && fold(expected) == fold(actual))
    }

    fn set_matches(&self, set: &CharSet, actual: char) -> bool {
        if !self.ignore_case {
            return set.contains(actual);
        }
        let lower = actual.to_lowercase().next().unwrap_or(actual);
        let upper = actual.to_uppercase().next().unwrap_or(actual);
        if set.negated {
            set.contains(lower) && set.contains(upper)
        } else {
            set.contains(lower) || set.contains(upper)
        }
    }

    /// Follows every path that consumes no text. Returns true when one reaches the end state.
    fn add(&self, list: &mut ThreadList, stack: &mut Vec<usize>, start: usize, place: &Place) -> bool {
        let mut matched = false;
        stack.push(start);
        while let Some(pc) = stack.pop() {
            if list.seen[pc] == list.generation {
                continue;
            }
            list.seen[pc] = list.generation;
            match &self.program[pc] {
                Inst::Jump(target) => stack.push(*target),
                Inst::Split(first, second) => {
                    stack.push(*second);
                    stack.push(*first);
                }
                Inst::Start => {
                    if place.previous.is_none() {
                        stack.push(pc + 1);
                    }
                }
                Inst::End => {
                    if place.next.is_none() {
                        stack.push(pc + 1);
                    }
                }
                Inst::WordBoundary => {
                    if is_word(place.previous) != is_word(place.next) {
                        stack.push(pc + 1);
                    }
                }
                Inst::Match => matched = true,
                _ => list.threads.push(pc),
            }
        }
        matched
    }

    /// Whether the pattern occurs anywhere in the text.
    pub fn is_match(&self, text: &str) -> bool {
        let mut current = ThreadList::new(self.program.len());
        let mut next = ThreadList::new(self.program.len());
        current.generation = 1;
        let mut stack = Vec::new();
        let mut previous = None;
        let mut rest = text.chars().peekable();
        loop {
            let upcoming = rest.peek().copied();
            let here = Place { previous, next: upcoming };
            // A new attempt starts at every position, which is what makes the search unanchored.
            if self.add(&mut current, &mut stack, 0, &here) {
                return true;
            }
            let Some(c) = rest.next() else { return false };
            let after = Place { previous: Some(c), next: rest.peek().copied() };
            next.reset();
            let mut matched = false;
            for index in 0..current.threads.len() {
                let pc = current.threads[index];
                let advances = match &self.program[pc] {
                    Inst::Char(expected) => self.char_matches(*expected, c),
                    Inst::Any => c != '\n',
                    Inst::Set(set) => self.set_matches(set, c),
                    _ => false,
                };
                if advances {
                    matched |= self.add(&mut next, &mut stack, pc + 1, &after);
                }
            }
            if matched {
                return true;
            }
            std::mem::swap(&mut current, &mut next);
            previous = Some(c);
        }
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(pattern: &str, text: &str) -> bool {
        Regex::new(pattern).unwrap().is_match(text)
    }

    #[test]
    fn literals_match_anywhere() {
        assert!(found("cat", "concatenate"));
        assert!(!found("cat", "dog"));
        assert!(found("", "anything"));
        assert!(found("", ""));
    }

    #[test]
    fn anchors() {
        assert!(found("^ab", "abc"));
        assert!(!found("^b", "abc"));
        assert!(found("bc$", "abc"));
        assert!(!found("b$", "abc"));
        assert!(found("^$", ""));
        assert!(!found("^$", "x"));
        assert!(found("^abc$", "abc"));
    }

    #[test]
    fn dot_stops_at_a_newline() {
        assert!(found("a.c", "abc"));
        assert!(!found("a.c", "a\nc"));
        assert!(found("^.$", "é"));
    }

    #[test]
    fn repeats() {
        assert!(found("^ab*c$", "ac"));
        assert!(found("^ab*c$", "abbbc"));
        assert!(found("^ab+c$", "abc"));
        assert!(!found("^ab+c$", "ac"));
        assert!(found("^ab?c$", "ac"));
        assert!(!found("^ab?c$", "abbc"));
        assert!(found("^a*?b$", "aab"));
    }

    #[test]
    fn counted_repeats() {
        assert!(found("^a{2}$", "aa"));
        assert!(!found("^a{2}$", "aaa"));
        assert!(found("^a{2,}$", "aaaa"));
        assert!(!found("^a{2,}$", "a"));
        assert!(found("^a{1,3}$", "aaa"));
        assert!(!found("^a{1,3}$", "aaaa"));
        assert!(found("^a{0}b$", "b"));
        assert!(found("^(ab){2}$", "abab"));
    }

    #[test]
    fn a_brace_that_is_not_a_count_is_literal() {
        assert!(found("^a{b}$", "a{b}"));
        assert!(found("^{$", "{"));
        assert!(found("^a{,2}$", "a{,2}"));
    }

    #[test]
    fn alternation_and_groups() {
        assert!(found("^(cat|dog)s?$", "dogs"));
        assert!(!found("^(cat|dog)s?$", "bird"));
        assert!(found("^(?:a|b|c)+$", "abcab"));
        assert!(found("^(|x)y$", "y"));
        assert!(found("a|", "zzz"));
    }

    #[test]
    fn classes() {
        assert!(found("^[abc]+$", "cab"));
        assert!(!found("^[abc]+$", "cad"));
        assert!(found("^[a-c0-2]+$", "a1c2"));
        assert!(found("^[^a-c]$", "z"));
        assert!(!found("^[^a-c]$", "b"));
        assert!(found("^[]a]+$", "]a"));
        assert!(found("^[a-]+$", "a-"));
        assert!(found(r"^[\]\-]+$", "]-"));
    }

    #[test]
    fn shorthands() {
        assert!(found(r"^\d+$", "123"));
        assert!(!found(r"^\d+$", "12a"));
        assert!(found(r"^\D+$", "abc"));
        assert!(found(r"^\w+$", "a_1"));
        assert!(found(r"^\W$", "-"));
        assert!(found(r"^\s$", "\t"));
        assert!(found(r"^\S$", "x"));
        assert!(found(r"^[\d.]+$", "1.25"));
        assert!(found(r"^[\D]+$", "ab"));
        assert!(!found(r"^[\D]+$", "a1"));
        assert!(found(r"^[^\d]$", "x"));
    }

    #[test]
    fn word_boundaries() {
        assert!(found(r"\bcat\b", "the cat sat"));
        assert!(!found(r"\bcat\b", "concatenate"));
        assert!(found(r"\b", "x"));
        assert!(!found(r"\b", " "));
    }

    #[test]
    fn escaped_metacharacters() {
        assert!(found(r"^a\.b$", "a.b"));
        assert!(!found(r"^a\.b$", "axb"));
        assert!(found(r"^\(\)\[\]\{\}\*\+\?\|\^\$\\$", "()[]{}*+?|^$\\"));
        assert!(found(r"^\n\t$", "\n\t"));
    }

    #[test]
    fn ignoring_case() {
        assert!(found("(?i)error", "An ERROR here"));
        assert!(found("(?i)^[a-c]+$", "ABC"));
        assert!(!found("(?i)^[^a-c]$", "B"));
        assert!(found("(?i)é", "É"));
        assert!(!found("error", "ERROR"));
    }

    #[test]
    fn bad_patterns_are_reported() {
        for pattern in [
            "(", ")", "a)", "[", "[a", "*", "+a", "?", "a|*", r"\", r"\q", "[z-a]", "a{3,1}", "(?x)", "^*", "[a-\\d]",
        ] {
            assert!(Regex::new(pattern).is_err(), "{pattern}");
        }
        assert_eq!(Regex::new("ab(").unwrap_err().position, 3);
    }

    #[test]
    fn huge_expansions_are_refused() {
        assert!(Regex::new("(a{1000}){1000}").is_err());
    }

    #[test]
    fn nesting_is_limited() {
        let pattern = format!("{}a{}", "(".repeat(100), ")".repeat(100));
        assert!(Regex::new(&pattern).is_err());
    }

    #[test]
    fn nasty_patterns_stay_linear() {
        let text = "a".repeat(2000);
        assert!(!found("(a*)*(a|aa)*b", &text));
        assert!(!found("^(a+)+$", &format!("{text}!")));
    }

    #[test]
    fn repeating_something_empty_terminates() {
        assert!(found("^(a*)*$", "aaa"));
        assert!(found("(|a)+", "b"));
    }
}
