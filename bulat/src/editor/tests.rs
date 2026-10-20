use super::*;

#[test]
fn numeric_float() {
    assert_eq!(
        Token::default().tokens(&Syntax::default(), "3.14"),
        [Token::new(TokenType::Numeric(true), "3.14")]
    );
}

#[test]
fn numeric_float_desription() {
    // In Rust, the suffix is part of the literal.
    assert_eq!(
        Token::default().tokens(&Syntax::default(), "3.14_f32"),
        [Token::new(TokenType::Numeric(true), "3.14_f32")]
    );
}

#[test]
fn numeric_separators_exponents_suffixes_and_bases() {
    let numbers = |text: &str| {
        Token::default()
            .tokens(&Syntax::rust(), text)
            .into_iter()
            .filter(|t| !matches!(t.ty(), TokenType::Whitespace(_)))
            .map(|t| (t.ty(), t.buffer().to_owned()))
            .collect::<Vec<_>>()
    };
    let n = |float: bool, s: &str| (TokenType::Numeric(float), s.to_owned());
    assert_eq!(
        numbers("1_000u32 7usize 2.5e-3 1e10 0xff_u8 0o777 0b1010_1010 6f64"),
        [
            n(false, "1_000u32"),
            n(false, "7usize"),
            n(true, "2.5e-3"),
            n(true, "1e10"),
            n(false, "0xff_u8"),
            n(false, "0o777"),
            n(false, "0b1010_1010"),
            n(true, "6f64"),
        ]
    );
    // Ranges and method calls on integers aren't fractions.
    assert_eq!(
        numbers("0..10"),
        [
            n(false, "0"),
            (TokenType::Punctuation('.'), ".".to_owned()),
            (TokenType::Punctuation('.'), ".".to_owned()),
            n(false, "10"),
        ]
    );
    assert_eq!(
        numbers("2.max(3)")[..3],
        [
            n(false, "2"),
            (TokenType::Punctuation('.'), ".".to_owned()),
            (TokenType::Function, "max".to_owned()),
        ]
    );
}

#[test]
fn numeric_float_second_dot() {
    assert_eq!(
        Token::default().tokens(&Syntax::default(), "3.14.032"),
        [
            Token::new(TokenType::Numeric(true), "3.14"),
            Token::new(TokenType::Punctuation('.'), "."),
            Token::new(TokenType::Numeric(false), "032")
        ]
    );
}

#[test]
fn simple_rust() {
    let syntax = Syntax::rust();
    let input = vec![
        Token::new(TokenType::Keyword, "fn"),
        Token::new(TokenType::Whitespace(' '), " "),
        Token::new(TokenType::Function, "function"),
        Token::new(TokenType::Punctuation('('), "("),
        Token::new(TokenType::Str('\"'), "\"String\""),
        Token::new(TokenType::Punctuation(')'), ")"),
        Token::new(TokenType::Punctuation('{'), "{"),
        // A run of whitespace is one token, typed by its first character.
        Token::new(TokenType::Whitespace('\n'), "\n\t"),
        Token::new(TokenType::Keyword, "let"),
        Token::new(TokenType::Whitespace(' '), " "),
        Token::new(TokenType::Literal, "x_0"),
        Token::new(TokenType::Punctuation(':'), ":"),
        Token::new(TokenType::Whitespace(' '), " "),
        Token::new(TokenType::Type, "f32"),
        Token::new(TokenType::Whitespace(' '), " "),
        Token::new(TokenType::Punctuation('='), "="),
        Token::new(TokenType::Numeric(true), "13.34"),
        Token::new(TokenType::Punctuation(';'), ";"),
        Token::new(TokenType::Whitespace('\n'), "\n"),
        Token::new(TokenType::Punctuation('}'), "}"),
    ];
    let str = input.iter().map(|h| h.buffer()).collect::<String>();
    let output = Token::default().tokens(&syntax, &str);
    println!("{str}");
    assert_eq!(input, output);
}
