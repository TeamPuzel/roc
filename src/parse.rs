use expr::Operator;
use expr::{Expr, Pattern, Ident};

use std::char;
use parse_state::{IndentablePosition};
use smallvec::SmallVec;

use combine::parser::char::{char, string, spaces, digit, hex_digit, HexDigit, alpha_num};
use combine::parser::repeat::{many, count_min_max, sep_by1, skip_many, skip_many1};
use combine::parser::item::{any, satisfy_map, value, position, satisfy};
use combine::parser::combinator::{look_ahead, not_followed_by};
use combine::{attempt, choice, eof, many1, parser, Parser, optional, between, unexpected_any, unexpected};
use combine::error::{Consumed, ParseError};
use combine::stream::{Stream, Positioned};
use combine::stream::state::{State};

pub const ERR_EMPTY_CHAR: &'static str = "EMPTY_CHAR";

pub fn parse_string(string: &str) -> Result<Expr, combine::easy::Errors<char, &str, IndentablePosition>> {
    let parse_state = State::with_positioner(string, IndentablePosition::default());

    expr().skip(eof()).easy_parse(parse_state).map(|( expr, _ )| expr)
}

pub fn expr<I>() -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    spaces().with(expr_body(1)).skip(whitespace_or_eof())
}

fn indentation<I>() -> impl Parser<Input = I, Output = i32>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> ,
    I: Positioned
{
    position().map(|pos: IndentablePosition| (pos.indent_col))
}

fn whitespace_or_eof<I>() -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    choice((
        spaces1(),
        eof().with(value(()))
    ))
}

fn whitespace<I>() -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    many::<Vec<_>, _>(choice((char(' '), char('\n')))).with(value(()))
}

fn whitespace1<I>() -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    skip_many1(choice((char(' '), char('\n'))))
}


fn spaces1<I>() -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    skip_many1(choice((char(' '), char('\n'))))
}

fn indented_whitespaces<I>(min_indent: i32) -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    skip_many(indented_whitespace(min_indent))
}

fn indented_whitespaces1<I>(min_indent: i32) -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
    skip_many1(indented_whitespace(min_indent))
}

fn indented_whitespace<I>(min_indent: i32) -> impl Parser<Input = I, Output = ()>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position> {
        choice((
            char(' ').with(value(())),
            // If we hit a newline, it must be followed by:
            //
            // - Any number of blank lines (which may contain only spaces)
            // - At least min_indent spaces, or else eof()
            char('\n')
                .skip(
                    skip_many(
                        char('\n').skip(skip_many(char(' ')))
                    )
                )
                .skip(
                    choice((
                        many::<Vec<_>, _>(char(' ')).then(move |chars| {
                            if chars.len() < min_indent as usize {
                                unexpected("outdent").left()
                            } else {
                                value(()).right()
                            }
                        }),
                        eof().with(value(()))
                    ))
                )
                .with(value(()))
        ))
}

/// This is separate from expr_body for the sake of function application,
/// so it can stop parsing when it reaches an operator (since they have
/// higher precedence.)
fn expr_body_without_operators<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    expr_body_without_operators_(min_indent)
}

parser! {
    #[inline(always)]
    fn expr_body_without_operators_[I](min_indent_ref: i32)(I) -> Expr
        where [ I: Stream<Item = char, Position = IndentablePosition> ]
    {
        // TODO figure out why min_indent_ref has the type &mut i32
        let min_indent = *min_indent_ref;

        choice((
            closure(min_indent),
            parenthetical_expr(min_indent),
            string("{}").with(value(Expr::EmptyRecord)),
            string_literal(),
            number_literal(),
            char_literal(),
            if_expr(min_indent),
            match_expr(min_indent),
            let_expr(min_indent),
            apply_variant(min_indent),
            func_or_var(min_indent),
        ))
    }
}

fn expr_body<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    expr_body_(min_indent)
}

// This macro allows recursive parsers
parser! {
    #[inline(always)]
    fn expr_body_[I](min_indent_ref: i32)(I) -> Expr
        where [ I: Stream<Item = char, Position = IndentablePosition> ]
    {
        // TODO figure out why min_indent_ref has the type &mut i32
        let min_indent = *min_indent_ref;

        expr_body_without_operators(min_indent)
        .and(
            // Optionally follow the expression with an operator,
            //
            // e.g. In the expression (1 + 2), the subexpression 1 is
            // followed by the operator + and another subexpression, 2
            optional(
                attempt(
                    indented_whitespaces(min_indent)
                        .with(operator())
                        .skip(whitespace())
                        .skip(indented_whitespaces(min_indent))
                        .and(expr_body(min_indent))
                )
            )
        ).map(|(expr1, opt_op)| {
            match opt_op {
                None => expr1,
                Some((op, expr2)) => {
                    Expr::Operator(Box::new(expr1), op, Box::new(expr2))
                },
            }
        })
    }
}

pub fn if_expr<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    string("if").skip(indented_whitespaces1(min_indent))
        .with(expr_body(min_indent)).skip(indented_whitespaces1(min_indent))
        .skip(string("then")).skip(indented_whitespaces1(min_indent))
        .and(expr_body(min_indent)).skip(indented_whitespaces1(min_indent))
        .skip(string("else")).skip(indented_whitespaces1(min_indent))
        .and(expr_body(min_indent))
        .map(|((conditional, then_branch), else_branch)|
            Expr::If(
                Box::new(conditional),
                Box::new(then_branch),
                Box::new(else_branch)
            )
        )
}

pub fn match_expr<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    string("case").skip(indented_whitespaces1(min_indent))
        .with(expr_body(min_indent)).skip(indented_whitespaces1(min_indent))
        .and(
            many::<SmallVec<_>, _>(
                string("when").skip(indented_whitespaces1(min_indent))
                    .with(pattern(min_indent)).skip(indented_whitespaces1(min_indent))
                    .skip(string("then")).skip(indented_whitespaces1(min_indent))
                    .and(expr_body(min_indent).map(|expr| Box::new(expr)))
            )
        )
        .map(|(conditional, branches)|
            if branches.is_empty() {
                // TODO handle this more gracefully
                panic!("encountered match-expression with no branches!")
            } else {
                Expr::Case(Box::new(conditional), branches)
            }
        )
}

pub fn parenthetical_expr<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    between(char('('), char(')'),
        indented_whitespaces(min_indent).with(expr_body(min_indent)).skip(indented_whitespaces(min_indent))
    ).and(
        // Parenthetical expressions can optionally be followed by
        // whitespace and one or more comma-separated expressions,
        // meaning this is function application!
        optional(attempt(function_application(min_indent)))
    ).map(|(expr, opt_args): (Expr, Option<Vec<Expr>>)|
        match opt_args {
            None => expr,
            Some(args) => Expr::Apply(Box::new(expr), args)
        }
    )
}

pub fn function_application<I>(min_indent: i32) -> impl Parser<Input = I, Output = Vec<Expr>>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    indented_whitespaces1(min_indent)
        .with(
            sep_by1(
                attempt(
                    // Keywords like "then" and "else" are not function application!
                    not_followed_by(choice((string("then"), string("else"), string("when"))))
                        // Don't parse operators, because they have a higher
                        // precedence than function application. If we see one,
                        // we're done!
                        .with(expr_body_without_operators(min_indent))
                        .skip(indented_whitespaces(min_indent))
                ),
                char(',')
                    .skip(indented_whitespaces(min_indent))
            )
        )
}


pub fn operator<I>() -> impl Parser<Input = I, Output = Operator>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    choice((
        string("==").map(|_| Operator::Equals),
        char('+').map(|_| Operator::Plus),
        char('-').map(|_| Operator::Minus),
        char('*').map(|_| Operator::Star),
        char('/').map(|_| Operator::Slash),
    ))
}

pub fn let_expr<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    attempt(
        pattern(min_indent).and(indentation())
            .skip(whitespace())
            .and(
                char('=').with(indentation())
                    // If the "=" after the identifier turns out to be
                    // either "==" or "=>" then this is not a declaration!
                    .skip(not_followed_by(choice((char('='), char('>')))))
            )
        )
        .skip(whitespace())
        .then(move |((var_pattern, original_indent), equals_sign_indent)| {
            if original_indent < min_indent {
                unexpected_any("this declaration is outdented too far").left()
            } else if equals_sign_indent < original_indent /* `<` because '=' should be same indent or greater */ {
                unexpected_any("the = in this declaration seems outdented").left()
            } else {
                expr_body(original_indent + 1 /* declaration body must be indented relative to original decl */)
                    .skip(whitespace1())
                    .and(expr_body(original_indent).and(indentation()))
                .then(move |(var_expr, (in_expr, in_expr_indent))| {
                    if in_expr_indent != original_indent {
                        unexpected_any("the return expression was indented differently from the original declaration").left()
                    } else {
                        value(Expr::Let(var_pattern.to_owned(), Box::new(var_expr), Box::new(in_expr))).right()
                    }
                }).right()
            }
        })
}

pub fn func_or_var<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    ident().and(optional(attempt(function_application(min_indent))))
        .map(|(name, opt_args): (String, Option<Vec<Expr>>)|
            // Use optional(sep_by1()) over sep_by() to avoid
            // allocating a Vec in the common case where this is a var
            match opt_args {
                None => Expr::Var(name),
                Some(args) => Expr::Func(name, args)
            }
        )
}

pub fn closure<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    // TODO patterns must be separated by commas!
    attempt(
        between(char('('), char(')'),
            sep_by1(
                pattern(min_indent),
                char(',').skip(indented_whitespaces(min_indent))
            ))
        .skip(indented_whitespaces1(min_indent))
        .skip(string("->"))
        .skip(indented_whitespaces1(min_indent))
    )
    .and(expr_body(min_indent))
    .map(|(patterns, closure_body)| {
        Expr::Closure(patterns, Box::new(closure_body))
    })
}

parser! {
    #[inline(always)]
    fn pattern[I](min_indent_ref: i32)(I) -> Pattern
        where [ I: Stream<Item = char, Position = IndentablePosition> ]
    {
        let min_indent = *min_indent_ref;

        choice((
            char('_').map(|_| Pattern::Underscore),
            string("{}").map(|_| Pattern::EmptyRecord),
            ident().map(|name| Pattern::Identifier(name)),
            match_variant(min_indent)
        ))
    }
}

pub fn apply_variant<I>(min_indent: i32) -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    attempt(variant_name())
        .and(optional(attempt(function_application(min_indent))))
        .map(|(name, opt_args): (String, Option<Vec<Expr>>)|
            Expr::ApplyVariant(name, opt_args)
        )
}

pub fn match_variant<I>(min_indent: i32) -> impl Parser<Input = I, Output = Pattern>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    attempt(variant_name())
        .and(optional(attempt(
            sep_by1(
                pattern(min_indent),
                char(',').skip(indented_whitespaces(min_indent))
        ))))
        .map(|(name, opt_args): (String, Option<Vec<Pattern>>)|
            // Use optional(sep_by1()) over sep_by() to avoid
            // allocating a Vec in case the variant is empty
            Pattern::Variant(name, opt_args)
        )
}

pub fn variant_name<I>() -> impl Parser<Input = I, Output = String>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    // Variants must begin with an uppercase letter, but can have any
    // combination of letters or numbers afterwards.
    // No underscores, dashes, or apostrophes.
    look_ahead(satisfy(|ch: char| ch.is_uppercase()))
        .with(many1::<Vec<_>, _>(alpha_num()))
        .map(|chars| chars.into_iter().collect())
}

pub fn ident<I>() -> impl Parser<Input = I, Output = String>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    // Identifiers must begin with a lowercase letter, but can have any
    // combination of letters or numbers afterwards.
    // No underscores, dashes, or apostrophes.
    many1::<Vec<_>, _>(alpha_num())
        .then(|chars: Vec<char>| {
            let valid_start_char = chars[0].is_lowercase();

            if valid_start_char {
                let ident_str:String = chars.into_iter().collect();

                match ident_str.as_str() {
                    "if" => unexpected_any("Reserved keyword `if`").left(),
                    "then" => unexpected_any("Reserved keyword `then`").left(),
                    "else" => unexpected_any("Reserved keyword `else`").left(),
                    "case" => unexpected_any("Reserved keyword `case`").left(),
                    "when" => unexpected_any("Reserved keyword `when`").left(),
                    _ => value(ident_str).right()
                }
            } else {
                unexpected_any("First character in an identifier that was not a lowercase letter").left()
            }
        })
}

pub fn string_literal<I>() -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    between(char('"'), char('"'),
        many::<Vec<(Ident, String)>, _>(
            choice((
                // Handle the edge cases where the interpolation happens
                // to be at the very beginning of the string literal,
                // or immediately following the previous interpolation.
                attempt(string("\\("))
                    .with(value("".to_string()))
                    .and(ident().skip(char(')'))),

                // Parse a bunch of non-interpolated characters until we hit \(
                many1::<Vec<char>, _>(string_body())
                    .map(|chars: Vec<char>| chars.into_iter().collect::<String>())
                    .and(choice((
                        (between(value('('), value(')'), ident())),
                        // If we never encountered \( then we hit the end of
                        // the string literal. Use empty Ident here because
                        // we're going to pop this Ident off the array anyhow.
                        value("".to_string())
                    ))),
            ))
    )
    .map(|mut pairs| {
        match pairs.len() {
            0 => Expr::EmptyStr,
            1 => Expr::Str(pairs.pop().unwrap().0),
            _ => {
                // Discard the final Ident; we stuck an empty string in there anyway.
                let (trailing_str, _) = pairs.pop().unwrap();

                Expr::InterpolatedStr(pairs, trailing_str)
            }
        }
    }))
}

pub fn char_literal<I>() -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    between(char('\''), char('\''), char_body().expected(ERR_EMPTY_CHAR))
        .map(|ch| Expr::Char(ch))
}


fn unicode_code_pt<I>() -> impl Parser<Input = I, Output = char>
where
    I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>,
{
    // You can put up to 6 hex digits inside \u{...}
    // e.g. \u{00A0} or \u{101010}
    // They must be no more than 10FFFF
    let hex_code_pt =
        count_min_max::<Vec<char>, HexDigit<I>>(1, 6, hex_digit())
        .then(|hex_digits| {
            let hex_str:String = hex_digits.into_iter().collect();

            match u32::from_str_radix(&hex_str, 16) {
                Ok(code_pt) => {
                    if code_pt > 0x10FFFF {
                        unexpected_any("Invalid Unicode code point. It must be no more than \\u{10FFFF}.").right()
                    } else {
                        match char::from_u32(code_pt) {
                            Some(ch) => value(ch).left(),
                            None => unexpected_any("Invalid Unicode code point.").right()
                        }
                    }
                },
                Err(_) => {
                    unexpected_any("Invalid hex code - Unicode code points must be specified using hexadecimal characters (the numbers 0-9 and letters A-F)").right()
                }
            }
        });

    char('u').with(between(char('{'), char('}'), hex_code_pt))
}

fn string_body<I>() -> impl Parser<Input = I, Output = char>
where
    I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>,
{
    parser(|input: &mut I| {
        let (parsed_char, consumed) = try!(any().parse_lazy(input).into());
        let mut escaped = satisfy_map(|escaped_char| {
            // NOTE! When modifying this, revisit char_body too!
            // Their implementations are similar but not the same.
            match escaped_char {
                '"' => Some('"'),
                '\\' => Some('\\'),
                't' => Some('\t'),
                'n' => Some('\n'),
                'r' => Some('\r'),
                _ => None,
            }
        });

        match parsed_char {
            '\\' => {
                if look_ahead(char('(')).parse_stream(input).is_ok() {
                    // If we hit a \( then we're doing string interpolation.
                    // Bail out after consuming the backslash!
                    Err(Consumed::Empty(I::Error::empty(input.position()).into()))
                } else {
                    consumed.combine(|_| {
                        // Try to parse basic backslash-escaped literals
                        // e.g. \t, \n, \r
                        escaped.parse_stream(input).or_else(|_|
                            // If we didn't find any of those, try \u{...}
                            unicode_code_pt().parse_stream(input)
                        )
                    })
                }
            },
            '"' => {
                // Never consume a double quote unless it was preceded by a
                // backslash. This means we're at the end of the string literal!
                Err(Consumed::Empty(I::Error::empty(input.position()).into()))
            },
            _ => Ok((parsed_char, consumed))
        }
    })
}

fn char_body<I>() -> impl Parser<Input = I, Output = char>
where
    I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>,
{
    parser(|input: &mut I| {
        let (parsed_char, consumed) = try!(any().parse_lazy(input).into());
        let mut escaped = satisfy_map(|escaped_char| {
            // NOTE! When modifying this, revisit string_body too!
            // Their implementations are similar but not the same.
            match escaped_char {
                '\'' => Some('\''),
                '\\' => Some('\\'),
                't' => Some('\t'),
                'n' => Some('\n'),
                'r' => Some('\r'),
                _ => None,
            }
        });

        match parsed_char {
            '\\' => {
                consumed.combine(|_| {
                    // Try to parse basic backslash-escaped literals
                    // e.g. \t, \n, \r
                    escaped.parse_stream(input).or_else(|_|
                        // If we didn't find any of those, try \u{...}
                        unicode_code_pt().parse_stream(input)
                    )
                })
            },
            '\'' => {
                // We should never consume a single quote unless
                // it's preceded by a backslash
                Err(Consumed::Empty(I::Error::empty(input.position()).into()))
            },
            _ => Ok((parsed_char, consumed)),
        }
    })
}

pub fn number_literal<I>() -> impl Parser<Input = I, Output = Expr>
where I: Stream<Item = char, Position = IndentablePosition>,
    I::Error: ParseError<I::Item, I::Range, I::Position>
{
    // We expect these to be digits, but read any alphanumeric characters
    // because it could turn out they're malformed identifiers which
    // happen to begin with a number. We'll check for that at the end.
    let digits_after_decimal =  many1::<Vec<_>, _>(alpha_num());

    // Digits before the decimal point can be space-separated
    // e.g. one million can be written as 1 000 000
    let digits_before_decimal = many1::<Vec<_>, _>(
        alpha_num().skip(optional(
                attempt(
                    char(' ').skip(
                        // Don't mistake keywords like `then` and `else` for
                        // space-separated digits!
                        not_followed_by(choice((string("then"), string("else"), string("when"))))
                    )
                )
        ))
    );

    optional(char('-'))
        // Do this lookahead to decide if we should parse this as a number.
        // This matters because once we commit to parsing it as a number,
        // we may discover non-digit chars, indicating this is actually an
        // invalid identifier. (e.g. "523foo" looks like a number, but turns
        // out to be an invalid identifier on closer inspection.)
        .and(look_ahead(digit()))
        .and(digits_before_decimal)
        .and(optional(char('.').with(digits_after_decimal)))
        .then(|(((opt_minus, _), int_digits), decimals): (((Option<char>, _), Vec<char>), Option<Vec<char>>)| {
            let is_positive = opt_minus.is_none();

            // TODO check length of digits and make sure not to overflow
            let int_str: String = int_digits.into_iter().collect();

            match ( int_str.parse::<i64>(), decimals ) {
                (Ok(int_val), None) => {
                    if is_positive {
                        value(Expr::Int(int_val as i64)).right()
                    } else {
                        value(Expr::Int(-int_val as i64)).right()
                    }
                },
                (Ok(int_val), Some(nums)) => {
                    let decimal_str: String = nums.into_iter().collect();
                    // calculate numerator and denominator
                    // e.g. 123.45 == 12345 / 100
                    let denom = (10 as i64).pow(decimal_str.len() as u32);

                    match decimal_str.parse::<u32>() {
                        Ok(decimal) => {
                            let numerator = (int_val * denom) + (decimal as i64);

                            if is_positive {
                                value(Expr::Frac(numerator, denom as u64)).right()
                            } else {
                                value(Expr::Frac(-numerator, denom as u64)).right()
                            }
                        },
                        Err(_) => {
                            unexpected_any("non-digit characters after decimal point in a number literal").left()
                        }
                    }
                },
                (Err(_), _) =>
                    unexpected_any("looked like a number but was actually malformed identifier").left()
            }
        })
}

