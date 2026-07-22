//! The recursive-descent type parser — a faithful port of phpstan/phpdoc-parser's
//! `TypeParser` algorithm (ADR-0029).
//!
//! The port preserves the details that decide compatibility:
//! - `parse` vs `subParse`: top-level union/intersection parsing stops at a
//!   *different* operator and leaves it as trailing input (`A & B | C` yields
//!   `(A & B)` with `| C` unconsumed), whereas the parenthesized `subParse`
//!   variant is newline-tolerant and single-operator.
//! - horizontal-whitespace sensitivity: `array{…}` is a shape but `array {…}` is
//!   the identifier `array`; `T[K]` is offset access but `T [K]` is not.
//! - the callable-vs-generic and const-fetch save-point/backtrack dance.
//! - the `<tag>…</tag>` HTML heuristic that makes `Foo <p>desc` stop at `Foo`.
//!
//! The parser never panics on input: any construct it cannot accept yields a
//! [`ParseError`]. Callers treat an error (and, in the wider design, a
//! [`crate::ast::TypeKind::Unsupported`] node) as "no envelope" — silence.

use crate::ast::*;
use crate::lexer::{Token, TokenKind, tokenize};

/// A parse failure. Carries a message and the byte offset/line at which the
/// unexpected token sits. Messages are informative only — compatibility is
/// judged on *whether* we reject, not on message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub offset: u32,
    pub line: u32,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at offset {} on line {}",
            self.message, self.offset, self.line
        )
    }
}

impl std::error::Error for ParseError {}

type PResult<T> = Result<T, ParseError>;

/// The outcome of parsing a whole type string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParse {
    /// The parsed type.
    pub ty: Type,
    /// `true` when the parse consumed the entire input; `false` when a type
    /// prefix was parsed and trailing tokens remain (the reference's "partial"
    /// case — e.g. a `@param` type followed by `$name` and a description).
    pub at_end: bool,
    /// Byte length of the consumed prefix (the end offset of the parsed type).
    pub consumed: u32,
}

/// Parse a full type-expression string.
///
/// Returns the parsed type plus whether all input was consumed. Mirrors the
/// reference `TypeParser::parse`, which parses a type prefix and does not itself
/// require the input to be exhausted.
pub fn parse_type(input: &str) -> PResult<TypeParse> {
    let tokens = tokenize(input);
    let mut p = Parser::new(tokens);
    let ty = p.parse()?;
    let at_end = p.cur().kind == TokenKind::End;
    let consumed = p.end_offset();
    Ok(TypeParse {
        ty,
        at_end,
        consumed,
    })
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        let mut p = Parser { tokens, index: 0 };
        p.skip_irrelevant();
        p
    }

    // --- token cursor (mirrors TokenIterator) ---------------------------------

    fn cur(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn cur_kind(&self) -> TokenKind {
        self.tokens[self.index].kind
    }

    fn cur_value(&self) -> &str {
        &self.tokens[self.index].value
    }

    fn is_kind(&self, k: TokenKind) -> bool {
        self.cur_kind() == k
    }

    fn is_value(&self, v: &str) -> bool {
        self.cur_value() == v
    }

    fn cur_offset(&self) -> u32 {
        self.tokens[self.index].start
    }

    fn cur_line(&self) -> u32 {
        self.tokens[self.index].line
    }

    fn next(&mut self) {
        self.index += 1;
        self.skip_irrelevant();
    }

    fn skip_irrelevant(&mut self) {
        while self.tokens[self.index].kind == TokenKind::HorizontalWs
            && self.index + 1 < self.tokens.len()
        {
            self.index += 1;
        }
    }

    fn is_preceded_by_hws(&self) -> bool {
        self.index > 0 && self.tokens[self.index - 1].kind == TokenKind::HorizontalWs
    }

    fn skipped_hws(&self) -> &str {
        if self.index > 0 && self.tokens[self.index - 1].kind == TokenKind::HorizontalWs {
            &self.tokens[self.index - 1].value
        } else {
            ""
        }
    }

    /// End offset of the last relevant (non-skipped) token before the cursor.
    fn end_offset(&self) -> u32 {
        let mut k = self.index;
        while k > 0 && self.tokens[k - 1].kind == TokenKind::HorizontalWs {
            k -= 1;
        }
        if k == 0 {
            self.tokens[self.index].start
        } else {
            self.tokens[k - 1].end
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            offset: self.cur_offset(),
            line: self.cur_line(),
        }
    }

    fn consume(&mut self, k: TokenKind) -> PResult<()> {
        if self.cur_kind() != k {
            return Err(self.error(format!("expected {k:?}, found {:?}", self.cur_kind())));
        }
        self.next();
        Ok(())
    }

    fn try_consume(&mut self, k: TokenKind) -> bool {
        if self.cur_kind() == k {
            self.next();
            true
        } else {
            false
        }
    }

    fn try_consume_value(&mut self, v: &str) -> bool {
        if self.cur_value() == v {
            self.next();
            true
        } else {
            false
        }
    }

    fn save(&self) -> usize {
        self.index
    }

    fn restore(&mut self, sp: usize) {
        self.index = sp;
    }

    /// Mirrors `skipNewLineTokensAndConsumeComments`.
    fn skip_ws_comments(&mut self) {
        if self.cur_kind() == TokenKind::Comment {
            self.next();
        }
        if self.cur_kind() != TokenKind::PhpdocEol {
            return;
        }
        loop {
            let found = self.try_consume(TokenKind::PhpdocEol);
            if self.cur_kind() == TokenKind::Comment {
                self.next();
            }
            if !found {
                break;
            }
        }
    }

    // --- helpers for span construction ---------------------------------------

    fn spanned(&self, start: u32, kind: TypeKind) -> Type {
        Type::new(Span::new(start, self.end_offset()), kind)
    }

    // --- grammar (mirrors TypeParser) ----------------------------------------

    /// `TypeParser::parse` — top-level type with the union/intersection
    /// save-point/backtrack behaviour.
    fn parse(&mut self) -> PResult<Type> {
        let start = self.cur_offset();
        if self.is_kind(TokenKind::Nullable) {
            return self.parse_nullable(start);
        }

        let ty = self.parse_atomic()?;

        // First attempt is speculative (the reference wraps it in try/catch):
        // a failure or "no operator" rolls back and retries. The retry is NOT
        // guarded, so its error propagates — e.g. `Foo &` (a `&` with nothing
        // after) is a hard parse error, not a bare `Foo`.
        let sp = self.save();
        self.skip_ws_comments();
        if let Ok(Some(t)) = self.enrich_union_or_intersection(ty.clone()) {
            return Ok(t);
        }
        self.restore(sp);
        match self.enrich_union_or_intersection(ty.clone())? {
            Some(t) => Ok(t),
            None => Ok(ty),
        }
    }

    fn enrich_union_or_intersection(&mut self, ty: Type) -> PResult<Option<Type>> {
        if self.is_kind(TokenKind::Union) {
            Ok(Some(self.parse_union(ty)?))
        } else if self.is_kind(TokenKind::Intersection) {
            Ok(Some(self.parse_intersection(ty)?))
        } else {
            Ok(None)
        }
    }

    /// `TypeParser::subParse` — parenthesized/nested type; handles conditionals
    /// and the greedy single-operator union/intersection variants.
    fn sub_parse(&mut self) -> PResult<Type> {
        let start = self.cur_offset();
        if self.is_kind(TokenKind::Nullable) {
            return self.parse_nullable(start);
        }
        if self.is_kind(TokenKind::Variable) {
            let name = self.cur_value().to_owned();
            return self.parse_conditional_for_parameter(start, name);
        }
        let ty = self.parse_atomic()?;
        if self.is_value("is") {
            return self.parse_conditional(start, ty);
        }
        self.skip_ws_comments();
        if self.is_kind(TokenKind::Union) {
            self.sub_parse_union(start, ty)
        } else if self.is_kind(TokenKind::Intersection) {
            self.sub_parse_intersection(start, ty)
        } else {
            Ok(ty)
        }
    }

    fn parse_nullable(&mut self, start: u32) -> PResult<Type> {
        self.consume(TokenKind::Nullable)?;
        let inner = self.parse_atomic()?;
        Ok(self.spanned(start, TypeKind::Nullable(Box::new(inner))))
    }

    fn parse_union(&mut self, first: Type) -> PResult<Type> {
        let start = first.span.start;
        let mut types = vec![first];
        while self.try_consume(TokenKind::Union) {
            types.push(self.parse_atomic()?);
            let sp = self.save();
            self.skip_ws_comments();
            if !self.is_kind(TokenKind::Union) {
                self.restore(sp);
                break;
            }
        }
        Ok(self.spanned(
            start,
            TypeKind::Union {
                types,
                benevolent: false,
            },
        ))
    }

    fn sub_parse_union(&mut self, start: u32, first: Type) -> PResult<Type> {
        let mut types = vec![first];
        while self.try_consume(TokenKind::Union) {
            self.skip_ws_comments();
            types.push(self.parse_atomic()?);
            self.skip_ws_comments();
        }
        Ok(self.spanned(
            start,
            TypeKind::Union {
                types,
                benevolent: false,
            },
        ))
    }

    fn parse_intersection(&mut self, first: Type) -> PResult<Type> {
        let start = first.span.start;
        let mut types = vec![first];
        while self.try_consume(TokenKind::Intersection) {
            types.push(self.parse_atomic()?);
            let sp = self.save();
            self.skip_ws_comments();
            if !self.is_kind(TokenKind::Intersection) {
                self.restore(sp);
                break;
            }
        }
        Ok(self.spanned(start, TypeKind::Intersection(types)))
    }

    fn sub_parse_intersection(&mut self, start: u32, first: Type) -> PResult<Type> {
        let mut types = vec![first];
        while self.try_consume(TokenKind::Intersection) {
            self.skip_ws_comments();
            types.push(self.parse_atomic()?);
            self.skip_ws_comments();
        }
        Ok(self.spanned(start, TypeKind::Intersection(types)))
    }

    fn parse_conditional(&mut self, start: u32, subject: Type) -> PResult<Type> {
        // current is the `is` identifier
        self.consume(TokenKind::Identifier)?;
        let negated = if self.is_value("not") {
            self.consume(TokenKind::Identifier)?;
            true
        } else {
            false
        };
        let target = self.parse()?;
        self.skip_ws_comments();
        self.consume(TokenKind::Nullable)?; // the `?`
        self.skip_ws_comments();
        let if_type = self.parse()?;
        self.skip_ws_comments();
        self.consume(TokenKind::Colon)?;
        self.skip_ws_comments();
        let else_type = self.sub_parse()?;
        Ok(self.spanned(
            start,
            TypeKind::Conditional(Conditional {
                subject: ConditionalSubject::Type(Box::new(subject)),
                target: Box::new(target),
                if_type: Box::new(if_type),
                else_type: Box::new(else_type),
                negated,
            }),
        ))
    }

    fn parse_conditional_for_parameter(&mut self, start: u32, name: String) -> PResult<Type> {
        self.consume(TokenKind::Variable)?;
        if !self.try_consume_value("is") {
            return Err(self.error("expected 'is'"));
        }
        let negated = if self.is_value("not") {
            self.consume(TokenKind::Identifier)?;
            true
        } else {
            false
        };
        let target = self.parse()?;
        self.skip_ws_comments();
        self.consume(TokenKind::Nullable)?;
        self.skip_ws_comments();
        let if_type = self.parse()?;
        self.skip_ws_comments();
        self.consume(TokenKind::Colon)?;
        self.skip_ws_comments();
        let else_type = self.sub_parse()?;
        Ok(self.spanned(
            start,
            TypeKind::Conditional(Conditional {
                subject: ConditionalSubject::Parameter(name),
                target: Box::new(target),
                if_type: Box::new(if_type),
                else_type: Box::new(else_type),
                negated,
            }),
        ))
    }

    /// `TypeParser::parseAtomic`.
    fn parse_atomic(&mut self) -> PResult<Type> {
        let start = self.cur_offset();

        if self.try_consume(TokenKind::OpenParen) {
            self.skip_ws_comments();
            let inner = self.sub_parse()?;
            self.skip_ws_comments();
            self.consume(TokenKind::CloseParen)?;
            let ty = if self.is_kind(TokenKind::OpenSquare) {
                self.try_parse_array_or_offset(inner)?
            } else {
                inner
            };
            // Re-span to include the parentheses/suffix.
            return Ok(Type::new(Span::new(start, self.end_offset()), ty.kind));
        }

        if self.try_consume(TokenKind::ThisVariable) {
            let mut ty = self.spanned(start, TypeKind::This);
            if self.is_kind(TokenKind::OpenSquare) {
                ty = self.try_parse_array_or_offset(ty)?;
            }
            return Ok(ty);
        }

        // Identifier (which may open a callable, generic, shape, offset access,
        // or — on `::` — a const fetch handled by the const-expr path).
        if self.is_kind(TokenKind::Identifier) {
            let name = self.cur_value().to_owned();
            let sp = self.save();
            self.next();
            if !self.is_kind(TokenKind::DoubleColon) {
                let ident = self.spanned(start, TypeKind::Identifier(name.clone()));
                let ty = self.parse_after_identifier(start, name, ident)?;
                return Ok(ty);
            } else {
                // `Foo::…` — a const fetch; rewind and let the const path handle it.
                self.restore(sp);
            }
        }

        // Const-expression path (literals and const fetches).
        self.parse_const_atomic(start)
    }

    /// The identifier continuation of `parseAtomic`: generic / callable / shape /
    /// offset access, or a bare identifier.
    fn parse_after_identifier(
        &mut self,
        start: u32,
        name: String,
        ident: Type,
    ) -> PResult<Type> {
        if self.is_kind(TokenKind::OpenAngle) {
            // Could be an HTML-tagged description, a callable with templates, or a
            // generic. Peek for HTML first.
            let sp = self.save();
            let is_html = self.is_html();
            self.restore(sp);
            if is_html {
                return Ok(ident);
            }
            // Try a templated callable; fall back to a generic.
            if let Some(callable) = self.try_parse_callable(start, name.clone(), true)? {
                return Ok(callable);
            }
            let mut ty = self.parse_generic(start, name)?;
            if self.is_kind(TokenKind::OpenSquare) {
                ty = self.try_parse_array_or_offset(ty)?;
            }
            return Ok(ty);
        }

        if self.is_kind(TokenKind::OpenParen) {
            if let Some(callable) = self.try_parse_callable(start, name.clone(), false)? {
                return Ok(callable);
            }
            return Ok(ident);
        }

        if self.is_kind(TokenKind::OpenSquare) {
            return self.try_parse_array_or_offset(ident);
        }

        if let Some(kind) = shape_kind(&name) {
            if self.is_kind(TokenKind::OpenCurly) && !self.is_preceded_by_hws() {
                let mut ty = self.parse_array_shape(start, kind)?;
                if self.is_kind(TokenKind::OpenSquare) {
                    ty = self.try_parse_array_or_offset(ty)?;
                }
                return Ok(ty);
            }
        } else if name == "object"
            && self.is_kind(TokenKind::OpenCurly)
            && !self.is_preceded_by_hws()
        {
            let mut ty = self.parse_object_shape(start)?;
            if self.is_kind(TokenKind::OpenSquare) {
                ty = self.try_parse_array_or_offset(ty)?;
            }
            return Ok(ty);
        }

        Ok(ident)
    }

    /// The literal / const-fetch path shared by `parseAtomic` (mirrors the
    /// `constExprParser->parse` tail).
    fn parse_const_atomic(&mut self, start: u32) -> PResult<Type> {
        let ce = self.parse_const_expr()?;
        let mut ty = self.spanned(start, TypeKind::Const(ce));
        if self.is_kind(TokenKind::OpenSquare) {
            ty = self.try_parse_array_or_offset(ty)?;
        }
        Ok(ty)
    }

    /// A restricted port of `ConstExprParser::parse` for the const types the
    /// grammar admits (no const arrays — those are a parse error in a type).
    fn parse_const_expr(&mut self) -> PResult<ConstExpr> {
        match self.cur_kind() {
            TokenKind::Float => {
                let v = strip_underscores(self.cur_value());
                self.next();
                Ok(ConstExpr::Float(v))
            }
            TokenKind::Integer => {
                let v = strip_underscores(self.cur_value());
                self.next();
                Ok(ConstExpr::Int(v))
            }
            TokenKind::SingleQuotedString => {
                let v = unescape_single(self.cur_value());
                self.next();
                Ok(ConstExpr::Str(StringLit::Single(v)))
            }
            TokenKind::DoubleQuotedString => {
                let v = unescape_double(self.cur_value());
                self.next();
                Ok(ConstExpr::Str(StringLit::Double(v)))
            }
            TokenKind::Identifier => {
                let ident = self.cur_value().to_owned();
                self.next();
                match ident.to_ascii_lowercase().as_str() {
                    "true" => return Ok(ConstExpr::True),
                    "false" => return Ok(ConstExpr::False),
                    "null" => return Ok(ConstExpr::Null),
                    "array" => {
                        // `array(...)` is a const array — rejected in type position.
                        return Err(self.error("unexpected const array in type"));
                    }
                    _ => {}
                }
                if self.try_consume(TokenKind::DoubleColon) {
                    let name = self.parse_const_fetch_name()?;
                    Ok(ConstExpr::Fetch {
                        class: ident,
                        name,
                    })
                } else {
                    Ok(ConstExpr::Fetch {
                        class: String::new(),
                        name: ident,
                    })
                }
            }
            _ => Err(self.error("expected a type")),
        }
    }

    /// Parse the constant name after `Class::`, supporting `IDENT`, wildcard `*`,
    /// and mixed sequences like `FOO_*BAR`, `*FOO*`, `A*B*C` (mirrors the const-
    /// fetch loop in ConstExprParser). Whitespace after a `*` ends the name.
    fn parse_const_fetch_name(&mut self) -> PResult<String> {
        let mut name = String::new();
        let mut last_ident = false;
        let mut last_wildcard = false;
        loop {
            if !last_ident && self.is_kind(TokenKind::Identifier) {
                name.push_str(self.cur_value());
                self.consume(TokenKind::Identifier)?;
                last_ident = true;
                last_wildcard = false;
                continue;
            }
            if !last_wildcard && self.is_kind(TokenKind::Wildcard) {
                name.push('*');
                self.next();
                last_wildcard = true;
                last_ident = false;
                if !self.skipped_hws().is_empty() {
                    break;
                }
                continue;
            }
            if !last_ident && !last_wildcard {
                // Nothing valid consumed — force the reference's error.
                self.consume(TokenKind::Wildcard)?;
            }
            break;
        }
        Ok(name)
    }

    /// `TypeParser::isHtml` — decide whether `<tag>…</tag>` after an identifier is
    /// a prose description (HTML) rather than a generic. Consumes tokens on a
    /// save-pointed copy; the caller rewinds.
    fn is_html(&mut self) -> bool {
        if self.consume(TokenKind::OpenAngle).is_err() {
            return false;
        }
        if !self.is_kind(TokenKind::Identifier) {
            return false;
        }
        let tag = self.cur_value().to_owned();
        self.next();
        if !self.try_consume(TokenKind::CloseAngle) {
            return false;
        }
        let end_tag = format!("/{tag}>");
        while !self.is_kind(TokenKind::End) {
            let opened = self.try_consume(TokenKind::OpenAngle);
            if opened && self.cur_value().contains(&end_tag) {
                return true;
            }
            if self.cur_value().ends_with(&end_tag) {
                return true;
            }
            if !opened {
                self.next();
            }
        }
        false
    }

    /// Try to parse a callable starting at `name`; `None` (with the cursor
    /// rewound) if it is not a callable after all.
    fn try_parse_callable(
        &mut self,
        start: u32,
        name: String,
        has_template: bool,
    ) -> PResult<Option<Type>> {
        let sp = self.save();
        match self.parse_callable(start, name, has_template) {
            Ok(ty) => Ok(Some(ty)),
            Err(_) => {
                self.restore(sp);
                Ok(None)
            }
        }
    }

    fn parse_callable(&mut self, start: u32, name: String, has_template: bool) -> PResult<Type> {
        let templates = if has_template {
            self.parse_callable_templates()?
        } else {
            Vec::new()
        };
        self.consume(TokenKind::OpenParen)?;
        self.skip_ws_comments();
        let mut params = Vec::new();
        if !self.is_kind(TokenKind::CloseParen) {
            params.push(self.parse_callable_parameter()?);
            self.skip_ws_comments();
            while self.try_consume(TokenKind::Comma) {
                self.skip_ws_comments();
                if self.is_kind(TokenKind::CloseParen) {
                    break;
                }
                params.push(self.parse_callable_parameter()?);
                self.skip_ws_comments();
            }
        }
        self.consume(TokenKind::CloseParen)?;
        self.consume(TokenKind::Colon)?;
        let return_type = self.parse_callable_return_type()?;
        Ok(self.spanned(
            start,
            TypeKind::Callable(CallableType {
                identifier: name,
                templates,
                params,
                return_type: Box::new(return_type),
            }),
        ))
    }

    fn parse_callable_templates(&mut self) -> PResult<Vec<TemplateParam>> {
        self.consume(TokenKind::OpenAngle)?;
        let mut templates = Vec::new();
        let mut first = true;
        while first || self.try_consume(TokenKind::Comma) {
            self.skip_ws_comments();
            if !first && self.is_kind(TokenKind::CloseAngle) {
                break;
            }
            first = false;
            templates.push(self.parse_template_param()?);
            self.skip_ws_comments();
        }
        self.consume(TokenKind::CloseAngle)?;
        Ok(templates)
    }

    fn parse_template_param(&mut self) -> PResult<TemplateParam> {
        if !self.is_kind(TokenKind::Identifier) {
            return Err(self.error("expected template name"));
        }
        let name = self.cur_value().to_owned();
        self.consume(TokenKind::Identifier)?;
        let bound = if self.try_consume_value("of") || self.try_consume_value("as") {
            Some(self.parse()?)
        } else {
            None
        };
        let lower = if self.try_consume_value("super") {
            Some(self.parse()?)
        } else {
            None
        };
        let default = if self.try_consume_value("=") {
            Some(self.parse()?)
        } else {
            None
        };
        if name.is_empty() {
            return Err(self.error("empty template name"));
        }
        Ok(TemplateParam {
            name,
            bound,
            lower,
            default,
        })
    }

    fn parse_callable_parameter(&mut self) -> PResult<CallableParam> {
        let ty = self.parse()?;
        let is_reference = self.try_consume(TokenKind::Reference);
        let is_variadic = self.try_consume(TokenKind::Variadic);
        let name = if self.is_kind(TokenKind::Variable) {
            let n = self.cur_value().to_owned();
            self.consume(TokenKind::Variable)?;
            n
        } else {
            String::new()
        };
        let is_optional = self.try_consume(TokenKind::Equal);
        Ok(CallableParam {
            ty,
            is_reference,
            is_variadic,
            name,
            is_optional,
        })
    }

    /// `TypeParser::parseCallableReturnType`.
    fn parse_callable_return_type(&mut self) -> PResult<Type> {
        let start = self.cur_offset();
        if self.is_kind(TokenKind::Nullable) {
            return self.parse_nullable(start);
        }
        if self.try_consume(TokenKind::OpenParen) {
            let inner = self.sub_parse()?;
            self.consume(TokenKind::CloseParen)?;
            let ty = if self.is_kind(TokenKind::OpenSquare) {
                self.try_parse_array_or_offset(inner)?
            } else {
                inner
            };
            return Ok(Type::new(Span::new(start, self.end_offset()), ty.kind));
        }
        if self.try_consume(TokenKind::ThisVariable) {
            let mut ty = self.spanned(start, TypeKind::This);
            if self.is_kind(TokenKind::OpenSquare) {
                ty = self.try_parse_array_or_offset(ty)?;
            }
            return Ok(ty);
        }
        if self.is_kind(TokenKind::Identifier) {
            let name = self.cur_value().to_owned();
            let sp = self.save();
            self.next();
            if !self.is_kind(TokenKind::DoubleColon) {
                let ident = self.spanned(start, TypeKind::Identifier(name.clone()));
                if self.is_kind(TokenKind::OpenAngle) {
                    let mut ty = self.parse_generic(start, name)?;
                    if self.is_kind(TokenKind::OpenSquare) {
                        ty = self.try_parse_array_or_offset(ty)?;
                    }
                    return Ok(ty);
                } else if self.is_kind(TokenKind::OpenSquare) {
                    return self.try_parse_array_or_offset(ident);
                } else if let Some(kind) = shape_kind(&name) {
                    if self.is_kind(TokenKind::OpenCurly) && !self.is_preceded_by_hws() {
                        let mut ty = self.parse_array_shape(start, kind)?;
                        if self.is_kind(TokenKind::OpenSquare) {
                            ty = self.try_parse_array_or_offset(ty)?;
                        }
                        return Ok(ty);
                    }
                } else if name == "object"
                    && self.is_kind(TokenKind::OpenCurly)
                    && !self.is_preceded_by_hws()
                {
                    let mut ty = self.parse_object_shape(start)?;
                    if self.is_kind(TokenKind::OpenSquare) {
                        ty = self.try_parse_array_or_offset(ty)?;
                    }
                    return Ok(ty);
                }
                return Ok(ident);
            } else {
                self.restore(sp);
            }
        }
        self.parse_const_atomic(start)
    }

    /// `TypeParser::parseGeneric`. `base` is the already-consumed identifier and
    /// the cursor is on `<`.
    fn parse_generic(&mut self, start: u32, base: String) -> PResult<Type> {
        self.consume(TokenKind::OpenAngle)?;
        self.skip_ws_comments();
        let mut args = Vec::new();
        let mut first = true;
        while first || self.try_consume(TokenKind::Comma) {
            self.skip_ws_comments();
            if !first && self.is_kind(TokenKind::CloseAngle) {
                break; // trailing comma
            }
            first = false;
            args.push(self.parse_generic_argument()?);
            self.skip_ws_comments();
        }
        self.consume(TokenKind::CloseAngle)?;

        // `__benevolent<T1|T2>` is accepted syntactically but expanded to the
        // plain union it wraps (ADR-0030): syntactic sugar, no benevolent union.
        if base == "__benevolent" && args.len() == 1 {
            let arg = args.into_iter().next().unwrap();
            let expanded = match arg.ty.kind {
                TypeKind::Union { types, .. } => Type::new(
                    Span::new(start, self.end_offset()),
                    TypeKind::Union {
                        types,
                        benevolent: true,
                    },
                ),
                other => Type::new(Span::new(start, self.end_offset()), other),
            };
            return Ok(expanded);
        }

        Ok(self.spanned(start, TypeKind::Generic { base, args }))
    }

    /// `TypeParser::parseGenericTypeArgument`.
    fn parse_generic_argument(&mut self) -> PResult<GenericArg> {
        let start = self.cur_offset();
        if self.try_consume(TokenKind::Wildcard) {
            return Ok(GenericArg {
                variance: Variance::Bivariant,
                ty: Type::new(
                    Span::new(start, self.end_offset()),
                    TypeKind::Identifier("mixed".to_owned()),
                ),
            });
        }
        let variance = if self.try_consume_value("contravariant") {
            Variance::Contravariant
        } else if self.try_consume_value("covariant") {
            Variance::Covariant
        } else {
            Variance::Invariant
        };
        let ty = self.parse()?;
        Ok(GenericArg { variance, ty })
    }

    /// `TypeParser::tryParseArrayOrOffsetAccess`. Each `[…]` iteration is
    /// save-pointed; a failure rolls the cursor back to before that `[` and
    /// stops, leaving the last good `ty` (and its trailing input) in place.
    fn try_parse_array_or_offset(&mut self, mut ty: Type) -> PResult<Type> {
        let start = ty.span.start;
        while self.is_kind(TokenKind::OpenSquare) {
            let sp = self.save();
            let step = (|p: &mut Self| -> PResult<Type> {
                let can_be_offset = !p.is_preceded_by_hws();
                p.consume(TokenKind::OpenSquare)?;
                if can_be_offset && !p.is_kind(TokenKind::CloseSquare) {
                    let offset = p.parse()?;
                    p.consume(TokenKind::CloseSquare)?;
                    Ok(Type::new(
                        Span::new(start, p.end_offset()),
                        TypeKind::OffsetAccess {
                            base: Box::new(ty.clone()),
                            offset: Box::new(offset),
                        },
                    ))
                } else {
                    p.consume(TokenKind::CloseSquare)?;
                    Ok(Type::new(
                        Span::new(start, p.end_offset()),
                        TypeKind::Array(Box::new(ty.clone())),
                    ))
                }
            })(self);
            match step {
                Ok(t) => ty = t,
                Err(_) => {
                    self.restore(sp);
                    break;
                }
            }
        }
        Ok(ty)
    }

    /// `TypeParser::parseArrayShape`.
    fn parse_array_shape(&mut self, start: u32, kind: ArrayShapeKind) -> PResult<Type> {
        self.consume(TokenKind::OpenCurly)?;
        let mut items = Vec::new();
        let mut sealed = true;
        let mut unsealed = None;
        let mut done = false;

        loop {
            self.skip_ws_comments();
            if self.try_consume(TokenKind::CloseCurly) {
                return Ok(self.spanned(
                    start,
                    TypeKind::ArrayShape(ArrayShape {
                        kind,
                        items,
                        sealed: true,
                        unsealed: None,
                    }),
                ));
            }
            if self.try_consume(TokenKind::Variadic) {
                sealed = false;
                self.skip_ws_comments();
                if self.is_kind(TokenKind::OpenAngle) {
                    unsealed = Some(if kind == ArrayShapeKind::Array {
                        self.parse_array_shape_unsealed()?
                    } else {
                        self.parse_list_shape_unsealed()?
                    });
                    self.skip_ws_comments();
                }
                self.try_consume(TokenKind::Comma);
                break;
            }
            items.push(self.parse_array_shape_item()?);
            self.skip_ws_comments();
            if !self.try_consume(TokenKind::Comma) {
                done = true;
            }
            if self.cur_kind() == TokenKind::Comment {
                self.next();
            }
            if done {
                break;
            }
        }

        self.skip_ws_comments();
        self.consume(TokenKind::CloseCurly)?;
        Ok(self.spanned(
            start,
            TypeKind::ArrayShape(ArrayShape {
                kind,
                items,
                sealed,
                unsealed,
            }),
        ))
    }

    /// `TypeParser::parseArrayShapeItem`.
    fn parse_array_shape_item(&mut self) -> PResult<ShapeItem> {
        self.skip_ws_comments();
        let sp = self.save();
        // Try `key(?): value`.
        let attempt = (|p: &mut Self| -> PResult<ShapeItem> {
            let key = p.parse_array_shape_key()?;
            let optional = p.try_consume(TokenKind::Nullable);
            p.consume(TokenKind::Colon)?;
            let value = p.parse()?;
            Ok(ShapeItem {
                key: Some(key),
                optional,
                value,
            })
        })(self);
        match attempt {
            Ok(item) => Ok(item),
            Err(_) => {
                self.restore(sp);
                let value = self.parse()?;
                Ok(ShapeItem {
                    key: None,
                    optional: false,
                    value,
                })
            }
        }
    }

    /// `TypeParser::parseArrayShapeKey`.
    fn parse_array_shape_key(&mut self) -> PResult<ShapeKey> {
        match self.cur_kind() {
            TokenKind::Integer => {
                let v = strip_underscores(self.cur_value());
                self.next();
                Ok(ShapeKey::Int(v))
            }
            TokenKind::SingleQuotedString => {
                let v = unescape_single(self.cur_value());
                self.next();
                Ok(ShapeKey::Str(StringLit::Single(v)))
            }
            TokenKind::DoubleQuotedString => {
                let v = unescape_double(self.cur_value());
                self.next();
                Ok(ShapeKey::Str(StringLit::Double(v)))
            }
            _ => {
                if !self.is_kind(TokenKind::Identifier) {
                    return Err(self.error("expected array shape key"));
                }
                let ident = self.cur_value().to_owned();
                self.consume(TokenKind::Identifier)?;
                if self.try_consume(TokenKind::DoubleColon) {
                    if !self.is_kind(TokenKind::Identifier) {
                        return Err(self.error("expected constant name"));
                    }
                    let name = self.cur_value().to_owned();
                    self.consume(TokenKind::Identifier)?;
                    Ok(ShapeKey::ConstFetch {
                        class: ident,
                        name,
                    })
                } else {
                    Ok(ShapeKey::Ident(ident))
                }
            }
        }
    }

    /// `TypeParser::parseArrayShapeUnsealedType`.
    fn parse_array_shape_unsealed(&mut self) -> PResult<UnsealedType> {
        self.consume(TokenKind::OpenAngle)?;
        self.skip_ws_comments();
        let mut value = self.parse()?;
        self.skip_ws_comments();
        let mut key = None;
        if self.try_consume(TokenKind::Comma) {
            self.skip_ws_comments();
            key = Some(Box::new(value));
            value = self.parse()?;
            self.skip_ws_comments();
        }
        self.consume(TokenKind::CloseAngle)?;
        Ok(UnsealedType {
            value: Box::new(value),
            key,
        })
    }

    /// `TypeParser::parseListShapeUnsealedType`.
    fn parse_list_shape_unsealed(&mut self) -> PResult<UnsealedType> {
        self.consume(TokenKind::OpenAngle)?;
        self.skip_ws_comments();
        let value = self.parse()?;
        self.skip_ws_comments();
        self.consume(TokenKind::CloseAngle)?;
        Ok(UnsealedType {
            value: Box::new(value),
            key: None,
        })
    }

    /// `TypeParser::parseObjectShape`.
    fn parse_object_shape(&mut self, start: u32) -> PResult<Type> {
        self.consume(TokenKind::OpenCurly)?;
        let mut items = Vec::new();
        loop {
            self.skip_ws_comments();
            if self.try_consume(TokenKind::CloseCurly) {
                return Ok(self.spanned(start, TypeKind::ObjectShape(items)));
            }
            items.push(self.parse_object_shape_item()?);
            self.skip_ws_comments();
            if !self.try_consume(TokenKind::Comma) {
                break;
            }
        }
        self.skip_ws_comments();
        self.consume(TokenKind::CloseCurly)?;
        Ok(self.spanned(start, TypeKind::ObjectShape(items)))
    }

    /// `TypeParser::parseObjectShapeItem`.
    fn parse_object_shape_item(&mut self) -> PResult<ShapeItem> {
        self.skip_ws_comments();
        let key = self.parse_object_shape_key()?;
        let optional = self.try_consume(TokenKind::Nullable);
        self.consume(TokenKind::Colon)?;
        let value = self.parse()?;
        Ok(ShapeItem {
            key: Some(key),
            optional,
            value,
        })
    }

    /// `TypeParser::parseObjectShapeKey`.
    fn parse_object_shape_key(&mut self) -> PResult<ShapeKey> {
        match self.cur_kind() {
            TokenKind::SingleQuotedString => {
                let v = unescape_single(self.cur_value());
                self.next();
                Ok(ShapeKey::Str(StringLit::Single(v)))
            }
            TokenKind::DoubleQuotedString => {
                let v = unescape_double(self.cur_value());
                self.next();
                Ok(ShapeKey::Str(StringLit::Double(v)))
            }
            _ => {
                if !self.is_kind(TokenKind::Identifier) {
                    return Err(self.error("expected object shape key"));
                }
                let ident = self.cur_value().to_owned();
                self.consume(TokenKind::Identifier)?;
                Ok(ShapeKey::Ident(ident))
            }
        }
    }
}

/// The shape kinds keyed by identifier name.
fn shape_kind(name: &str) -> Option<ArrayShapeKind> {
    match name {
        "array" => Some(ArrayShapeKind::Array),
        "list" => Some(ArrayShapeKind::List),
        "non-empty-array" => Some(ArrayShapeKind::NonEmptyArray),
        "non-empty-list" => Some(ArrayShapeKind::NonEmptyList),
        _ => None,
    }
}

fn strip_underscores(s: &str) -> String {
    s.replace('_', "")
}

/// `StringUnescaper::unescapeString` for single-quoted strings: only `\\` and `\'`.
fn unescape_single(raw: &str) -> String {
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1] == b'\\' || bytes[i + 1] == b'\'') {
            out.push(bytes[i + 1] as char);
            i += 2;
        } else {
            // Copy one UTF-8 char.
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&inner[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

/// `StringUnescaper::unescapeString` for double-quoted strings: `\"` plus the
/// C-style escape sequences (`\n \r \t \f \v \e`, `\xHH`, octal, `\u{…}`).
fn unescape_double(raw: &str) -> String {
    let inner = &raw[1..raw.len() - 1];
    let bytes = inner.as_bytes();
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            match n {
                b'\\' => {
                    out.push('\\');
                    i += 2;
                }
                b'"' => {
                    out.push('"');
                    i += 2;
                }
                b'n' => {
                    out.push('\n');
                    i += 2;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                }
                b'f' => {
                    out.push('\u{0c}');
                    i += 2;
                }
                b'v' => {
                    out.push('\u{0b}');
                    i += 2;
                }
                b'e' => {
                    out.push('\u{1b}');
                    i += 2;
                }
                b'x' | b'X' => {
                    let mut j = i + 2;
                    let mut hex = String::new();
                    while j < bytes.len() && hex.len() < 2 && bytes[j].is_ascii_hexdigit() {
                        hex.push(bytes[j] as char);
                        j += 1;
                    }
                    if hex.is_empty() {
                        out.push('\\');
                        i += 1;
                    } else {
                        if let Ok(code) = u8::from_str_radix(&hex, 16) {
                            out.push(code as char);
                        }
                        i = j;
                    }
                }
                b'u' if i + 2 < bytes.len() && bytes[i + 2] == b'{' => {
                    let mut j = i + 3;
                    let mut hex = String::new();
                    while j < bytes.len() && bytes[j] != b'}' && bytes[j].is_ascii_hexdigit() {
                        hex.push(bytes[j] as char);
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b'}' && !hex.is_empty() {
                        if let Ok(code) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(code)
                        {
                            out.push(ch);
                        }
                        i = j + 1;
                    } else {
                        out.push('\\');
                        i += 1;
                    }
                }
                b'0'..=b'7' => {
                    let mut j = i + 1;
                    let mut oct = String::new();
                    while j < bytes.len() && oct.len() < 3 && (b'0'..=b'7').contains(&bytes[j]) {
                        oct.push(bytes[j] as char);
                        j += 1;
                    }
                    if let Ok(code) = u8::from_str_radix(&oct, 8) {
                        out.push(code as char);
                    }
                    i = j;
                }
                _ => {
                    out.push('\\');
                    i += 1;
                }
            }
        } else {
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&inner[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}
