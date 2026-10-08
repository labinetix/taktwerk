//! Header import: a C header in, a proposed descriptor out.
//!
//! The grammar is deliberately small and this is not a C parser: it reads flat `typedef struct
//! { … } Name;` blocks and function prototypes whose types are the admitted scalars, `void`,
//! pointers to those, or pointers to the structs it read. Comments and string literals are
//! skipped, macros are never expanded, and anything the grammar does not cover is skipped with
//! a note rather than guessed at.
//!
//! A header in the recommended shape (see [`crate::shape`]) is read without guessing: the
//! descriptor comes out with `confirmed = true`. Any other header gets a heuristic proposal:
//! function roles, dimension members and pointer→length relations are guessed by name. Every
//! guess, and every way the header deviates from the recommended shape, is listed in
//! [`Proposal::notes`] and the descriptor is written with `confirmed = false`, which
//! [`Descriptor::validate`] refuses until the developer has checked it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use taktwerk_core::model::{Causality, Dimension, Instances, ModelInterface, Variable};
use taktwerk_core::value::{Dim, Layout, ScalarType};

use crate::descriptor::{
    Abi, Arg, Builtin, CType, Call, Descriptor, Handle, Member, Number, PhaseValues, Returns,
    StructSpec,
};

/// A header the importer cannot read at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("import: {0}")]
pub struct ImportError(pub String);

// ==========================================================================
// Parsed header.
// ==========================================================================

/// A parsed header: its flat typedef structs and function prototypes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    /// `typedef struct` blocks, in order.
    pub structs: Vec<Struct>,
    /// Function prototypes, in order.
    pub functions: Vec<Function>,
    /// What was skipped and why.
    pub notes: Vec<String>,
}

/// One `typedef struct … { … } Name;`.
#[derive(Debug, Clone, PartialEq)]
pub struct Struct {
    /// Typedef name.
    pub name: String,
    /// Members in declaration order.
    pub members: Vec<Slot>,
    /// Line of the opening brace.
    pub line: usize,
}

/// One function prototype.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// Symbol name.
    pub name: String,
    /// Return type.
    pub returns: ParsedType,
    /// Parameters in order.
    pub params: Vec<Slot>,
    /// Line of the declaration.
    pub line: usize,
}

/// A struct member or a function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// Name; a parameter may have none.
    pub name: Option<String>,
    /// Type.
    pub ty: ParsedType,
    /// Declared `const`.
    pub is_const: bool,
    /// Declaration line.
    pub line: usize,
    /// Text of a comment that follows a struct member's `;` on the same line, trimmed.
    pub comment: Option<String>,
}

/// A type the grammar resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedType {
    /// An admitted scalar, `T *`, or `void *`.
    Scalar(CType),
    /// `void **`.
    VoidPtrPtr,
    /// Plain `void` (a return type, or an empty parameter list).
    Void,
    /// A pointer to a typedef struct of this header (`pointer`), or the struct by value.
    Struct {
        /// Typedef name.
        name: String,
        /// `Name *` rather than `Name`.
        pointer: bool,
    },
    /// Anything else, as written.
    Other(String),
}

/// A type as written: words, stars, const.
#[derive(Debug, Clone, Default)]
struct RawType {
    words: Vec<String>,
    stars: usize,
    is_const: bool,
    array: bool,
}

impl RawType {
    fn resolve(&self, structs: &BTreeSet<String>) -> ParsedType {
        let words: Vec<&str> = self
            .words
            .iter()
            .map(String::as_str)
            .filter(|w| *w != "const" && *w != "volatile")
            .collect();
        let stars = self.stars + usize::from(self.array);
        let written = || {
            format!(
                "{}{}",
                self.words.join(" "),
                " *".repeat(self.stars) + if self.array { "[]" } else { "" }
            )
        };
        if words.len() == 1 && structs.contains(words[0]) {
            if stars > 1 {
                return ParsedType::Other(written());
            }
            return ParsedType::Struct {
                name: words[0].to_owned(),
                pointer: stars == 1,
            };
        }
        if words == ["void"] {
            return match stars {
                0 => ParsedType::Void,
                1 => ParsedType::Scalar(CType {
                    base: None,
                    pointer: true,
                }),
                2 => ParsedType::VoidPtrPtr,
                _ => ParsedType::Other(written()),
            };
        }
        if stars > 1 {
            return ParsedType::Other(written());
        }
        let spelled = format!("{}{}", words.join(" "), if stars == 1 { " *" } else { "" });
        CType::parse(&spelled).map_or_else(|_| ParsedType::Other(written()), ParsedType::Scalar)
    }
}

// ==========================================================================
// Lexer.
// ==========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
    Directive(String),
}

#[derive(Debug, Clone)]
struct Lexed {
    token: Token,
    line: usize,
}

/// A comment, kept beside the tokens.
#[derive(Debug, Clone)]
struct Comment {
    /// Number of tokens before it.
    after: usize,
    /// Line it starts on.
    line: usize,
    /// Its text without the delimiters, trimmed.
    text: String,
}

/// Tokens with line numbers, and the comments beside them; literals dropped.
fn lex(file: &str, text: &str) -> Result<(Vec<Lexed>, Vec<Comment>), ImportError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut comments = Vec::new();
    let (mut i, mut line, mut fresh_line) = (0_usize, 1_usize, true);
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            fresh_line = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let opened = line;
            i += 2;
            let start = i;
            loop {
                match chars.get(i) {
                    None => {
                        return Err(ImportError(format!(
                            "{file}:{opened}: unterminated comment"
                        )));
                    }
                    Some('\n') => line += 1,
                    Some('*') if chars.get(i + 1) == Some(&'/') => {
                        comments.push(Comment {
                            after: tokens.len(),
                            line: opened,
                            text: chars[start..i].iter().collect::<String>().trim().to_owned(),
                        });
                        i += 2;
                        break;
                    }
                    Some(_) => {}
                }
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            let start = i + 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            comments.push(Comment {
                after: tokens.len(),
                line,
                text: chars[start..i].iter().collect::<String>().trim().to_owned(),
            });
            continue;
        }
        if c == '#' && fresh_line {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                if chars[i] == '\\' && chars.get(i + 1) == Some(&'\n') {
                    line += 1;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            tokens.push(Lexed {
                token: Token::Directive(chars[start..i].iter().collect()),
                line,
            });
            continue;
        }
        fresh_line = false;
        if c == '"' || c == '\'' {
            i += 1;
            loop {
                match chars.get(i) {
                    None => {
                        return Err(ImportError(format!("{file}:{line}: unterminated literal")));
                    }
                    Some('\\') => i += 1,
                    Some('\n') => line += 1,
                    Some(q) if *q == c => break,
                    Some(_) => {}
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(Lexed {
                token: Token::Word(chars[start..i].iter().collect()),
                line,
            });
            continue;
        }
        tokens.push(Lexed {
            token: Token::Punct(c),
            line,
        });
        i += 1;
    }
    Ok((tokens, comments))
}

// ==========================================================================
// Parser.
// ==========================================================================

/// Unresolved parse products, before struct names are known.
#[derive(Debug, Default)]
struct RawHeader {
    structs: Vec<(String, Vec<RawSlot>, usize)>,
    functions: Vec<(String, RawType, Vec<RawSlot>, usize)>,
    notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawSlot {
    name: Option<String>,
    ty: RawType,
    line: usize,
    comment: Option<String>,
}

/// Parse header text. `file` names it in messages.
///
/// # Errors
/// An unterminated comment or literal, or a header without any struct or function.
pub fn parse_header(text: &str, file: &str) -> Result<Header, ImportError> {
    let (tokens, comments) = lex(file, text)?;
    let mut raw = RawHeader::default();
    let mut i = 0_usize;
    while i < tokens.len() {
        let here = &tokens[i];
        match &here.token {
            Token::Directive(d) => {
                let rest = d.trim_start_matches('#').trim_start();
                if let Some(inc) = rest.strip_prefix("include") {
                    let inc = inc.trim();
                    if !["<stdint.h>", "<stdbool.h>", "<stddef.h>"].contains(&inc) {
                        raw.notes.push(format!(
                            "{file}:{}: #include {inc} is not read; types from it are unknown",
                            here.line
                        ));
                    }
                }
                i += 1;
            }
            Token::Word(w) if w == "typedef" => {
                let is_struct = matches!(tokens.get(i + 1), Some(Lexed { token: Token::Word(s), .. }) if s == "struct");
                let mut j = i + 2;
                if is_struct
                    && matches!(
                        tokens.get(j),
                        Some(Lexed {
                            token: Token::Word(_),
                            ..
                        })
                    )
                {
                    j += 1;
                }
                if is_struct
                    && matches!(
                        tokens.get(j),
                        Some(Lexed {
                            token: Token::Punct('{'),
                            ..
                        })
                    )
                {
                    i = parse_struct(file, &tokens, &comments, j + 1, &mut raw);
                } else {
                    i = skip_statement(&tokens, i);
                }
            }
            Token::Word(w)
                if w == "extern"
                    && matches!(
                        tokens.get(i + 1),
                        Some(Lexed {
                            token: Token::Punct('{'),
                            ..
                        })
                    ) =>
            {
                i += 2;
            }
            Token::Punct('}') | Token::Punct(';') => i += 1,
            _ => i = parse_declaration(file, &tokens, i, &mut raw),
        }
    }
    if raw.structs.is_empty() && raw.functions.is_empty() {
        return Err(ImportError(format!(
            "{file}: no typedef struct and no function prototype found"
        )));
    }
    let names: BTreeSet<String> = raw.structs.iter().map(|(n, _, _)| n.clone()).collect();
    let resolve_slot = |s: &RawSlot| Slot {
        name: s.name.clone(),
        ty: s.ty.resolve(&names),
        is_const: s.ty.is_const,
        line: s.line,
        comment: s.comment.clone(),
    };
    Ok(Header {
        structs: raw
            .structs
            .iter()
            .map(|(name, members, line)| Struct {
                name: name.clone(),
                members: members.iter().map(resolve_slot).collect(),
                line: *line,
            })
            .collect(),
        functions: raw
            .functions
            .iter()
            .map(|(name, returns, params, line)| Function {
                name: name.clone(),
                returns: returns.resolve(&names),
                params: params.iter().map(resolve_slot).collect(),
                line: *line,
            })
            .collect(),
        notes: raw.notes,
    })
}

/// Index just past the next `;` at brace/paren depth 0 (or past a balanced `{ … }` body).
fn skip_statement(tokens: &[Lexed], mut i: usize) -> usize {
    let mut depth = 0_i32;
    while i < tokens.len() {
        match tokens[i].token {
            Token::Punct('{' | '(') => depth += 1,
            Token::Punct('}') | Token::Punct(')') => {
                depth -= 1;
                if depth <= 0 && tokens[i].token == Token::Punct('}') {
                    return i + 1;
                }
            }
            Token::Punct(';') if depth <= 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Parse a struct body from just after `{`; returns the index after the closing `;`.
fn parse_struct(
    file: &str,
    tokens: &[Lexed],
    comments: &[Comment],
    start: usize,
    raw: &mut RawHeader,
) -> usize {
    let line = tokens.get(start).map_or(0, |t| t.line);
    let mut members = Vec::new();
    let mut i = start;
    let mut ok = true;
    while i < tokens.len() && tokens[i].token != Token::Punct('}') {
        if let Token::Directive(_) = tokens[i].token {
            raw.notes.push(format!(
                "{file}:{}: a directive inside a struct body; the struct is skipped",
                tokens[i].line
            ));
            ok = false;
            i += 1;
            continue;
        }
        let end = statement_end(tokens, i);
        match parse_slot(&tokens[i..end]) {
            Some(mut slot) if slot.name.is_some() => {
                // A comment right after the `;`, on its line, belongs to the member.
                slot.comment = tokens.get(end).and_then(|semi| {
                    comments
                        .iter()
                        .find(|c| c.after == end + 1 && c.line == semi.line)
                        .map(|c| c.text.clone())
                });
                members.push(slot);
            }
            _ => {
                raw.notes.push(format!(
                    "{file}:{}: unreadable struct member; the struct is skipped",
                    tokens[i].line
                ));
                ok = false;
            }
        }
        i = end + 1;
    }
    i += 1; // `}`
    let name = match tokens.get(i) {
        Some(Lexed {
            token: Token::Word(w),
            ..
        }) => Some(w.clone()),
        _ => None,
    };
    let end = skip_statement(tokens, i);
    match name {
        Some(name) if ok && !members.is_empty() => {
            if raw.structs.iter().any(|(n, _, _)| *n == name) {
                raw.notes.push(format!(
                    "{file}:{line}: struct {name} declared twice; the second is skipped"
                ));
            } else {
                raw.structs.push((name, members, line));
            }
        }
        Some(name) => raw
            .notes
            .push(format!("{file}:{line}: struct {name} skipped")),
        None => raw
            .notes
            .push(format!("{file}:{line}: anonymous typedef struct skipped")),
    }
    end
}

/// Index of the `;` that ends the statement starting at `i` (or `tokens.len()`).
fn statement_end(tokens: &[Lexed], mut i: usize) -> usize {
    while i < tokens.len()
        && tokens[i].token != Token::Punct(';')
        && tokens[i].token != Token::Punct('}')
    {
        i += 1;
    }
    i
}

/// `[const] T [*…] [name] [[N]]` → a slot, or `None` when it is not that shape.
fn parse_slot(tokens: &[Lexed]) -> Option<RawSlot> {
    let line = tokens.first()?.line;
    let mut words = Vec::new();
    let mut stars = 0_usize;
    let mut array = false;
    let mut name = None;
    let mut i = 0_usize;
    while i < tokens.len() {
        match &tokens[i].token {
            Token::Word(w) => {
                if stars > 0 || name.is_some() {
                    // A word after the stars is the name; a second one is not this grammar.
                    if name.is_some() {
                        return None;
                    }
                    name = Some(w.clone());
                } else {
                    words.push(w.clone());
                }
            }
            Token::Punct('*') => stars += 1,
            Token::Punct('[') => {
                array = true;
                while i < tokens.len() && tokens[i].token != Token::Punct(']') {
                    i += 1;
                }
            }
            _ => return None,
        }
        i += 1;
    }
    // Without stars, the last word is the name when more than one type word precedes it and
    // the words before it spell a type; `unsigned int n` → name `n`, `double` → unnamed.
    if name.is_none() && words.len() >= 2 {
        let head = words[..words.len() - 1].join(" ");
        let stripped: Vec<&str> = head.split(' ').filter(|w| *w != "const").collect();
        if CType::parse(&stripped.join(" ")).is_ok() || stripped.len() == 1 || stripped == ["void"]
        {
            name = words.pop();
        }
    }
    if words.is_empty() {
        return None;
    }
    let is_const = words.iter().any(|w| w == "const");
    Some(RawSlot {
        name,
        ty: RawType {
            words,
            stars,
            is_const,
            array,
        },
        line,
        comment: None,
    })
}

/// A top-level declaration: a prototype is recorded, anything else skipped.
fn parse_declaration(file: &str, tokens: &[Lexed], start: usize, raw: &mut RawHeader) -> usize {
    let line = tokens[start].line;
    let end = skip_statement(tokens, start);
    let stmt = &tokens[start..end];
    let Some(open) = stmt.iter().position(|t| t.token == Token::Punct('(')) else {
        return end;
    };
    let Some(close) = stmt.iter().rposition(|t| t.token == Token::Punct(')')) else {
        return end;
    };
    if open == 0 || close < open {
        return end;
    }
    let Token::Word(name) = &stmt[open - 1].token else {
        return end;
    };
    let is_definition = stmt.last().is_some_and(|t| t.token == Token::Punct('}'));
    if is_definition {
        raw.notes.push(format!(
            "{file}:{line}: {name} is defined inline and not exported; skipped"
        ));
        return end;
    }
    // Return type: the longest suffix of the words before the name that spells a type, so a
    // leading export macro is dropped.
    let head: Vec<&Lexed> = stmt[..open - 1]
        .iter()
        .filter(|t| !matches!(&t.token, Token::Word(w) if w == "extern" || w == "static" || w == "inline"))
        .collect();
    let stars = head.iter().filter(|t| t.token == Token::Punct('*')).count();
    let words: Vec<String> = head
        .iter()
        .filter_map(|t| match &t.token {
            Token::Word(w) => Some(w.clone()),
            _ => None,
        })
        .collect();
    let mut returns = None;
    for drop in 0..words.len() {
        let tail = &words[drop..];
        let candidate = RawType {
            words: tail.to_vec(),
            stars,
            is_const: false,
            array: false,
        };
        let plain: Vec<&str> = tail
            .iter()
            .map(String::as_str)
            .filter(|w| *w != "const")
            .collect();
        if plain == ["void"] || CType::parse(&plain.join(" ")).is_ok() || tail.len() == 1 {
            returns = Some(candidate);
            break;
        }
    }
    let Some(returns) = returns else {
        return end;
    };
    let mut params = Vec::new();
    let inner = &stmt[open + 1..close];
    if !inner.is_empty() && !(inner.len() == 1 && inner[0].token == Token::Word("void".to_owned()))
    {
        for group in inner.split(|t| t.token == Token::Punct(',')) {
            match parse_slot(group) {
                Some(slot) => params.push(slot),
                None => {
                    raw.notes.push(format!(
                        "{file}:{line}: {name}: a parameter the grammar does not read; the function is skipped"
                    ));
                    return end;
                }
            }
        }
    }
    raw.functions.push((name.clone(), returns, params, line));
    end
}

// ==========================================================================
// Proposal.
// ==========================================================================

/// A proposed descriptor and the guesses behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// The descriptor: `confirmed = true` when read from the recommended shape, else `false`.
    pub descriptor: Descriptor,
    /// Every guess, every deviation from the recommended shape and every skipped declaration.
    pub notes: Vec<String>,
    /// Read from the recommended shape, confirmed.
    pub from_shape: bool,
}

impl Proposal {
    /// TOML text: the notes as a leading comment, then the descriptor.
    ///
    /// # Errors
    /// Serialization failure (does not happen for a proposal).
    pub fn to_toml(&self) -> Result<String, ImportError> {
        let mut out = String::from(if self.from_shape {
            "# Read by `taktwerk import-header` from the recommended shape of the header: every\n\
             # role, shape and bound is stated there, so the descriptor is confirmed as read.\n"
        } else {
            "# Proposed by `taktwerk import-header`. Review every note, fix the descriptor and\n\
             # set `abi.confirmed = true`; an unconfirmed descriptor is refused at load.\n"
        });
        for note in &self.notes {
            out.push_str("# - ");
            out.push_str(note);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(
            &self
                .descriptor
                .to_toml()
                .map_err(|e| ImportError(e.to_string()))?,
        );
        Ok(out)
    }
}

/// What the caller knows about a header that its text does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// One function that serves as both init and step (a single entry point); which call it is
    /// told by a `phase` member or argument.
    pub entry: Option<String>,
    /// `(parameter, struct)`: an opaque `char *` or `void *` parameter that is really a pointer
    /// to this typedef struct of the header.
    pub arg_structs: Vec<(String, String)>,
    /// Require the recommended shape: a header that deviates from it is an error listing every
    /// deviation, instead of a heuristic proposal.
    pub require_shape: bool,
}

/// Read and parse `path`, then [`propose`] with the file stem as model name.
///
/// # Errors
/// The file cannot be read or parsed, or declares no usable function.
pub fn import_header(path: &Path) -> Result<Proposal, ImportError> {
    import_header_with(path, &ImportOptions::default())
}

/// [`import_header`] with [`ImportOptions`].
///
/// # Errors
/// As [`import_header`], plus an entry, parameter or struct the options name but the header
/// does not declare.
pub fn import_header_with(path: &Path, options: &ImportOptions) -> Result<Proposal, ImportError> {
    let file = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| ImportError(format!("{file}: {e}")))?;
    let header = parse_header(&text, &file)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".to_owned());
    propose_with(&header, &name, options)
}

/// Guess roles, dimensions and lengths for a parsed header.
///
/// # Errors
/// No function could serve as the step.
pub fn propose(header: &Header, name: &str) -> Result<Proposal, ImportError> {
    propose_with(header, name, &ImportOptions::default())
}

/// [`propose`] with [`ImportOptions`].
///
/// # Errors
/// No function could serve as the step, or the options name something the header lacks.
pub fn propose_with(
    header: &Header,
    name: &str,
    options: &ImportOptions,
) -> Result<Proposal, ImportError> {
    let explicit = options.entry.is_some() || !options.arg_structs.is_empty();
    if options.require_shape && explicit {
        return Err(ImportError(
            "the recommended shape takes no entry point and no argument structs".to_owned(),
        ));
    }
    let mut notes = header.notes.clone();
    if !explicit {
        match crate::shape::read(header, name) {
            Ok(descriptor) => {
                return Ok(Proposal {
                    descriptor,
                    notes,
                    from_shape: true,
                });
            }
            Err(deviations) if options.require_shape => {
                return Err(ImportError(format!(
                    "not the recommended shape:\n  - {}",
                    deviations.join("\n  - ")
                )));
            }
            Err(deviations) => notes.extend(
                deviations
                    .into_iter()
                    .map(|d| format!("not the recommended shape: {d}")),
            ),
        }
    }
    let mut b = Builder {
        header: Some(header),
        notes,
        ..Builder::default()
    };
    for (param, st) in &options.arg_structs {
        if !header.structs.iter().any(|s| &s.name == st) {
            return Err(ImportError(format!(
                "parameter {param}: struct {st} is not declared in the header"
            )));
        }
        b.arg_structs.insert(param.clone(), st.clone());
        let l = param.to_lowercase();
        if l.contains("out") {
            b.hints.insert(st.clone(), Causality::Output);
        } else if l.contains("in") {
            b.hints.insert(st.clone(), Causality::Input);
        }
    }
    if let Some(entry) = &options.entry {
        return propose_single_entry(b, header, name, entry);
    }
    let usable: Vec<&Function> = header.functions.iter().filter(|f| b.usable(f)).collect();
    let find = |keys: &[&str], taken: &[&str]| -> Option<&Function> {
        usable.iter().copied().find(|f| {
            !taken.contains(&f.name.as_str())
                && keys.iter().any(|k| f.name.to_lowercase().contains(k))
        })
    };
    let terminate = find(TERMINATE_KEYS, &[]);
    let taken: Vec<&str> = terminate.iter().map(|f| f.name.as_str()).collect();
    let step = find(STEP_KEYS, &taken);
    let mut taken2 = taken.clone();
    taken2.extend(step.iter().map(|f| f.name.as_str()));
    let init = find(INIT_KEYS, &taken2);
    let step = step.or_else(|| {
        let rest: Vec<&Function> = usable
            .iter()
            .copied()
            .filter(|f| !taken2.contains(&f.name.as_str()) && init.is_none_or(|i| i.name != f.name))
            .collect();
        match rest.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    });
    let Some(step) = step else {
        return Err(ImportError(format!(
            "no step function found among: {}",
            header
                .functions
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    b.notes.push(format!("step = {} (by name)", step.name));
    let init_call = match init {
        Some(f) => {
            b.notes.push(format!("init = {} (by name)", f.name));
            b.call(f)
        }
        None => {
            b.notes
                .push("no init function found: set abi.init.symbol".to_owned());
            Call {
                symbol: String::new(),
                returns: Returns::Int,
                args: Vec::new(),
            }
        }
    };
    let step_call = b.call(step);
    let terminate_call = terminate.map(|f| {
        b.notes.push(format!("terminate = {} (by name)", f.name));
        b.call(f)
    });
    Ok(b.finish(name, init_call, step_call, terminate_call))
}

/// The single-entry proposal: one function for init and step.
fn propose_single_entry(
    mut b: Builder<'_>,
    header: &Header,
    name: &str,
    entry: &str,
) -> Result<Proposal, ImportError> {
    let Some(f) = header.functions.iter().find(|f| f.name == entry) else {
        return Err(ImportError(format!(
            "entry {entry} is not declared in the header"
        )));
    };
    for param in b.arg_structs.keys() {
        if !f.params.iter().any(|p| p.name.as_deref() == Some(param)) {
            return Err(ImportError(format!("{entry} has no parameter {param}")));
        }
    }
    if !b.usable(f) {
        return Err(ImportError(format!(
            "entry {entry} cannot be called by the adapter (see notes: {})",
            b.notes.join("; ")
        )));
    }
    b.single_entry = true;
    // Lengths the library reports in an output struct are dimensions before any member names
    // its length after them.
    let mapped: Vec<String> = b.arg_structs.values().cloned().collect();
    for st in &mapped {
        if b.hint(st) != Some(Causality::Output) {
            continue;
        }
        if let Some(s) = header.structs.iter().find(|s| &s.name == st) {
            for m in &s.members {
                if let (Some(n), ParsedType::Scalar(t)) = (&m.name, &m.ty) {
                    if t.is_integer() && !looks_like_flag(n) {
                        if let Some(dim) = reported_dim(n) {
                            b.dims.entry(dim).or_insert(*t);
                        }
                    }
                }
            }
        }
    }
    b.notes.push(format!(
        "init = step = {entry} (single entry point): a phase member or argument must tell the \
         library which call runs"
    ));
    let call = b.call(f);
    if !call.args.iter().any(|a| a.phase.is_some())
        && !b
            .structs
            .values()
            .any(|s| s.members.iter().any(|m| m.phase.is_some()))
    {
        b.notes.push(
            "no flag-like member or argument found: add a `phase = { init = …, step = … }`"
                .to_owned(),
        );
    }
    b.notes.push(
        "no terminate with a single entry point; add one by hand if the library has it".to_owned(),
    );
    Ok(b.finish(name, call.clone(), call, None))
}

impl Builder<'_> {
    /// Assemble the unconfirmed descriptor.
    fn finish(
        self,
        name: &str,
        init_call: Call,
        step_call: Call,
        terminate_call: Option<Call>,
    ) -> Proposal {
        let mut b = self;
        let instances = if b.has_handle {
            b.notes
                .push("a void * handle was found: instances = multiple".to_owned());
            Instances::Multiple
        } else {
            b.notes
                .push("no handle: instances = single (state assumed in globals)".to_owned());
            Instances::Single
        };
        if !b.dims.is_empty() {
            b.notes.push(format!(
                "dimensions guessed from integer names: {}; min = 1 assumed",
                b.dims.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        b.notes
            .push("matrices: set two shape entries and `layout` by hand".to_owned());
        let descriptor = Descriptor {
            interface: ModelInterface {
                name: name.to_owned(),
                dimensions: b
                    .dims
                    .keys()
                    .map(|n| Dimension {
                        name: n.clone(),
                        min: Some(1),
                        max: None,
                        default: None,
                    })
                    .collect(),
                variables: b.variables.clone(),
                instances,
            },
            abi: Abi {
                confirmed: false,
                library: None,
                ok_codes: vec![0],
                init: init_call,
                step: step_call,
                terminate: terminate_call,
                structs: b.structs.clone(),
            },
        };
        Proposal {
            descriptor,
            notes: b.notes,
            from_shape: false,
        }
    }
}

const TERMINATE_KEYS: &[&str] = &[
    "term", "free", "close", "destroy", "release", "deinit", "cleanup", "finish", "delete", "end",
];
const STEP_KEYS: &[&str] = &[
    "step", "update", "run", "compute", "calc", "tick", "cycle", "process", "eval", "execute",
];
const INIT_KEYS: &[&str] = &[
    "init", "create", "open", "setup", "start", "new", "alloc", "reset",
];

/// Accumulates variables, dimensions and structs while calls are proposed.
#[derive(Default)]
struct Builder<'h> {
    header: Option<&'h Header>,
    notes: Vec<String>,
    variables: Vec<Variable>,
    dims: BTreeMap<String, CType>,
    structs: BTreeMap<String, StructSpec>,
    has_handle: bool,
    /// Parameter name → struct, from [`ImportOptions::arg_structs`].
    arg_structs: BTreeMap<String, String>,
    /// Struct → causality hint from the parameter that carries it.
    hints: BTreeMap<String, Causality>,
    /// Proposing for a single entry point.
    single_entry: bool,
}

impl<'h> Builder<'h> {
    fn usable(&mut self, f: &Function) -> bool {
        let returns_ok = matches!(
            f.returns,
            ParsedType::Void
                | ParsedType::Scalar(CType {
                    base: Some(ScalarType::I32),
                    pointer: false
                })
        );
        if !returns_ok {
            self.notes.push(format!(
                "{}: return type is neither int nor void; skipped",
                f.name
            ));
            return false;
        }
        for p in &f.params {
            let ok = match &p.ty {
                ParsedType::Scalar(_) | ParsedType::VoidPtrPtr => true,
                ParsedType::Struct { pointer, .. } => *pointer,
                ParsedType::Void | ParsedType::Other(_) => false,
            };
            if !ok {
                self.notes.push(format!(
                    "{}: parameter {} has a type the adapter cannot pass; skipped",
                    f.name,
                    p.name.as_deref().unwrap_or("?")
                ));
                return false;
            }
        }
        true
    }

    /// Dimension slots of a list, in order: scalar integers whose name looks like a length.
    fn dims_in(slots: &[Slot]) -> Vec<(usize, String, CType)> {
        slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match (&s.name, &s.ty) {
                (Some(n), ParsedType::Scalar(t))
                    if !t.pointer && t.is_integer() && looks_like_dim(n) =>
                {
                    Some((i, n.clone(), *t))
                }
                _ => None,
            })
            .collect()
    }

    /// Guess which dimension sizes pointer `name` at position `at` in `slots`.
    fn guess_len(&mut self, slots: &[Slot], at: usize, name: &str, context: &str) -> Vec<Dim> {
        let local = Self::dims_in(slots);
        let candidates = [
            format!("n{name}"),
            format!("n_{name}"),
            format!("{name}_len"),
            format!("{name}_n"),
            format!("{name}_size"),
            format!("{name}_count"),
            format!("len_{name}"),
            format!("num_{name}"),
            format!("size_{name}"),
        ];
        let known: Vec<&String> = local
            .iter()
            .map(|(_, n, _)| n)
            .chain(self.dims.keys())
            .collect();
        let hit = candidates.iter().find(|c| known.contains(c)).cloned();
        let hit = hit.or_else(|| match local.as_slice() {
            [(_, only, _)] => Some(only.clone()),
            [] if self.dims.len() == 1 => self.dims.keys().next().cloned(),
            [] => None,
            many => many
                .iter()
                .rev()
                .find(|(i, _, _)| *i < at)
                .or_else(|| many.first())
                .map(|(_, n, _)| n.clone()),
        });
        match hit {
            Some(dim) => {
                self.notes
                    .push(format!("{context}: {name} sized by {dim} (guessed)"));
                vec![Dim::Symbol(dim)]
            }
            None => {
                self.notes.push(format!(
                    "{context}: {name} has no length candidate; declared scalar"
                ));
                Vec::new()
            }
        }
    }

    fn register_dims(&mut self, slots: &[Slot]) {
        for (_, name, ty) in Self::dims_in(slots) {
            self.dims.entry(name).or_insert(ty);
        }
    }

    /// Add or reuse a variable; returns its name.
    fn variable(
        &mut self,
        name: &str,
        ty: ScalarType,
        shape: Vec<Dim>,
        causality: Causality,
        context: &str,
    ) -> String {
        if let Some(existing) = self.variables.iter().find(|v| v.name == name) {
            if existing.ty == ty {
                return name.to_owned();
            }
            let renamed = format!("{context}_{name}");
            self.notes.push(format!(
                "{context}: {name} clashes with another {name}; named {renamed}"
            ));
            return self.variable(&renamed, ty, shape, causality, context);
        }
        self.variables.push(Variable {
            name: name.to_owned(),
            causality,
            ty,
            shape,
            layout: Layout::RowMajor,
            unit: None,
            description: None,
        });
        name.to_owned()
    }

    fn builtin_of(name: &str, ty: CType) -> Option<Builtin> {
        if ty.pointer || ty.base != Some(ScalarType::F64) {
            return None;
        }
        match name.to_lowercase().as_str() {
            "dt" | "h" | "ts" | "tstep" | "step" | "step_size" | "stepsize" | "dt_s" | "period"
            | "sample_time" | "t_s" => Some(Builtin::StepSize),
            "t" | "time" | "now" | "t_now" | "clock" => Some(Builtin::Time),
            _ => None,
        }
    }

    fn call(&mut self, f: &Function) -> Call {
        self.register_dims(&f.params);
        let mut args = Vec::with_capacity(f.params.len());
        for (i, p) in f.params.iter().enumerate() {
            let pname = p.name.clone().unwrap_or_else(|| format!("arg{i}"));
            let context = f.name.clone();
            if let Some(st) = self.arg_structs.get(&pname).cloned() {
                self.notes
                    .push(format!("{context}: {pname} carries struct {st} (as told)"));
                self.struct_spec(&st);
                args.push(Arg {
                    struct_: Some(st),
                    ..Arg::default()
                });
                continue;
            }
            if let ParsedType::Scalar(
                t @ CType {
                    base: Some(_),
                    pointer: false,
                },
            ) = &p.ty
            {
                if t.is_integer() && looks_like_flag(&pname) {
                    self.notes.push(format!(
                        "{context}: {pname} looks like an init flag: phase = {{ init = 1, step = 0 }} (guessed)"
                    ));
                    args.push(Arg {
                        phase: Some(INIT_FLAG),
                        ty: Some(*t),
                        ..Arg::default()
                    });
                    continue;
                }
                if self.single_entry && i == 0 && t.is_integer() && !self.dims.contains_key(&pname)
                {
                    self.notes.push(format!(
                        "{context}: {pname} is proposed as a constant: TODO set the value the \
                         library expects (0 is a placeholder)"
                    ));
                    args.push(Arg {
                        const_: Some(Number::Int(0)),
                        ty: Some(*t),
                        ..Arg::default()
                    });
                    continue;
                }
            }
            let arg = match &p.ty {
                ParsedType::VoidPtrPtr => {
                    self.has_handle = true;
                    Arg {
                        handle: Some(Handle::Out),
                        ..Arg::default()
                    }
                }
                ParsedType::Scalar(CType { base: None, .. }) => {
                    self.has_handle = true;
                    Arg {
                        handle: Some(Handle::In),
                        ..Arg::default()
                    }
                }
                ParsedType::Struct { name, .. } => {
                    self.struct_spec(name);
                    Arg {
                        struct_: Some(name.clone()),
                        ..Arg::default()
                    }
                }
                ParsedType::Scalar(CType {
                    base: Some(base),
                    pointer: true,
                }) => {
                    let shape = self.guess_len(&f.params, i, &pname, &context);
                    let causality = if p.is_const {
                        Causality::Input
                    } else {
                        Causality::Output
                    };
                    self.notes.push(format!(
                        "{context}: {pname} is {} ({})",
                        if p.is_const { "an input" } else { "an output" },
                        if p.is_const { "const" } else { "non-const" }
                    ));
                    let var = self.variable(&pname, *base, shape, causality, &context);
                    Arg {
                        array: Some(var),
                        ..Arg::default()
                    }
                }
                ParsedType::Scalar(
                    t @ CType {
                        base: Some(base),
                        pointer: false,
                    },
                ) => {
                    if self.dims.contains_key(&pname) {
                        Arg {
                            dim: Some(pname.clone()),
                            ty: Some(*t),
                            ..Arg::default()
                        }
                    } else if let Some(builtin) = Self::builtin_of(&pname, *t) {
                        self.notes
                            .push(format!("{context}: {pname} taken as {builtin:?}"));
                        Arg {
                            builtin: Some(builtin),
                            ..Arg::default()
                        }
                    } else {
                        self.notes.push(format!(
                            "{context}: {pname} by value is a parameter (guessed)"
                        ));
                        let var = self.variable(
                            &pname,
                            *base,
                            Vec::new(),
                            Causality::Parameter,
                            &context,
                        );
                        Arg {
                            value: Some(var),
                            ..Arg::default()
                        }
                    }
                }
                ParsedType::Void | ParsedType::Other(_) => Arg::default(),
            };
            args.push(arg);
        }
        Call {
            symbol: f.name.clone(),
            returns: if f.returns == ParsedType::Void {
                Returns::Void
            } else {
                Returns::Int
            },
            args,
        }
    }

    /// Causality hint for a struct: by its name, else by the parameter that carries it.
    fn hint(&self, name: &str) -> Option<Causality> {
        let lower = name.to_lowercase();
        if lower.contains("out") {
            Some(Causality::Output)
        } else if lower.contains("in") {
            Some(Causality::Input)
        } else {
            self.hints.get(name).copied()
        }
    }

    fn struct_spec(&mut self, name: &str) {
        if self.structs.contains_key(name) {
            return;
        }
        let Some(st) = self
            .header
            .and_then(|h| h.structs.iter().find(|s| s.name == name))
        else {
            self.notes
                .push(format!("struct {name} is not declared in this header"));
            self.structs.insert(
                name.to_owned(),
                StructSpec {
                    members: Vec::new(),
                },
            );
            return;
        };
        let st = st.clone();
        self.register_dims(&st.members);
        let struct_hint = self.hint(name);
        let mut members = Vec::with_capacity(st.members.len());
        for (i, m) in st.members.iter().enumerate() {
            let mname = m.name.clone().unwrap_or_default();
            let ParsedType::Scalar(t) = &m.ty else {
                self.notes.push(format!("struct {name}: member {mname} has a type the layout cannot carry; set it by hand"));
                continue;
            };
            let Some(base) = t.base else {
                members.push(Member {
                    name: mname,
                    ty: *t,
                    variable: None,
                    dim: None,
                    builtin: None,
                    const_: None,
                    phase: None,
                    reported: false,
                });
                continue;
            };
            if t.is_integer() && looks_like_flag(&mname) {
                self.notes.push(format!(
                    "struct {name}: {mname} looks like an init flag: phase = {{ init = 1, step = 0 }} (guessed)"
                ));
                members.push(Member {
                    name: mname,
                    ty: *t,
                    variable: None,
                    dim: None,
                    builtin: None,
                    const_: None,
                    phase: Some(INIT_FLAG),
                    reported: false,
                });
                continue;
            }
            if t.pointer && t.is_integer() && struct_hint == Some(Causality::Output) {
                if let Some(dim) = reported_dim(&mname) {
                    self.notes.push(format!(
                        "struct {name}: {mname} looks like a length the library reports: \
                         dim = {dim}, reported = true (guessed); set the dimension's max, its \
                         buffers are allocated at that size"
                    ));
                    self.dims.entry(dim.clone()).or_insert(*t);
                    members.push(Member {
                        name: mname,
                        ty: *t,
                        variable: None,
                        dim: Some(dim),
                        builtin: None,
                        const_: None,
                        phase: None,
                        reported: true,
                    });
                    continue;
                }
            }
            let member = if t.pointer {
                if base == ScalarType::U8 {
                    self.notes.push(format!(
                        "struct {name}: {mname} is a byte buffer; if it carries text, give its \
                         variable a literal shape = [capacity] (bytes, NUL included)"
                    ));
                }
                let shape = self.guess_len(&st.members, i, &mname, name);
                let causality = if m.is_const {
                    Causality::Input
                } else {
                    struct_hint.unwrap_or_else(|| {
                        let l = mname.to_lowercase();
                        if l.starts_with('y') || l.contains("out") {
                            Causality::Output
                        } else {
                            Causality::Input
                        }
                    })
                };
                if shape.is_empty() && !m.is_const && causality == Causality::Input {
                    self.notes.push(format!(
                        "struct {name}: {mname} points at one value; if the library writes it \
                         back, make its variable an output"
                    ));
                }
                let var = self.variable(&mname, base, shape, causality, name);
                Member {
                    name: mname,
                    ty: *t,
                    variable: Some(var),
                    dim: None,
                    builtin: None,
                    const_: None,
                    phase: None,
                    reported: false,
                }
            } else if self.dims.contains_key(&mname) {
                Member {
                    name: mname.clone(),
                    ty: *t,
                    variable: None,
                    dim: Some(mname),
                    builtin: None,
                    const_: None,
                    phase: None,
                    reported: false,
                }
            } else if let Some(builtin) = Self::builtin_of(&mname, *t) {
                self.notes
                    .push(format!("struct {name}: {mname} taken as {builtin:?}"));
                Member {
                    name: mname,
                    ty: *t,
                    variable: None,
                    dim: None,
                    builtin: Some(builtin),
                    const_: None,
                    phase: None,
                    reported: false,
                }
            } else {
                let causality = struct_hint.unwrap_or(Causality::Parameter);
                self.notes.push(format!(
                    "struct {name}: {mname} by value is {causality:?} (guessed)"
                ));
                let var = self.variable(&mname, base, Vec::new(), causality, name);
                Member {
                    name: mname,
                    ty: *t,
                    variable: Some(var),
                    dim: None,
                    builtin: None,
                    const_: None,
                    phase: None,
                    reported: false,
                }
            };
            members.push(member);
        }
        self.structs.insert(name.to_owned(), StructSpec { members });
    }
}

/// `phase = { init = 1, step = 0 }`, proposed for flag-like integers.
const INIT_FLAG: PhaseValues = PhaseValues {
    init: Number::Int(1),
    step: Number::Int(0),
};

/// Whether an integer's name reads as an init/step flag: `flag`, `init_flag`, `first_call`, …
fn looks_like_flag(name: &str) -> bool {
    let l = name.to_lowercase();
    l.contains("flag")
        || l == "init"
        || l == "first"
        || l.ends_with("_ini")
        || l.ends_with("_init")
        || l.starts_with("is_init")
        || l.starts_with("first_")
}

/// The dimension a size-like integer names: `nu` for `dim_nu` or `size_nu`, the whole name for
/// `nx` or `x_len`, `None` when it does not read as a length.
fn reported_dim(name: &str) -> Option<String> {
    let tail = name.rsplit('_').next().unwrap_or(name);
    let generic = ["len", "size", "count", "num", "dim", "n"];
    if tail != name && looks_like_dim(tail) && !generic.contains(&tail.to_lowercase().as_str()) {
        Some(tail.to_owned())
    } else if looks_like_dim(name) {
        Some(name.to_owned())
    } else {
        None
    }
}

/// Whether an integer's name reads as a length: `n`, `nx`, `num_x`, `len`, `x_len`, …
fn looks_like_dim(name: &str) -> bool {
    let l = name.to_lowercase();
    let prefixes = ["num", "len", "size", "count", "dim"];
    let suffixes = ["_len", "_n", "_size", "_count", "_dim"];
    if l == "n" || l == "m" || l == "rows" || l == "cols" {
        return true;
    }
    if prefixes.iter().any(|p| l.starts_with(p)) || suffixes.iter().any(|s| l.ends_with(s)) {
        return true;
    }
    // `nx`, `nu`, `ny`: `n` plus up to two characters; longer `n…` names only when the second
    // character cannot start a word (`nmax`, `n_in`, not `name`).
    let mut chars = l.chars();
    chars.next() == Some('n')
        && (l.len() <= 3
            || (l.len() <= 5
                && chars
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '_' || !"aeiou".contains(c))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"
#ifndef PI_H
#define PI_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* a size-generic PI controller over arrays */
typedef struct {
    int n;            // number of channels
    const double *kp; /* gains */
    double ki;
} pi_params;

int pi_create(void **handle, const pi_params *p, double dt);
int pi_step(void *handle, const double *sp, const double *pv, double *out, int n, double t);
void pi_destroy(void *handle);
int helper(int (*cb)(int));
#ifdef __cplusplus
}
#endif
#endif
"#;

    #[test]
    fn structs_and_prototypes_are_read() {
        let h = parse_header(HEADER, "pi.h").unwrap();
        assert_eq!(h.structs.len(), 1);
        assert_eq!(h.structs[0].members.len(), 3);
        assert_eq!(
            h.structs[0].members[1].ty,
            ParsedType::Scalar(CType::parse("const double *").unwrap())
        );
        assert!(h.structs[0].members[1].is_const);
        let names: Vec<&str> = h.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["pi_create", "pi_step", "pi_destroy"]);
        assert!(
            h.notes.iter().any(|n| n.contains("helper")),
            "{:?}",
            h.notes
        );
        assert_eq!(h.functions[0].params[0].ty, ParsedType::VoidPtrPtr);
        assert_eq!(
            h.functions[0].params[1].ty,
            ParsedType::Struct {
                name: "pi_params".to_owned(),
                pointer: true
            }
        );
        assert_eq!(h.functions[2].returns, ParsedType::Void);
    }

    #[test]
    fn the_proposal_guesses_roles_lengths_and_handles() {
        let h = parse_header(HEADER, "pi.h").unwrap();
        let p = propose(&h, "pi").unwrap();
        let d = &p.descriptor;
        assert!(!d.abi.confirmed);
        assert_eq!(d.abi.init.symbol, "pi_create");
        assert_eq!(d.abi.step.symbol, "pi_step");
        assert_eq!(d.abi.terminate.as_ref().unwrap().symbol, "pi_destroy");
        assert_eq!(d.abi.terminate.as_ref().unwrap().returns, Returns::Void);
        assert_eq!(d.interface.instances, Instances::Multiple);
        assert_eq!(d.interface.dimensions.len(), 1);
        assert_eq!(d.interface.dimensions[0].name, "n");
        let sp = d
            .interface
            .variables
            .iter()
            .find(|v| v.name == "sp")
            .unwrap();
        assert_eq!(sp.shape, vec![Dim::Symbol("n".to_owned())]);
        assert_eq!(sp.causality, Causality::Input);
        let out = d
            .interface
            .variables
            .iter()
            .find(|v| v.name == "out")
            .unwrap();
        assert_eq!(out.causality, Causality::Output);
        let kp = d
            .interface
            .variables
            .iter()
            .find(|v| v.name == "kp")
            .unwrap();
        assert_eq!(kp.shape, vec![Dim::Symbol("n".to_owned())]);
        assert_eq!(d.abi.init.args[0].handle, Some(Handle::Out));
        assert_eq!(d.abi.init.args[2].builtin, Some(Builtin::StepSize));
        assert_eq!(d.abi.step.args[5].builtin, Some(Builtin::Time));
        assert_eq!(d.abi.step.args[4].dim.as_deref(), Some("n"));
        assert!(p.notes.iter().any(|n| n.contains("helper")));
        // The text round-trips, and stays refused until confirmed.
        let text = p.to_toml().unwrap();
        let back = Descriptor::parse(&text).unwrap();
        assert_eq!(&back, d);
        assert!(back.validate().is_err());
        let mut confirmed = back;
        confirmed.abi.confirmed = true;
        confirmed.validate().unwrap();
    }

    #[test]
    fn dimension_names() {
        for yes in [
            "n",
            "nx",
            "nu",
            "num_states",
            "len",
            "x_len",
            "rows",
            "n_in",
        ] {
            assert!(looks_like_dim(yes), "{yes}");
        }
        for no in ["name", "node", "kp", "x", "flag"] {
            assert!(!looks_like_dim(no), "{no}");
        }
    }
}
