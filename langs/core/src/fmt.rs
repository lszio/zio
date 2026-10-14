//! The Zio source formatter.
//!
//! This is a layout pass over the token stream, not a re-evaluation:
//! every atom is reproduced exactly as it was read, so formatting never
//! changes what the source means. It preserves `;` comments — a
//! formatter that eats the comments a program's author left behind
//! would be a tool nobody runs twice — and it is idempotent:
//! `format(format(x)) == format(x)` is part of the contract tests.
//!
//! The rules are deliberately few:
//!
//! - a form prints on one line when it fits inside [`WIDTH`], otherwise
//!   its opener starts a line and every child gets its own line, two
//!   spaces deeper;
//! - map entries stay pairwise on one line (`:key value`), because a
//!   key separated from its value reads as two unrelated values;
//! - reader prefixes (`'`, `` ` ``, `~`, `~@`) hug their target;
//! - one blank line survives between top-level forms; blank lines
//!   inside a form do not;
//! - the file ends with exactly one newline.

use crate::error::EvalError;

/// The line a form may occupy before it is broken across lines.
const WIDTH: usize = 80;
/// One level of indentation.
const INDENT: usize = 2;

/// A parsed node, with the source line it started on. Comments are
/// attached to the forms they belong to — a leading comment owns its
/// own line before a form, a trailing comment shares the form's line —
/// so the printer never has to reason about a comment that arrived
/// between two siblings.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    kind: Kind,
    /// The first source line the form occupies, its leading comment
    /// included.
    line: usize,
    /// The last source line the form occupies, its closer's line for a
    /// compound. Blank-line decisions need both ends of a form.
    end_line: usize,
    leading: Vec<String>,
    trailing: Option<String>,
}

impl Node {
    fn new(kind: Kind, line: usize) -> Self {
        Node {
            kind,
            line,
            end_line: line,
            leading: Vec::new(),
            trailing: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// A symbol, number, keyword, boolean, char or string literal.
    Atom(String),
    /// `(...)`, `[...]`, `{...}`, `#{...}` or `#(...)`, with children.
    Compound {
        open: String,
        close: String,
        children: Vec<Node>,
    },
    /// `'x`, `` `x ``, `~x` or `~@x`, with its target.
    Reader { prefix: String, target: Box<Node> },
    /// A comment with no form after it: free-floating to the end of
    /// the source, printed on its own line.
    Comment { text: String },
    /// The slot a reader prefix fills with its target. It never
    /// survives parsing.
    Empty,
}

/// One lexed token: its text, its line, and whether it starts its line.
struct Token {
    text: String,
    line: usize,
    standalone: bool,
}

/// A formatter error: unbalanced input is reported with its line, the
/// same courtesy the reader gives a parse error.
fn fmt_error(line: usize, message: &str) -> EvalError {
    EvalError::custom(format!("fmt: line {line}: {message}"))
}

fn tokenize(source: &str) -> Result<Vec<Token>, EvalError> {
    let mut tokens = Vec::new();
    let mut line = 1usize;
    // A token is standalone when nothing but whitespace came before it
    // on its line. `seen_on_line` tracks whether this line produced one.
    let mut seen_on_line = false;
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0usize;

    while index < chars.len() {
        let c = chars[index];
        if c == '\n' {
            line += 1;
            seen_on_line = false;
            index += 1;
            continue;
        }
        if c.is_whitespace() {
            index += 1;
            continue;
        }
        let start_line = line;
        let standalone = !seen_on_line;
        let (text, next) = match c {
            ';' => {
                let mut text = String::new();
                let mut cursor = index;
                while let Some(&current) = chars.get(cursor) {
                    if current == '\n' {
                        break;
                    }
                    text.push(current);
                    cursor += 1;
                }
                (text.trim_end().to_string(), cursor)
            }
            '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '`' => (c.to_string(), index + 1),
            '~' => {
                if chars.get(index + 1) == Some(&'@') {
                    ("~@".to_string(), index + 2)
                } else {
                    ("~".to_string(), index + 1)
                }
            }
            '#' => match chars.get(index + 1) {
                // `#{` opens a set literal, `#(` an anonymous-function
                // form; both are printed as written.
                Some('{') => ("#{".to_string(), index + 2),
                Some('(') => ("#(".to_string(), index + 2),
                // `#\c` is a char literal: one token, so the printer
                // keeps the dispatch next to its payload.
                Some('\\') => char_literal(&chars, index),
                _ => atom_at(&chars, index),
            },
            '"' => {
                let mut text = String::new();
                text.push('"');
                let mut cursor = index + 1;
                loop {
                    match chars.get(cursor) {
                        None => return Err(fmt_error(start_line, "unterminated string")),
                        Some('\\') => {
                            text.push('\\');
                            cursor += 1;
                            if let Some(&escaped) = chars.get(cursor) {
                                text.push(escaped);
                                cursor += 1;
                            }
                        }
                        Some('"') => {
                            text.push('"');
                            cursor += 1;
                            break;
                        }
                        Some(&other) => {
                            text.push(other);
                            cursor += 1;
                        }
                    }
                }
                (text, cursor)
            }
            '\\' => char_literal(&chars, index),
            _ => atom_at(&chars, index),
        };
        tokens.push(Token {
            text,
            line: start_line,
            standalone,
        });
        seen_on_line = true;
        index = next.max(index + 1);
    }

    Ok(tokens)
}

/// A character literal with its leading dispatch: `#\c`, `#\space`, or
/// `\c`. The payload runs to the next token boundary, exactly as the
/// reader's lexer reads it.
fn char_literal(chars: &[char], index: usize) -> (String, usize) {
    let mut atom = String::new();
    let mut next = index;
    if chars[next] == '#' {
        atom.push('#');
        next += 1;
    }
    // The backslash itself.
    atom.push('\\');
    next += 1;
    if let Some(&first) = chars.get(next) {
        atom.push(first);
        next += 1;
        if !first.is_whitespace() && !"()[]{}'\";".contains(first) {
            while let Some(&c) = chars.get(next) {
                if c.is_whitespace() || "()[]{}'\";".contains(c) {
                    break;
                }
                atom.push(c);
                next += 1;
            }
        }
    }
    (atom, next)
}

/// Read a bare atom from `index`, stopping at the same characters the
/// reader's lexer stops at.
fn atom_at(chars: &[char], index: usize) -> (String, usize) {
    let mut atom = String::new();
    let mut next = index;
    while let Some(&c) = chars.get(next) {
        if c.is_whitespace() || "()[]{}\"'#;".contains(c) {
            break;
        }
        atom.push(c);
        next += 1;
    }
    if atom.is_empty() {
        // The caller only lands here for a real atom; a lone sharp
        // that produced nothing still consumes its character.
        atom.push(chars[index]);
        next = index + 1;
    }
    (atom, next)
}

/// Parse a token stream into top-level forms. Delimiters resolve as the
/// reader's do: `()` `[]` `{}`, with `#{` closing on `}` and `#(` on `)`.
fn parse(tokens: &[Token]) -> Result<Vec<Node>, EvalError> {
    let mut top: Vec<Node> = Vec::new();
    // Open nodes currently accepting children: compounds, and reader
    // prefixes until their target arrives.
    let mut stack: Vec<Node> = Vec::new();
    // Standalone comments waiting for the form they precede, with the
    // line each was written on.
    let mut pending: Vec<(usize, String)> = Vec::new();

    for token in tokens {
        match token.text.as_str() {
            "(" | "[" | "{" | "#{" | "#(" => {
                let mut node = Node::new(
                    Kind::Compound {
                        open: token.text.clone(),
                        close: String::new(),
                        children: Vec::new(),
                    },
                    token.line,
                );
                attach_pending(&mut node, &mut pending);
                stack.push(node);
            }
            ")" | "]" | "}" => {
                let Some(mut open) = stack.pop() else {
                    return Err(fmt_error(token.line, "unmatched closing delimiter"));
                };
                let Kind::Compound {
                    open: open_text,
                    children,
                    ..
                } = open.kind
                else {
                    unreachable!("the stack only holds compounds and reader prefixes");
                };
                if token.text != closer_for(&open_text) {
                    return Err(fmt_error(
                        token.line,
                        &format!(
                            "delimiter mismatch: {open_text} was closed by {}",
                            token.text
                        ),
                    ));
                }
                open.end_line = token.line;
                open.kind = Kind::Compound {
                    open: open_text,
                    close: token.text.clone(),
                    children,
                };
                push_child(&mut stack, &mut top, open);
            }
            "'" | "`" | "~" | "~@" => {
                let mut node = Node::new(
                    Kind::Reader {
                        prefix: token.text.clone(),
                        target: Box::new(Node::new(Kind::Empty, token.line)),
                    },
                    token.line,
                );
                attach_pending(&mut node, &mut pending);
                stack.push(node);
            }
            text if text.starts_with(';') && token.standalone => {
                pending.push((token.line, text.to_string()));
            }
            _ => {
                let node = if token.text.starts_with(';') {
                    Node::new(
                        Kind::Comment {
                            text: token.text.clone(),
                        },
                        token.line,
                    )
                } else {
                    let mut node = Node::new(Kind::Atom(token.text.clone()), token.line);
                    attach_pending(&mut node, &mut pending);
                    node
                };
                push_child(&mut stack, &mut top, node);
            }
        }
    }

    if let Some(open) = stack.last() {
        let (opener, line) = match &open.kind {
            Kind::Compound {
                open: open_text, ..
            } => (open_text.clone(), open.line),
            Kind::Reader { prefix, .. } => (prefix.clone(), open.line),
            _ => unreachable!("the stack only holds compounds and reader prefixes"),
        };
        if "([{#".contains(opener.as_str()) {
            return Err(fmt_error(
                line,
                &format!("unclosed {opener} at end of input"),
            ));
        }
        return Err(fmt_error(line, &format!("dangling reader prefix {opener}")));
    }

    // Comments with no form after them travel at the end of the source.
    for (_, text) in pending {
        top.push(Node::new(Kind::Comment { text }, 0));
    }

    Ok(top)
}

/// Move the comments waiting for a form onto that form. The form's
/// recorded line becomes the first line it occupies, so a blank line in
/// the source before the comment is not doubled by the layout.
fn attach_pending(node: &mut Node, pending: &mut Vec<(usize, String)>) {
    if pending.is_empty() {
        return;
    }
    let first_line = pending[0].0;
    if first_line < node.line {
        node.line = first_line;
    }
    node.leading = pending.drain(..).map(|(_, text)| text).collect();
}

fn closer_for(open: &str) -> &'static str {
    match open {
        "(" | "#(" => ")",
        "[" => "]",
        "{" | "#{" => "}",
        _ => unreachable!("only the four openers above open a form"),
    }
}

/// Attach a finished node to its parent, or to the top level. A form
/// whose parent is a reader prefix becomes that prefix's target and
/// completes it. A comment attaches to the form it follows on the line:
/// the inner compound's last form, the compound itself when it has
/// none yet, or the last top-level form.
fn push_child(stack: &mut Vec<Node>, top: &mut Vec<Node>, node: Node) {
    let Some(parent) = stack.last_mut() else {
        if let Kind::Comment { text } = &node.kind {
            if let Some(last) = top.last_mut() {
                last.trailing = Some(text.clone());
                return;
            }
        }
        top.push(node);
        return;
    };
    if let Kind::Reader { target, .. } = &mut parent.kind {
        if matches!(target.kind, Kind::Empty) {
            if let Kind::Comment { text } = &node.kind {
                // `' ; why\n x`: the note stays with the prefix, on the
                // line the prefix is printed, and the target keeps its
                // own line.
                parent.leading.push(text.clone());
                return;
            }
            target.line = node.line;
            parent.end_line = node.end_line;
            **target = node;
            let finished = stack.pop().expect("the reader prefix is on the stack");
            push_child(stack, top, finished);
            return;
        }
        unreachable!("a reader prefix takes one target");
    }
    if let Kind::Comment { text } = &node.kind {
        if let Kind::Compound { children, .. } = &mut parent.kind {
            if let Some(last) = children.last_mut() {
                last.trailing = Some(text.clone());
                return;
            }
        }
        // A comment before a compound's first form has no form to
        // trail yet: it leads the compound, on its own line.
        parent.leading.push(text.clone());
        return;
    }
    if let Kind::Compound { children, .. } = &mut parent.kind {
        children.push(node);
    }
}

struct Printer {
    out: String,
    col: usize,
    at_line_start: bool,
}

impl Printer {
    fn new() -> Self {
        Printer {
            out: String::new(),
            col: 0,
            at_line_start: true,
        }
    }

    fn push(&mut self, text: &str) {
        self.out.push_str(text);
        self.col += text.chars().count();
        self.at_line_start = false;
    }

    fn newline(&mut self, indent: usize) {
        self.out.push('\n');
        self.out.push_str(&" ".repeat(indent));
        self.col = indent;
        self.at_line_start = true;
    }

    /// Print a form at `indent`: the comments it carries, then the form,
    /// then the comment that trails it.
    fn node(&mut self, node: &Node, indent: usize) {
        for comment in &node.leading {
            if !self.at_line_start {
                self.newline(indent);
            }
            self.push(comment);
        }
        if !node.leading.is_empty() && !self.at_line_start {
            // The comments own their lines; the form starts a new one.
            self.newline(indent);
        }
        match &node.kind {
            Kind::Atom(text) | Kind::Comment { text } => self.push(text),
            Kind::Empty => {}
            Kind::Reader { prefix, target } => {
                self.push(prefix);
                self.node(target, indent);
            }
            Kind::Compound {
                open,
                close,
                children,
            } => {
                let mut inlined = false;
                // A subtree carrying comments cannot be one line: a
                // comment inside a form would be stranded.
                if !forbids_inline(node) {
                    if let Some(inline) = inline_render(open, children, close) {
                        if self.col + inline.chars().count() <= WIDTH {
                            self.push(&inline);
                            inlined = true;
                        }
                    }
                }
                if !inlined {
                    self.compound(open, close, children, indent);
                }
            }
        }
        if let Some(text) = &node.trailing {
            self.push(" ");
            self.push(text);
        }
    }

    /// The broken layout: the opener is followed by its children in
    /// fill order — each child is appended to the current line while it
    /// fits, and the line wraps at the child indent when it does not.
    /// That keeps a signature on the opener line (`(defn name [args]`
    /// …) and gives every wrapped line the same indentation, instead of
    /// a ragged one-form-per-line column.
    fn compound(&mut self, open: &str, close: &str, children: &[Node], indent: usize) {
        self.push(open);
        let child_indent = indent + INDENT;
        // Map entries print as `:key value` on one line, so a key and
        // its value are placed together.
        let paired = open == "{";
        let mut force_wrap = false;
        let mut index = 0usize;
        let mut first = true;
        while index < children.len() {
            let child = &children[index];
            // A map's key and its value are placed together even when
            // the value carries a trailing comment: splitting the pair
            // across a line break separates a key from its value, which
            // reads as two unrelated values.
            let mut value: Option<&Node> = None;
            if paired
                && index + 1 < children.len()
                && !matches!(children[index + 1].kind, Kind::Comment { .. })
            {
                value = Some(&children[index + 1]);
            }
            let mut width = inline_node(child).chars().count();
            if let Some(value) = value {
                width += 1 + inline_node(value).chars().count();
            }
            // A child carrying leading comments always starts a line of
            // its own: placed after a separator, the comments would
            // trail it and swallow the previous sibling's line.
            let carries_comments =
                !child.leading.is_empty() || value.is_some_and(|v| !v.leading.is_empty());
            let fits = self.col + 1 + width <= WIDTH;
            if force_wrap || carries_comments || !fits {
                self.newline(child_indent);
            } else if !first {
                self.push(" ");
            }
            self.node(child, child_indent);
            if let Some(value) = value {
                self.push(" ");
                self.node(value, child_indent);
                index += 1;
            }
            first = false;
            // A trailing comment ends the line it is on, so the next
            // child cannot be appended after it.
            force_wrap =
                child.trailing.is_some() || value.is_some_and(|value| value.trailing.is_some());
            index += 1;
        }
        // A compound that closes a form stays on that form's last line
        // — `(defn f [x]\n  (body)\n)` would read as two forms — but a
        // plain atom's closer takes a line of its own, and a trailing
        // comment always owns the end of its line.
        let hug = children.last().is_some_and(|last| {
            matches!(last.kind, Kind::Compound { .. }) && last.trailing.is_none()
        });
        if hug {
            self.push(close);
        } else {
            self.newline(indent);
            self.push(close);
        }
    }
}

impl Node {
    /// Whether this node is free of comments — its own or any in its
    /// subtree, so the one-line layout cannot strand one.
    fn comment_free(&self) -> bool {
        if matches!(self.kind, Kind::Comment { .. }) {
            return false;
        }
        self.leading.is_empty() && self.trailing.is_none()
    }
}

/// Whether a form's DESCENDANTS carry a comment that forbids the
/// one-line layout. Comments the form carries itself are fine — a
/// leading one owns a line before the form and a trailing one follows
/// it — but a comment inside a nested form cannot be printed on one
/// line.
fn forbids_inline(node: &Node) -> bool {
    match &node.kind {
        Kind::Compound { children, .. } => children
            .iter()
            .any(|child| !child.comment_free() || forbids_inline(child)),
        Kind::Reader { target, .. } => !target.comment_free() || forbids_inline(target),
        Kind::Atom(_) | Kind::Comment { .. } | Kind::Empty => false,
    }
}

/// Render a form with no comments of its own on one line.
fn inline_render(open: &str, children: &[Node], close: &str) -> Option<String> {
    let mut text = String::from(open);
    let paired = open == "{";
    let mut index = 0usize;
    let mut wrote = false;
    while index < children.len() {
        let child = &children[index];
        if !child.comment_free() {
            return None;
        }
        if wrote {
            text.push(' ');
        }
        text.push_str(&inline_node(child));
        wrote = true;
        index += 1;
        if paired {
            if let Some(value) = children.get(index) {
                if !matches!(value.kind, Kind::Comment { .. }) {
                    text.push(' ');
                    text.push_str(&inline_node(value));
                    index += 1;
                }
            }
        }
    }
    text.push_str(close);
    Some(text)
}

/// The one-line text of a node. Compounds render their children
/// recursively, which is only correct when the whole form fits — the
/// caller checks the width first.
fn inline_node(node: &Node) -> String {
    match &node.kind {
        Kind::Atom(text) => text.clone(),
        Kind::Comment { text } => text.clone(),
        Kind::Empty => String::new(),
        Kind::Reader { prefix, target } => format!("{prefix}{}", inline_node(target)),
        Kind::Compound {
            open,
            close,
            children,
        } => match inline_render(open, children, close) {
            Some(text) => text,
            None => format!("{open}{close}"),
        },
    }
}

/// Format Zio source. The result is idempotent and ends in a newline; a
/// source with no forms formats to the empty string, so checking an
/// empty file is stable.
pub fn format_source(source: &str) -> Result<String, EvalError> {
    let tokens = tokenize(source)?;
    let forms = parse(&tokens)?;
    let mut printer = Printer::new();

    for (index, form) in forms.iter().enumerate() {
        if index > 0 {
            printer.newline(0);
            // One blank line survives between top-level forms; a wider
            // gap in the source collapses to it. The gap is measured
            // between where the previous form ended and where this one
            // begins, so a form broken across lines does not invent a
            // blank after itself.
            if forms[index - 1].end_line + 1 < form.line {
                printer.newline(0);
            }
        }
        printer.node(form, 0);
    }

    let mut text = printer.out;
    while text.ends_with('\n') {
        text.pop();
    }
    if text.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("{text}\n"))
}

/// Whether `source` is already formatted.
pub fn is_formatted(source: &str) -> Result<bool, EvalError> {
    Ok(format_source(source)? == source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_forms_stay_on_one_line() {
        assert_eq!(format_source("(a b)").unwrap(), "(a b)\n");
        assert_eq!(format_source("[1 2 3]").unwrap(), "[1 2 3]\n");
        assert_eq!(format_source("{:a 1 :b 2}").unwrap(), "{:a 1 :b 2}\n");
        assert_eq!(format_source("hello").unwrap(), "hello\n");
    }

    #[test]
    fn long_forms_break_one_child_per_line() {
        let source = "(defn long-name [argument-one argument-two argument-three] (+ argument-one argument-two argument-three))";
        let formatted = format_source(source).unwrap();
        assert_eq!(
            formatted,
            "(defn long-name [argument-one argument-two argument-three]\n  (+ argument-one argument-two argument-three))\n"
        );
        // Idempotent: a second pass changes nothing.
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn nested_breaks_indent_two_spaces() {
        let source = "(outer (a-very-long-inner-form-name that clearly does not fit on one single line at all ok) tail)";
        let formatted = format_source(source).unwrap();
        assert_eq!(
            formatted,
            "(outer\n  (a-very-long-inner-form-name that clearly does not fit on one single line at\n    all ok\n  ) tail\n)\n"
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn map_entries_keep_their_pairs_when_broken() {
        let source = "{:alpha-one 11 :bravo-two 22 :charlie-three 33 :delta-four 44 :echo-five 55 :foxtrot-six 66}";
        let formatted = format_source(source).unwrap();
        assert_eq!(
            formatted,
            "{:alpha-one 11 :bravo-two 22 :charlie-three 33 :delta-four 44 :echo-five 55\n  :foxtrot-six 66\n}\n"
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn comments_survive_formatting() {
        let source =
            "; leading note\n(a b) ; trailing note\n; standalone between\n(c d)\n; at the end\n";
        let formatted = format_source(source).unwrap();
        assert!(formatted.contains("; leading note\n"), "{formatted}");
        assert!(formatted.contains("(a b) ; trailing note\n"), "{formatted}");
        assert!(formatted.contains("; standalone between\n"), "{formatted}");
        assert!(formatted.contains("; at the end\n"), "{formatted}");
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn a_comment_inside_a_form_forces_the_break() {
        let source = "(a ; why\n b)";
        let formatted = format_source(source).unwrap();
        assert!(formatted.contains("; why"), "{formatted}");
        assert!(formatted.contains("b"), "{formatted}");
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn reader_prefixes_hug_their_target() {
        assert_eq!(format_source("'(a b)").unwrap(), "'(a b)\n");
        assert_eq!(format_source("`(a ,b)").unwrap(), "`(a ,b)\n");
        assert_eq!(format_source("~x").unwrap(), "~x\n");
        assert_eq!(format_source("~@xs").unwrap(), "~@xs\n");
    }

    #[test]
    fn sets_and_chars_are_printed_as_written() {
        assert_eq!(format_source("#{a b c}").unwrap(), "#{a b c}\n");
        assert_eq!(format_source(r#""a string""#).unwrap(), "\"a string\"\n");
        assert_eq!(format_source(r#""tab\there""#).unwrap(), "\"tab\\there\"\n");
    }

    #[test]
    fn one_blank_line_survives_between_top_level_forms() {
        let formatted = format_source("(a)\n\n\n\n(b)\n(c)").unwrap();
        assert_eq!(formatted, "(a)\n\n(b)\n(c)\n");
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn an_empty_source_stays_empty() {
        assert_eq!(format_source("").unwrap(), "");
        assert_eq!(format_source("\n\n  \n").unwrap(), "");
        assert!(is_formatted("").unwrap());
    }

    #[test]
    fn only_comments_are_kept() {
        assert_eq!(format_source("; just a note\n").unwrap(), "; just a note\n");
    }

    #[test]
    fn unbalanced_input_is_reported_with_a_line() {
        let err = format_source("(a\n) ]").unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        let err = format_source("(a (b)").unwrap_err().to_string();
        assert!(err.contains("line 1"), "{err}");
        let err = format_source("(a\n\"unterminated").unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        let err = format_source("(a [b)").unwrap_err().to_string();
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn repository_sources_are_idempotent() {
        // The formatter must be stable on the real library corpus: running
        // it over the shipped sources cannot move them onto a fixed point
        // that a second pass then moves again.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("zio-core lives at <root>/langs/core")
            .to_path_buf();
        let mut checked = 0usize;
        for dir in ["libs", "examples", "langs/core/assets"] {
            let start = root.join(dir);
            if !start.is_dir() {
                continue;
            }
            let mut stack = vec![start];
            while let Some(current) = stack.pop() {
                let entries = match std::fs::read_dir(&current) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().and_then(|e| e.to_str()) == Some("zio") {
                        let source = std::fs::read_to_string(&path).unwrap();
                        let once = format_source(&source)
                            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                        let twice = format_source(&once)
                            .unwrap_or_else(|e| panic!("reformat {}: {e}", path.display()));
                        assert_eq!(once, twice, "not idempotent: {}", path.display());
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 0, "the corpus walk found no Zio sources");
    }
}
