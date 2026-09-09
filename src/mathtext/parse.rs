//! The recursive-descent LaTeX parser (internal/mathtext/parse.go): [`Node`] is the AST the layout
//! consumes; [`parse`] is the entry point. Grammar and every quirk = spec `mathtext.md` §3.
//!
//! The AST shape and the scanner → parser descent are modelled on the go-latex/latex project
//! (BSD-3-Clause, archived) exactly as Go's `parse.go:16-21` records; no upstream code is used.
//!
//! Go's `nil` Node (an absent optional) maps to `Option` on `Sqrt.index` / `BigOp.lower` /
//! `BigOp.upper`, and to an EMPTY `Seq` where Go stores a nil in a non-optional slot (a required
//! argument that was a dropped layout macro, e.g. `\hat\quad`): both lay out to the empty picture,
//! so the rendering is identical.

use std::fmt;

use crate::mathtext::symbols::{AccentKind, MathStyle, apply_math_font};
use crate::mathtext::{MAX_MATH_INPUT_RUNES, MAX_PARSE_DEPTH, macros, symbols};

/// The math AST (parse.go:37-150).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    /// A horizontal sequence.
    Seq(Vec<Node>),
    /// One glyph or identifier run.
    Atom(String),
    /// Literal text (`\text{…}`).
    Text(String),
    /// `\frac{num}{den}`.
    Frac {
        /// Numerator.
        num: Box<Node>,
        /// Denominator.
        den: Box<Node>,
    },
    /// `\sqrt[index]{radicand}`.
    Sqrt {
        /// The radicand.
        radicand: Box<Node>,
        /// The optional index.
        index: Option<Box<Node>>,
    },
    /// `base^exp`.
    Sup {
        /// The base.
        base: Box<Node>,
        /// The exponent.
        exp: Box<Node>,
    },
    /// `base_sub`.
    Sub {
        /// The base.
        base: Box<Node>,
        /// The subscript.
        sub: Box<Node>,
    },
    /// `base^sup_sub` in either order.
    SupSub {
        /// The base.
        base: Box<Node>,
        /// The superscript.
        sup: Box<Node>,
        /// The subscript.
        sub: Box<Node>,
    },
    /// `\left … \right` (or plain bracket pair) around `inner`.
    Delim {
        /// The opening glyph (may be empty for `.`).
        left: String,
        /// The closing glyph (may be empty for `.`).
        right: String,
        /// The enclosed expression.
        inner: Box<Node>,
    },
    /// A big operator with optional limits (`\sum_{…}^{…}`, `\lim_{…}`).
    BigOp {
        /// The operator family.
        op: OpFamily,
        /// The single-glyph form (`∑`, `∫`, …; empty for word operators).
        glyph: &'static str,
        /// The word form (`lim`, `max`, …; empty for glyph operators).
        word: &'static str,
        /// The lower limit.
        lower: Option<Box<Node>>,
        /// The upper limit.
        upper: Option<Box<Node>>,
    },
    /// A matrix/cases/aligned environment.
    Matrix {
        /// The environment name.
        env: String,
        /// Rows of cells.
        rows: Vec<Vec<Node>>,
    },
    /// An accent over `base`.
    Accent {
        /// The accent kind.
        kind: AccentKind,
        /// The accented expression.
        base: Box<Node>,
    },
}

/// Big-operator family (macros.go:17-27 `bigOpKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpFamily {
    /// `\sum`-like (∑, ∏, ⋃, …).
    Sum,
    /// `\prod`-like.
    Prod,
    /// `\int`-like (∫, ∬, ∮, …; the tall form).
    Int,
    /// Word operators (`\lim`, `\max`, `\sup`, …).
    Lim,
}

/// The parser's refusal (parse.go:35 `ErrUnsupported`); the text is never displayed — `Render2D`
/// discards it and falls back to the cleaned source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsupported(pub(crate) String);

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mathtext: unsupported LaTeX construct: {}", self.0)
    }
}

/// Builds an [`Unsupported`] from a message.
fn bad(msg: impl Into<String>) -> Unsupported {
    Unsupported(msg.into())
}

impl Node {
    /// Whether the node is an empty sequence (`{}`) — Go's `isEmptyNode` (parse.go:769).
    pub(crate) fn is_empty_seq(&self) -> bool {
        matches!(self, Node::Seq(items) if items.is_empty())
    }

    /// The empty node the parser stores where Go stores a nil in a non-optional slot.
    fn absent() -> Node {
        Node::Seq(Vec::new())
    }
}

/// Parses `latex` into its AST (parse.go:157 `Parse`); every unsupported construct, the depth cap
/// and the rune cap are `Err(Unsupported)`.
///
/// The input may still carry a surrounding delimiter pair — it is stripped first.
pub(crate) fn parse(latex: &str) -> Result<Node, Unsupported> {
    let body = crate::mathtext::strip_delimiters(latex);
    if body.trim().is_empty() {
        return Err(bad("empty math body")); // parse.go:159-161
    }
    let src: Vec<char> = body.chars().collect();
    if src.len() > MAX_MATH_INPUT_RUNES {
        return Err(bad("formula too long")); // parse.go:167-169
    }
    let mut p = Parser { src, pos: 0 };
    let node = p.parse_expr(0)?;
    p.skip_space();
    if p.pos < p.src.len() {
        // Leftover input: a stray close brace/bracket, a \right without a \left, … (parse.go:176).
        return Err(bad("unexpected token at end"));
    }
    Ok(node)
}

/// The rune scan state of the recursive descent (parse.go:185-188).
struct Parser {
    /// The stripped body as runes (Go `[]rune`).
    src: Vec<char>,
    /// The scan position, in runes.
    pos: usize,
}

impl Parser {
    /// A run of atoms up to a stopper, with scripts bound to the preceding atom and a big
    /// operator's scripts lifted into its limits (parse.go:194-235).
    fn parse_expr(&mut self, depth: usize) -> Result<Node, Unsupported> {
        if depth > MAX_PARSE_DEPTH {
            return Err(bad("nesting too deep"));
        }
        let mut items: Vec<Node> = Vec::new();
        loop {
            self.skip_space();
            if self.at_stopper() {
                break;
            }
            let Some(atom) = self.parse_atom(depth)? else {
                continue; // a layout/spacing macro produced nothing (parse.go:208-211)
            };
            if matches!(atom, Node::BigOp { .. }) {
                // A big operator absorbs following _/^ as LIMITS, never as scripts.
                let mut op = atom;
                self.attach_limits(&mut op, depth)?;
                items.push(op);
                continue;
            }
            let scripted = self.parse_scripts(atom, depth)?;
            items.push(scripted);
        }
        if items.len() == 1 {
            return Ok(items.remove(0));
        }
        Ok(Node::Seq(items))
    }

    /// Whether the scanner sits at a token that ends the current run (parse.go:239-258).
    fn at_stopper(&self) -> bool {
        let Some(&c) = self.src.get(self.pos) else {
            return true;
        };
        match c {
            '}' | ']' | '&' => true,
            '\\' => {
                self.has_command_at(self.pos, "right")
                    || self.has_row_break_at(self.pos)
                    || self.has_command_at(self.pos, "end")
            }
            _ => false,
        }
    }

    /// Binds a trailing `^` and/or `_` to `base` (parse.go:263-301). Two of the same script on
    /// one base is malformed; the two may appear in either order.
    fn parse_scripts(&mut self, base: Node, depth: usize) -> Result<Node, Unsupported> {
        let mut sup: Option<Node> = None;
        let mut sub: Option<Node> = None;
        loop {
            self.skip_space();
            let Some(&c) = self.src.get(self.pos) else {
                break;
            };
            if c != '^' && c != '_' {
                break;
            }
            self.pos += 1;
            let arg = self.parse_script_arg(depth)?;
            if c == '^' {
                if sup.is_some() {
                    return Err(bad("double superscript"));
                }
                sup = arg;
            } else {
                if sub.is_some() {
                    return Err(bad("double subscript"));
                }
                sub = arg;
            }
        }
        Ok(match (sup, sub) {
            (Some(s), Some(b)) => Node::SupSub {
                base: Box::new(base),
                sup: Box::new(s),
                sub: Box::new(b),
            },
            (Some(s), None) => Node::Sup {
                base: Box::new(base),
                exp: Box::new(s),
            },
            (None, Some(b)) => Node::Sub {
                base: Box::new(base),
                sub: Box::new(b),
            },
            (None, None) => base,
        })
    }

    /// The argument of a `^` or `_`: a braced group, or ONE atom without scripts of its own
    /// (parse.go:305-314). `None` mirrors Go's nil for a dropped layout macro.
    fn parse_script_arg(&mut self, depth: usize) -> Result<Option<Node>, Unsupported> {
        self.skip_space();
        match self.src.get(self.pos) {
            None => Err(bad("missing script argument")),
            Some('{') => self.parse_group(depth).map(Some),
            Some(_) => self.parse_atom(depth + 1),
        }
    }

    /// Reads the `_`/`^` limits that follow a big operator, in either order (parse.go:320-348).
    /// The operand is NOT consumed — it stays the next sibling of the enclosing `Seq`.
    fn attach_limits(&mut self, op: &mut Node, depth: usize) -> Result<(), Unsupported> {
        let Node::BigOp { lower, upper, .. } = op else {
            return Ok(());
        };
        loop {
            self.skip_space();
            let Some(&c) = self.src.get(self.pos) else {
                return Ok(());
            };
            if c != '^' && c != '_' {
                break;
            }
            self.pos += 1;
            let arg = self.parse_script_arg(depth)?;
            if c == '^' {
                if upper.is_some() {
                    return Err(bad("double upper limit"));
                }
                *upper = arg.map(Box::new);
            } else {
                if lower.is_some() {
                    return Err(bad("double lower limit"));
                }
                *lower = arg.map(Box::new);
            }
        }
        Ok(())
    }

    /// One leading atom (parse.go:353-381); `Ok(None)` for a layout/spacing macro that yields no
    /// node.
    fn parse_atom(&mut self, depth: usize) -> Result<Option<Node>, Unsupported> {
        if depth > MAX_PARSE_DEPTH {
            return Err(bad("nesting too deep"));
        }
        let Some(&c) = self.src.get(self.pos) else {
            return Err(bad("unexpected end of input"));
        };
        if c == '\\' {
            return self.parse_command(depth);
        }
        if c == '{' {
            return self.parse_group(depth).map(Some);
        }
        if c.is_ascii_digit() {
            return Ok(Some(self.parse_number()));
        }
        if c.is_ascii_alphabetic() {
            self.pos += 1;
            return Ok(Some(Node::Atom(c.to_string())));
        }
        if c == '^' || c == '_' {
            return Err(bad("script without base"));
        }
        if c == '}' || c == ']' || c == '&' {
            return Err(bad("unexpected delimiter"));
        }
        // Ordinary/relation/operator character: parentheses, + - = < > | etc.
        self.pos += 1;
        Ok(Some(Node::Atom(c.to_string())))
    }

    /// A run of digits and interior decimal points as ONE atom (parse.go:385-396), so `128` is a
    /// single visual unit — and `\frac12` takes `12` as its numerator.
    fn parse_number(&mut self) -> Node {
        let start = self.pos;
        while let Some(&c) = self.src.get(self.pos) {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        Node::Atom(self.src[start..self.pos].iter().collect())
    }

    /// A braced group; the INNER node is returned, not a wrapper (parse.go:399-413).
    fn parse_group(&mut self, depth: usize) -> Result<Node, Unsupported> {
        if self.src.get(self.pos) != Some(&'{') {
            return Err(bad("expected '{'"));
        }
        self.pos += 1;
        let inner = self.parse_expr(depth + 1)?;
        if self.src.get(self.pos) != Some(&'}') {
            return Err(bad("unbalanced '{'"));
        }
        self.pos += 1;
        Ok(inner)
    }

    /// A backslash sequence (parse.go:419-511). THE ORDER OF THE CASES IS THE CONTRACT: escaped
    /// literal → control symbols → structural macros → big/word operators → accents → fonts →
    /// function names → layout macros → symbol tables → unknown (a fallback).
    fn parse_command(&mut self, depth: usize) -> Result<Option<Node>, Unsupported> {
        // Escaped punctuation: \{ \} \$ \% \& \# \_ → the literal; "\ " is a dropped control
        // space (parse.go:421-431).
        if let Some(&ch) = self.src.get(self.pos + 1)
            && matches!(ch, '{' | '}' | '$' | '%' | '&' | '#' | '_' | ' ')
        {
            self.pos += 2;
            if ch == ' ' {
                return Ok(None);
            }
            return Ok(Some(Node::Atom(ch.to_string())));
        }
        let (name, _spaced) = self.scan_command_name();
        if name.is_empty() {
            return Err(bad("lone backslash")); // parse.go:433-435
        }
        // Control symbols that map to a glyph (parse.go:440-445).
        match name.as_str() {
            "|" => return Ok(Some(Node::Atom("\u{2016}".to_owned()))), // ‖ norm bar
            "backslash" => return Ok(Some(Node::Atom("\\".to_owned()))),
            _ => {}
        }
        // Structural macros (parse.go:447-469).
        match name.as_str() {
            "frac" | "tfrac" | "dfrac" | "cfrac" => return self.parse_frac(depth).map(Some),
            "sqrt" => return self.parse_sqrt(depth).map(Some),
            "left" => return self.parse_delim(depth).map(Some),
            "right" => return Err(bad("\\right without \\left")),
            "begin" => return self.parse_env(depth).map(Some),
            "end" => return Err(bad("\\end without \\begin")),
            "text" | "textrm" | "textbf" | "textit" | "textsf" | "texttt" | "mbox" => {
                return self.parse_text().map(Some);
            }
            n if macros::is_big_op(n) => {
                return Ok(Some(Node::BigOp {
                    op: macros::big_op_kind(n),
                    glyph: macros::big_op_single_glyph(n),
                    word: "",
                    lower: None,
                    upper: None,
                }));
            }
            n if macros::is_word_op(n) => {
                return Ok(Some(Node::BigOp {
                    op: OpFamily::Lim,
                    glyph: "",
                    word: macros::word_op_name(n),
                    lower: None,
                    upper: None,
                }));
            }
            _ => {}
        }
        // Accents (parse.go:473-479).
        if let Some(kind) = macros::accent_kind(&name) {
            let base = self.parse_required_arg(depth)?;
            return Ok(Some(Node::Accent {
                kind,
                base: Box::new(base),
            }));
        }
        // Math fonts (parse.go:484-490): the whole subtree is rewritten, so this never falls back.
        if let Some(style) = macros::math_font_style(&name) {
            let arg = self.parse_required_arg(depth)?;
            return Ok(Some(apply_font(style, arg)));
        }
        // Named functions render as their upright name — one multi-letter atom (parse.go:494-496).
        if macros::is_func_name(&name) {
            return Ok(Some(Node::Atom(name)));
        }
        // Layout / spacing / style macros carry no glyph: drop them (parse.go:500-502).
        if macros::is_layout_macro(&name) {
            return Ok(None);
        }
        // Symbol macro from the greek/operator tables (parse.go:505-507).
        if let Some(r) = symbols::symbol_rune(&name) {
            return Ok(Some(Node::Atom(r.to_string())));
        }
        Err(bad(format!("unknown macro \\{name}"))) // parse.go:510
    }

    /// A control word (a run of ASCII letters) or a control symbol (a single non-letter) just
    /// after the backslash at `pos` (parse.go:517-538). Trailing ASCII SPACES (only `' '`) are
    /// swallowed after a control word; `spaced` records whether any were.
    fn scan_command_name(&mut self) -> (String, bool) {
        let i = self.pos + 1; // skip the backslash
        let Some(&first) = self.src.get(i) else {
            self.pos = i;
            return (String::new(), false);
        };
        if !first.is_ascii_alphabetic() {
            self.pos = i + 1;
            return (first.to_string(), false);
        }
        let mut j = i;
        while self.src.get(j).is_some_and(char::is_ascii_alphabetic) {
            j += 1;
        }
        let name: String = self.src[i..j].iter().collect();
        let mut spaced = false;
        while self.src.get(j) == Some(&' ') {
            j += 1;
            spaced = true;
        }
        self.pos = j;
        (name, spaced)
    }

    /// The two brace arguments of `\frac{num}{den}` (parse.go:585-595).
    fn parse_frac(&mut self, depth: usize) -> Result<Node, Unsupported> {
        let num = self.parse_required_arg(depth)?;
        let den = self.parse_required_arg(depth)?;
        Ok(Node::Frac {
            num: Box::new(num),
            den: Box::new(den),
        })
    }

    /// `\sqrt{x}` and the optional-degree `\sqrt[n]{x}` (parse.go:598-613).
    fn parse_sqrt(&mut self, depth: usize) -> Result<Node, Unsupported> {
        let mut index = None;
        self.skip_space();
        if self.src.get(self.pos) == Some(&'[') {
            index = Some(Box::new(self.parse_bracket_arg(depth)?));
        }
        let radicand = self.parse_required_arg(depth)?;
        Ok(Node::Sqrt {
            radicand: Box::new(radicand),
            index,
        })
    }

    /// `\left<D> … \right<D>` (parse.go:618-637).
    fn parse_delim(&mut self, depth: usize) -> Result<Node, Unsupported> {
        let left = self.read_delim_symbol()?;
        let inner = self.parse_expr(depth + 1)?;
        self.skip_space();
        if !self.has_command_at(self.pos, "right") {
            return Err(bad("\\left without matching \\right"));
        }
        self.consume_command("right");
        let right = self.read_delim_symbol()?;
        Ok(Node::Delim {
            left,
            right,
            inner: Box::new(inner),
        })
    }

    /// The delimiter following `\left`/`\right`, normalized (parse.go:642-678). `\|`, `\vert` and
    /// `\Vert` all collapse to a SINGLE bar — a faithful Go quirk the goldens pin.
    fn read_delim_symbol(&mut self) -> Result<String, Unsupported> {
        self.skip_space();
        let Some(&c) = self.src.get(self.pos) else {
            return Err(bad("missing delimiter after \\left/\\right"));
        };
        if c == '\\' {
            let (name, _spaced) = self.scan_command_name();
            let sym = match name.as_str() {
                "{" => "{",
                "}" => "}",
                "|" | "vert" | "Vert" => "|",
                "langle" => "\u{27E8}", // ⟨
                "rangle" => "\u{27E9}", // ⟩
                "lfloor" => "\u{230A}", // ⌊
                "rfloor" => "\u{230B}", // ⌋
                "lceil" => "\u{2308}",  // ⌈
                "rceil" => "\u{2309}",  // ⌉
                _ => return Err(bad("unsupported delimiter")),
            };
            return Ok(sym.to_owned());
        }
        if matches!(c, '(' | ')' | '[' | ']' | '|' | '/' | '.') {
            self.pos += 1;
            return Ok(c.to_string());
        }
        Err(bad("unsupported delimiter"))
    }

    /// `\begin{env} … \end{env}` into a [`Node::Matrix`] (parse.go:683-733).
    fn parse_env(&mut self, depth: usize) -> Result<Node, Unsupported> {
        let env = self.read_env_name()?;
        if !macros::is_matrix_env(&env) && !macros::is_aligned_env(&env) {
            return Err(bad("unsupported environment"));
        }
        let mut rows: Vec<Vec<Node>> = Vec::new();
        let mut row: Vec<Node> = Vec::new();
        loop {
            let cell = self.parse_expr(depth + 1)?;
            row.push(cell);
            self.skip_space();
            if self.pos >= self.src.len() {
                return Err(bad("environment not closed"));
            }
            if self.src.get(self.pos) == Some(&'&') {
                self.pos += 1;
                continue;
            }
            if self.has_row_break_at(self.pos) {
                self.pos += 2; // consume "\\"
                self.skip_row_break_options();
                rows.push(std::mem::take(&mut row));
                continue;
            }
            if self.has_command_at(self.pos, "end") {
                rows.push(std::mem::take(&mut row));
                self.consume_command("end");
                let close = self.read_env_name()?;
                if close != env {
                    return Err(bad("environment closed by a different \\end"));
                }
                break;
            }
            return Err(bad("malformed environment"));
        }
        drop_trailing_empty_row(&mut rows);
        if rows.is_empty() {
            return Err(bad("empty environment"));
        }
        Ok(Node::Matrix { env, rows })
    }

    /// Drops the optional spacing argument after a `\\` row break — `\\*` and/or `\\[4pt]`
    /// (parse.go:740-754).
    fn skip_row_break_options(&mut self) {
        self.skip_space();
        if self.src.get(self.pos) == Some(&'*') {
            self.pos += 1;
            self.skip_space();
        }
        if self.src.get(self.pos) == Some(&'[') {
            while self.pos < self.src.len() && self.src[self.pos] != ']' {
                self.pos += 1;
            }
            if self.pos < self.src.len() {
                self.pos += 1; // consume ']'
            }
        }
    }

    /// The `{name}` group after `\begin`/`\end`, with ONE trailing `*` stripped
    /// (parse.go:777-793).
    fn read_env_name(&mut self) -> Result<String, Unsupported> {
        self.skip_space();
        if self.src.get(self.pos) != Some(&'{') {
            return Err(bad("expected {name} after \\begin/\\end"));
        }
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != '}' {
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Err(bad("unterminated environment name"));
        }
        let name: String = self.src[start..self.pos].iter().collect();
        self.pos += 1; // consume '}'
        Ok(name.strip_suffix('*').unwrap_or(&name).to_owned())
    }

    /// `\text{…}` as literal prose (parse.go:798-836): brace-balanced (nested braces are copied),
    /// a backslash is DROPPED and its follower copied verbatim, whitespace preserved.
    fn parse_text(&mut self) -> Result<Node, Unsupported> {
        self.skip_space();
        if self.src.get(self.pos) != Some(&'{') {
            return Err(bad("\\text without a braced argument"));
        }
        self.pos += 1;
        let mut out = String::new();
        let mut depth = 1_usize;
        while let Some(&c) = self.src.get(self.pos) {
            match c {
                '\\' => {
                    if let Some(&next) = self.src.get(self.pos + 1) {
                        out.push(next);
                        self.pos += 2;
                        continue;
                    }
                    self.pos += 1; // a lone trailing backslash is dropped
                }
                '{' => {
                    depth += 1;
                    out.push(c);
                    self.pos += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(Node::Text(out));
                    }
                    out.push(c);
                    self.pos += 1;
                }
                _ => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        Err(bad("unterminated \\text argument"))
    }

    /// A mandatory macro argument: a braced group, or a single atom (parse.go:840-852). A dropped
    /// layout macro yields Go's nil, which is [`Node::absent`] here.
    fn parse_required_arg(&mut self, depth: usize) -> Result<Node, Unsupported> {
        self.skip_space();
        let Some(&c) = self.src.get(self.pos) else {
            return Err(bad("missing required argument"));
        };
        if c == '{' {
            return self.parse_group(depth);
        }
        if c == '}' || c == '&' || c == ']' {
            return Err(bad("missing required argument"));
        }
        Ok(self.parse_atom(depth + 1)?.unwrap_or_else(Node::absent))
    }

    /// The optional `[n]` argument of `\sqrt[n]{}` (parse.go:856-867).
    fn parse_bracket_arg(&mut self, depth: usize) -> Result<Node, Unsupported> {
        self.pos += 1; // consume '['
        let inner = self.parse_expr(depth + 1)?;
        if self.src.get(self.pos) != Some(&']') {
            return Err(bad("unbalanced '['"));
        }
        self.pos += 1;
        Ok(inner)
    }

    /// Advances over inter-token space — ASCII `' '`, `'\t'`, `'\n'`, `'\r'` ONLY, never
    /// `char::is_whitespace` (parse.go:871-880).
    fn skip_space(&mut self) {
        while let Some(&c) = self.src.get(self.pos) {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                self.pos += 1;
            } else {
                return;
            }
        }
    }

    /// Whether rune `i` begins the control word `\name` and NOT a longer one (`\endfoo` ≠ `\end`;
    /// parse.go:884-903).
    fn has_command_at(&self, i: usize, name: &str) -> bool {
        if self.src.get(i) != Some(&'\\') {
            return false;
        }
        let nm: Vec<char> = name.chars().collect();
        if i + 1 + nm.len() > self.src.len() {
            return false;
        }
        for (k, r) in nm.iter().enumerate() {
            if self.src[i + 1 + k] != *r {
                return false;
            }
        }
        !self
            .src
            .get(i + 1 + nm.len())
            .is_some_and(char::is_ascii_alphabetic)
    }

    /// Whether rune `i` starts a `\\` row separator (parse.go:906-908).
    fn has_row_break_at(&self, i: usize) -> bool {
        self.src.get(i) == Some(&'\\') && self.src.get(i + 1) == Some(&'\\')
    }

    /// Advances past `\name` and any swallowed trailing spaces (parse.go:912-917); the caller has
    /// already matched with [`Parser::has_command_at`].
    fn consume_command(&mut self, name: &str) {
        self.pos += 1 + name.chars().count();
        while self.src.get(self.pos) == Some(&' ') {
            self.pos += 1;
        }
    }
}

/// Removes a final row that is a single empty cell — the artefact of a trailing `\\` before
/// `\end` (parse.go:758-766).
fn drop_trailing_empty_row(rows: &mut Vec<Vec<Node>>) {
    if let Some(last) = rows.last()
        && last.len() == 1
        && last[0].is_empty_seq()
    {
        rows.pop();
    }
}

/// Rewrites every `Atom`/`Text` through [`apply_math_font`], recursing into every node EXCEPT
/// `BigOp` and `Matrix`, which are returned untouched (parse.go:546-582 `applyFont`).
fn apply_font(style: MathStyle, n: Node) -> Node {
    if style == MathStyle::Plain {
        return n; // parse.go:547-549
    }
    match n {
        Node::Atom(t) => Node::Atom(apply_math_font(style, &t)),
        Node::Text(s) => Node::Text(apply_math_font(style, &s)),
        Node::Seq(items) => Node::Seq(items.into_iter().map(|it| apply_font(style, it)).collect()),
        Node::Frac { num, den } => Node::Frac {
            num: Box::new(apply_font(style, *num)),
            den: Box::new(apply_font(style, *den)),
        },
        Node::Sqrt { radicand, index } => Node::Sqrt {
            radicand: Box::new(apply_font(style, *radicand)),
            index: index.map(|i| Box::new(apply_font(style, *i))),
        },
        Node::Sup { base, exp } => Node::Sup {
            base: Box::new(apply_font(style, *base)),
            exp: Box::new(apply_font(style, *exp)),
        },
        Node::Sub { base, sub } => Node::Sub {
            base: Box::new(apply_font(style, *base)),
            sub: Box::new(apply_font(style, *sub)),
        },
        Node::SupSub { base, sup, sub } => Node::SupSub {
            base: Box::new(apply_font(style, *base)),
            sup: Box::new(apply_font(style, *sup)),
            sub: Box::new(apply_font(style, *sub)),
        },
        Node::Accent { kind, base } => Node::Accent {
            kind,
            base: Box::new(apply_font(style, *base)),
        },
        Node::Delim { left, right, inner } => Node::Delim {
            left,
            right,
            inner: Box::new(apply_font(style, *inner)),
        },
        // BigOp / Matrix: left structurally intact (parse.go:577-580).
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{Node, OpFamily, parse};
    use crate::mathtext::symbols::AccentKind;

    #[track_caller]
    fn must_parse(src: &str) -> Node {
        match parse(src) {
            Ok(n) => n,
            Err(e) => panic!("parse({src:?}) error: {e}"),
        }
    }

    /// Go: `parse_test.go:9` `atomText`.
    #[track_caller]
    fn atom_text(n: &Node, want: &str) {
        match n {
            Node::Atom(t) => assert_eq!(t, want, "Atom text"),
            other => panic!("node = {other:?}, want Atom({want:?})"),
        }
    }

    /// Go: `parse_test.go:21` `seqOf`.
    #[track_caller]
    fn seq_of(n: &Node, want: usize) -> &[Node] {
        match n {
            Node::Seq(items) => {
                assert_eq!(items.len(), want, "Seq length: {items:?}");
                items
            }
            other => panic!("node = {other:?}, want Seq of {want}"),
        }
    }

    // Go: internal/mathtext/parse_test.go:43 TestParseFrac
    #[test]
    fn parse_frac() {
        let n = must_parse(r"\frac{a+b}{c}");
        let Node::Frac { num, den } = &n else {
            panic!("node = {n:?}, want Frac")
        };
        let items = seq_of(num, 3);
        atom_text(&items[0], "a");
        atom_text(&items[1], "+");
        atom_text(&items[2], "b");
        atom_text(den, "c");
    }

    // Go: internal/mathtext/parse_test.go:57 TestParseSup
    #[test]
    fn parse_sup() {
        let n = must_parse("x^2");
        let Node::Sup { base, exp } = &n else {
            panic!("node = {n:?}, want Sup")
        };
        atom_text(base, "x");
        atom_text(exp, "2");
    }

    // Go: internal/mathtext/parse_test.go:68 TestParseSub
    #[test]
    fn parse_sub() {
        let n = must_parse("a_i");
        let Node::Sub { base, sub } = &n else {
            panic!("node = {n:?}, want Sub")
        };
        atom_text(base, "a");
        atom_text(sub, "i");
    }

    // Go: internal/mathtext/parse_test.go:79 TestParseSupSub
    #[test]
    fn parse_sup_sub() {
        let n = must_parse("x_i^2");
        let Node::SupSub { base, sup, sub } = &n else {
            panic!("node = {n:?}, want SupSub")
        };
        atom_text(base, "x");
        atom_text(sub, "i");
        atom_text(sup, "2");

        // The other order a^b_c binds the same way.
        let n2 = must_parse("a^b_c");
        let Node::SupSub { base, sup, sub } = &n2 else {
            panic!("node = {n2:?}, want SupSub")
        };
        atom_text(base, "a");
        atom_text(sup, "b");
        atom_text(sub, "c");
    }

    // Go: internal/mathtext/parse_test.go:101 TestParseSqrt
    #[test]
    fn parse_sqrt() {
        let n = must_parse(r"\sqrt{x+1}");
        let Node::Sqrt { radicand, index } = &n else {
            panic!("node = {n:?}, want Sqrt")
        };
        assert!(index.is_none(), "Sqrt.index = {index:?}, want None");
        let items = seq_of(radicand, 3);
        atom_text(&items[0], "x");
        atom_text(&items[1], "+");
        atom_text(&items[2], "1");
    }

    // Go: internal/mathtext/parse_test.go:117 TestParseRootIndex
    #[test]
    fn parse_root_index() {
        let n = must_parse(r"\sqrt[3]{x}");
        let Node::Sqrt { radicand, index } = &n else {
            panic!("node = {n:?}, want Sqrt")
        };
        let idx = index.as_ref().expect("Sqrt.index = None, want atom 3");
        atom_text(idx, "3");
        atom_text(radicand, "x");
    }

    // Go: internal/mathtext/parse_test.go:132 TestParseSum
    #[test]
    fn parse_sum() {
        let n = must_parse(r"\sum_{i=1}^{n} i");
        let items = seq_of(&n, 2);
        let Node::BigOp {
            op, lower, upper, ..
        } = &items[0]
        else {
            panic!("first item = {:?}, want BigOp", items[0])
        };
        assert_eq!(*op, OpFamily::Sum);
        let lower = lower.as_ref().expect("BigOp.lower");
        let low = seq_of(lower, 3);
        atom_text(&low[0], "i");
        atom_text(&low[1], "=");
        atom_text(&low[2], "1");
        atom_text(upper.as_ref().expect("BigOp.upper"), "n");
        atom_text(&items[1], "i");
    }

    // Go: internal/mathtext/parse_test.go:151 TestParseInt
    #[test]
    fn parse_int() {
        let n = must_parse(r"\int_0^1 f");
        let items = seq_of(&n, 2);
        let Node::BigOp {
            op, lower, upper, ..
        } = &items[0]
        else {
            panic!("first item = {:?}, want BigOp", items[0])
        };
        assert_eq!(*op, OpFamily::Int);
        atom_text(lower.as_ref().expect("BigOp.lower"), "0");
        atom_text(upper.as_ref().expect("BigOp.upper"), "1");
        atom_text(&items[1], "f");
    }

    // Go: internal/mathtext/parse_test.go:167 TestParsePmatrix
    #[test]
    fn parse_pmatrix() {
        let n = must_parse(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}");
        let Node::Matrix { env, rows } = &n else {
            panic!("node = {n:?}, want Matrix")
        };
        assert_eq!(env, "pmatrix");
        assert_eq!(
            (rows.len(), rows[0].len(), rows[1].len()),
            (2, 2, 2),
            "matrix shape"
        );
        atom_text(&rows[0][0], "a");
        atom_text(&rows[0][1], "b");
        atom_text(&rows[1][0], "c");
        atom_text(&rows[1][1], "d");
    }

    // Go: internal/mathtext/parse_test.go:186 TestParseCases
    #[test]
    fn parse_cases() {
        let n = must_parse(r"\begin{cases} x & a \\ y & b \end{cases}");
        let Node::Matrix { env, rows } = &n else {
            panic!("node = {n:?}, want Matrix")
        };
        assert_eq!(env, "cases");
        assert_eq!(
            (rows.len(), rows[0].len(), rows[1].len()),
            (2, 2, 2),
            "cases shape"
        );
        atom_text(&rows[0][0], "x");
        atom_text(&rows[0][1], "a");
        atom_text(&rows[1][0], "y");
        atom_text(&rows[1][1], "b");
    }

    // Go: internal/mathtext/parse_test.go:205 TestParseDelim
    #[test]
    fn parse_delim() {
        let n = must_parse(r"\left( \frac{a}{b} \right)");
        let Node::Delim { left, right, inner } = &n else {
            panic!("node = {n:?}, want Delim")
        };
        assert_eq!((left.as_str(), right.as_str()), ("(", ")"));
        let Node::Frac { num, den } = inner.as_ref() else {
            panic!("Delim.inner = {inner:?}, want Frac")
        };
        atom_text(num, "a");
        atom_text(den, "b");
    }

    // Go: internal/mathtext/parse_test.go:223 TestParseText
    #[test]
    fn parse_text() {
        let n = must_parse(r"\text{if } x");
        let items = seq_of(&n, 2);
        match &items[0] {
            Node::Text(s) => assert_eq!(s, "if ", "the trailing space is kept"),
            other => panic!("first item = {other:?}, want Text"),
        }
        atom_text(&items[1], "x");
    }

    // Go: internal/mathtext/parse_test.go:237 TestParseGreek
    #[test]
    fn parse_greek() {
        atom_text(&must_parse(r"\alpha"), "α");
    }

    // Go: internal/mathtext/parse_test.go:244 TestParseAccent
    #[test]
    fn parse_accent() {
        let cases: [(&str, AccentKind); 7] = [
            (r"\hat{f}", AccentKind::Hat),
            (r"\bar{x}", AccentKind::Bar),
            (r"\overline{ab}", AccentKind::Bar),
            (r"\vec{v}", AccentKind::Vec),
            (r"\tilde{a}", AccentKind::Tilde),
            (r"\dot{x}", AccentKind::Dot),
            (r"\ddot{x}", AccentKind::Ddot),
        ];
        for (src, want) in cases {
            let n = must_parse(src);
            let Node::Accent { kind, .. } = &n else {
                panic!("parse({src:?}) = {n:?}, want Accent")
            };
            assert_eq!(*kind, want, "parse({src:?}) kind");
        }
        let n = must_parse(r"\hat{f}");
        let Node::Accent { base, .. } = &n else {
            panic!("want Accent")
        };
        atom_text(base, "f");
    }

    // Go: internal/mathtext/parse_test.go:271 TestParseMathFont
    #[test]
    fn parse_math_font() {
        atom_text(&must_parse(r"\mathbb{R}"), "ℝ");
        atom_text(&must_parse(r"\mathbb{C}"), "ℂ");
        atom_text(&must_parse(r"\mathbb{N}"), "ℕ");
        atom_text(&must_parse(r"\mathbf{E}"), "E"); // plain style degrades to itself
        atom_text(&must_parse(r"\mathrm{d}"), "d");
        atom_text(&must_parse(r"\mathcal{L}"), "ℒ"); // Letterlike hole
        atom_text(&must_parse(r"\mathfrak{g}"), "𝔤"); // systematic fraktur
        let n = must_parse(r"\mathbb{RC}");
        let items = seq_of(&n, 2);
        atom_text(&items[0], "ℝ");
        atom_text(&items[1], "ℂ");
    }

    // Go: internal/mathtext/parse_test.go:286 TestParseMid
    #[test]
    fn parse_mid() {
        atom_text(&must_parse(r"\mid"), "∣");
    }

    // Go: internal/mathtext/parse_test.go:291 TestParseLiteralBrace
    #[test]
    fn parse_literal_brace() {
        let n = must_parse(r"\{ x \}");
        let items = seq_of(&n, 3);
        atom_text(&items[0], "{");
        atom_text(&items[1], "x");
        atom_text(&items[2], "}");
    }

    // Go: internal/mathtext/parse_test.go:299 TestParseNorm
    #[test]
    fn parse_norm() {
        atom_text(&must_parse(r"\|"), "‖");
    }

    // Go: internal/mathtext/parse_test.go:305 TestParseAligned
    #[test]
    fn parse_aligned() {
        let n = must_parse(r"\begin{aligned} a &= b \\ c &= d \end{aligned}");
        let Node::Matrix { env, rows } = &n else {
            panic!("node = {n:?}, want Matrix")
        };
        assert_eq!(env, "aligned");
        assert_eq!(
            (rows.len(), rows[0].len(), rows[1].len()),
            (2, 2, 2),
            "aligned shape"
        );
        atom_text(&rows[0][0], "a");
        atom_text(&rows[1][0], "c");
    }

    // Go: internal/mathtext/parse_test.go:323 TestParseAlignedRowBreakSpacing
    #[test]
    fn parse_aligned_row_break_spacing() {
        let n = must_parse(r"\begin{aligned} a &= b \\[4pt] c &= d \end{aligned}");
        let Node::Matrix { rows, .. } = &n else {
            panic!("node = {n:?}, want Matrix")
        };
        assert_eq!(rows.len(), 2, r"\\[4pt] must not force a fallback");
    }

    // Go: internal/mathtext/parse_test.go:336 TestParseErrors
    #[test]
    fn parse_errors() {
        let bad: [&str; 11] = [
            r"\foobar",                         // unknown macro
            r"\overbrace{x}",                   // Tier-3 accent-brace
            r"\begin{matrixx} a \end{matrixx}", // unknown environment
            r"\phantom{x}",                     // Tier-3 spacing
            r"\frac{a}",                        // missing second frac arg
            "{a",                               // unbalanced group
            "a}",                               // stray close brace
            r"\left( a",                        // \left with no \right
            "",                                 // empty body
            "x^a^b",                            // double superscript
            r"\sqrt",                           // missing radicand
        ];
        for src in bad {
            assert!(parse(src).is_err(), "parse({src:?}) = Ok, want Err");
        }
    }

    // Go: internal/mathtext/parse.go:26,30 the depth and rune caps
    #[test]
    fn parse_bounds() {
        let deep = format!("{}x{}", "{".repeat(70), "}".repeat(70));
        assert!(parse(&deep).is_err(), "depth cap");
        let long = "a+b+".repeat(2000) + "c";
        assert!(parse(&long).is_err(), "rune cap");
        // Just under the cap still parses.
        let ok = "a+".repeat(100) + "b";
        assert!(parse(&ok).is_ok());
    }

    // Go: internal/mathtext/parse.go:385-396 parseNumber / :305-314 parseScriptArg —
    // the `\frac12` and `x^12` quirks the goldens pin (spec mathtext.md §3.12).
    #[test]
    fn number_run_quirks() {
        // \frac12 takes "12" as the numerator, so the denominator is missing → fallback.
        assert!(parse(r"\frac12").is_err());
        // x^12 binds the whole digit run.
        let n = must_parse("x^12");
        let Node::Sup { exp, .. } = &n else {
            panic!("node = {n:?}, want Sup")
        };
        atom_text(exp, "12");
        assert_eq!(must_parse("x^{12}"), n);
        // A bare `[…]` at top level stops at `]` and leaves input → fallback.
        assert!(parse("[a, b]").is_err());
        // `\\` outside an environment is a stopper that leaves input → fallback.
        assert!(parse(r"a \\ b").is_err());
    }
}
