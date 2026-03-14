/// S-expression tokenizer + recursive-descent parser for the Reconcile DSL.
use super::ast::{CyclePolicy, Expr, Module, Policy, Rule};
use super::types::{ParseError, Status, Value};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    Symbol(String),
    StringLit(String),
}

fn tokenize(src: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0;
    let mut tokens = Vec::new();

    while pos < chars.len() {
        let ch = chars[pos];
        match ch {
            // Skip whitespace
            c if c.is_whitespace() => {
                pos += 1;
            }
            // Line comment
            ';' => {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
            }
            '(' => {
                tokens.push(Token::LParen);
                pos += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                pos += 1;
            }
            '"' => {
                pos += 1; // consume opening quote
                let mut s = String::new();
                while pos < chars.len() && chars[pos] != '"' {
                    if chars[pos] == '\\' && pos + 1 < chars.len() {
                        pos += 1; // skip backslash
                        s.push(chars[pos]);
                    } else {
                        s.push(chars[pos]);
                    }
                    pos += 1;
                }
                if pos >= chars.len() {
                    return Err(ParseError::UnexpectedEof);
                }
                pos += 1; // consume closing quote
                tokens.push(Token::StringLit(s));
            }
            _ => {
                // Symbol: read until whitespace, '(', ')', '"', or ';'
                let mut s = String::new();
                while pos < chars.len() {
                    let c = chars[pos];
                    if c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == ';' {
                        break;
                    }
                    s.push(c);
                    pos += 1;
                }
                tokens.push(Token::Symbol(s));
            }
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Parser state
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Result<Token, ParseError> {
        if self.pos >= self.tokens.len() {
            return Err(ParseError::UnexpectedEof);
        }
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        Ok(tok)
    }

    fn expect_lparen(&mut self) -> Result<(), ParseError> {
        match self.next()? {
            Token::LParen => Ok(()),
            got => Err(ParseError::UnexpectedToken {
                got: format!("{got:?}"),
                expected: "'('".to_string(),
            }),
        }
    }

    fn expect_rparen(&mut self) -> Result<(), ParseError> {
        match self.next()? {
            Token::RParen => Ok(()),
            got => Err(ParseError::UnexpectedToken {
                got: format!("{got:?}"),
                expected: "')'".to_string(),
            }),
        }
    }

    fn expect_symbol(&mut self, name: &str) -> Result<(), ParseError> {
        match self.next()? {
            Token::Symbol(s) if s == name => Ok(()),
            got => Err(ParseError::UnexpectedToken {
                got: format!("{got:?}"),
                expected: format!("'{name}'"),
            }),
        }
    }

    fn next_symbol(&mut self) -> Result<String, ParseError> {
        match self.next()? {
            Token::Symbol(s) => Ok(s),
            got => Err(ParseError::UnexpectedToken {
                got: format!("{got:?}"),
                expected: "symbol".to_string(),
            }),
        }
    }

    fn next_string_or_symbol(&mut self) -> Result<String, ParseError> {
        match self.next()? {
            Token::Symbol(s) => Ok(s),
            Token::StringLit(s) => Ok(s),
            got => Err(ParseError::UnexpectedToken {
                got: format!("{got:?}"),
                expected: "symbol or string literal".to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Parse module
// ---------------------------------------------------------------------------

/// Parse a DSL source string into a `Module`.
pub fn parse_module(src: &str) -> Result<Module, ParseError> {
    let tokens = tokenize(src)?;
    let mut p = Parser::new(tokens);

    p.expect_lparen()?;
    p.expect_symbol("module")?;

    let mut policy = Policy::default();
    let mut policy_explicit = false;
    let mut rules: Vec<Rule> = Vec::new();

    // Parse zero or more forms until ')'
    loop {
        match p.peek() {
            None => return Err(ParseError::UnexpectedEof),
            Some(Token::RParen) => {
                p.next()?; // consume ')'
                break;
            }
            Some(Token::LParen) => {
                // peek at head symbol
                // We need to look one more token ahead to decide if it's policy or define
                let next_pos = p.pos + 1;
                let head = p.tokens.get(next_pos);
                match head {
                    Some(Token::Symbol(s)) if s == "policy" => {
                        policy = parse_policy(&mut p)?;
                        policy_explicit = true;
                    }
                    Some(Token::Symbol(s)) if s == "define" => {
                        let rule = parse_define(&mut p)?;
                        if rules.iter().any(|r| r.name == rule.name) {
                            return Err(ParseError::DuplicateRule(rule.name));
                        }
                        rules.push(rule);
                    }
                    Some(Token::Symbol(s)) => {
                        return Err(ParseError::UnexpectedToken {
                            got: s.clone(),
                            expected: "'policy' or 'define'".to_string(),
                        });
                    }
                    _ => {
                        return Err(ParseError::UnexpectedToken {
                            got: format!("{:?}", p.tokens.get(next_pos)),
                            expected: "'policy' or 'define'".to_string(),
                        });
                    }
                }
            }
            Some(tok) => {
                return Err(ParseError::UnexpectedToken {
                    got: format!("{tok:?}"),
                    expected: "'(' or ')'".to_string(),
                });
            }
        }
    }

    Ok(Module {
        policy,
        policy_explicit,
        rules,
    })
}

fn parse_policy(p: &mut Parser) -> Result<Policy, ParseError> {
    p.expect_lparen()?;
    p.expect_symbol("policy")?;

    let mut policy = Policy::default();

    loop {
        match p.peek() {
            None => return Err(ParseError::UnexpectedEof),
            Some(Token::RParen) => {
                p.next()?;
                break;
            }
            Some(Token::LParen) => {
                p.expect_lparen()?;
                let key = p.next_symbol()?;
                let value = p.next_string_or_symbol()?;
                p.expect_rparen()?;

                match key.as_str() {
                    "cycle" => match value.as_str() {
                        "error" => policy.cycle = CyclePolicy::Error,
                        "unknown" => policy.cycle = CyclePolicy::Unknown,
                        v => {
                            return Err(ParseError::InvalidPolicyValue {
                                key: "cycle".to_string(),
                                value: v.to_string(),
                            })
                        }
                    },
                    "unknown-status" => match value.as_str() {
                        "none" => policy.unknown_status = Status::None,
                        "todo" => policy.unknown_status = Status::Todo,
                        "wip" => policy.unknown_status = Status::Wip,
                        "done" => policy.unknown_status = Status::Done,
                        v => {
                            return Err(ParseError::InvalidPolicyValue {
                                key: "unknown-status".to_string(),
                                value: v.to_string(),
                            })
                        }
                    },
                    k => return Err(ParseError::InvalidPolicyKey(k.to_string())),
                }
            }
            Some(tok) => {
                return Err(ParseError::UnexpectedToken {
                    got: format!("{tok:?}"),
                    expected: "'(' or ')'".to_string(),
                });
            }
        }
    }

    Ok(policy)
}

fn parse_define(p: &mut Parser) -> Result<Rule, ParseError> {
    p.expect_lparen()?;
    p.expect_symbol("define")?;

    // Expect (name param...)
    p.expect_lparen()?;
    let name = p.next_symbol()?;
    let mut params = Vec::new();
    loop {
        match p.peek() {
            None => return Err(ParseError::UnexpectedEof),
            Some(Token::RParen) => {
                p.next()?;
                break;
            }
            Some(Token::Symbol(_)) => {
                params.push(p.next_symbol()?);
            }
            Some(tok) => {
                return Err(ParseError::UnexpectedToken {
                    got: format!("{tok:?}"),
                    expected: "symbol or ')'".to_string(),
                });
            }
        }
    }

    let body = parse_expr(p)?;
    p.expect_rparen()?;

    Ok(Rule { name, params, body })
}

fn parse_expr(p: &mut Parser) -> Result<Expr, ParseError> {
    match p.peek().ok_or(ParseError::UnexpectedEof)? {
        Token::RParen => Err(ParseError::UnexpectedToken {
            got: "')'".to_string(),
            expected: "expression".to_string(),
        }),
        Token::Symbol(_) => {
            let s = p.next_symbol()?;
            // bool literals
            if s == "true" {
                return Ok(Expr::Lit(Value::Bool(true)));
            }
            if s == "false" {
                return Ok(Expr::Lit(Value::Bool(false)));
            }
            // status literals
            match s.as_str() {
                "none" => return Ok(Expr::Lit(Value::Status(Status::None))),
                "todo" => return Ok(Expr::Lit(Value::Status(Status::Todo))),
                "wip" => return Ok(Expr::Lit(Value::Status(Status::Wip))),
                "done" => return Ok(Expr::Lit(Value::Status(Status::Done))),
                _ => {}
            }
            Ok(Expr::Var(s))
        }
        Token::StringLit(_) => {
            let s = match p.next()? {
                Token::StringLit(s) => s,
                _ => unreachable!(),
            };
            Ok(Expr::Lit(Value::String(Rc::from(s))))
        }
        Token::LParen => {
            p.expect_lparen()?;
            // Look at head
            match p.peek().ok_or(ParseError::UnexpectedEof)? {
                Token::RParen => {
                    // empty list — shouldn't appear in well-formed DSL, treat as error
                    return Err(ParseError::InvalidExprHead("(empty form)".to_string()));
                }
                Token::Symbol(_) => {}
                Token::LParen => {
                    return Err(ParseError::InvalidExprHead("nested-paren head".to_string()));
                }
                Token::StringLit(s) => {
                    return Err(ParseError::InvalidExprHead(format!("\"{s}\"")));
                }
            }

            let head = p.next_symbol()?;

            let expr = match head.as_str() {
                "if" => {
                    let cond = parse_expr(p)?;
                    let then = parse_expr(p)?;
                    let else_ = parse_expr(p)?;
                    p.expect_rparen()?;
                    Expr::If {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        else_: Box::new(else_),
                    }
                }
                name => {
                    // All other heads → Expr::Call
                    let mut args = Vec::new();
                    loop {
                        match p.peek() {
                            None => return Err(ParseError::UnexpectedEof),
                            Some(Token::RParen) => {
                                p.next()?;
                                break;
                            }
                            _ => args.push(parse_expr(p)?),
                        }
                    }
                    Expr::Call {
                        name: name.to_string(),
                        args,
                    }
                }
            };
            Ok(expr)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::default_module::DEFAULT_MODULE;

    #[test]
    fn valid_default_module() {
        let module = parse_module(DEFAULT_MODULE).expect("default module must parse");
        assert_eq!(module.rules.len(), 6, "expected 6 rules in default module");
        assert!(module.rules.iter().any(|r| r.name == "child_status"));
        assert!(module.rules.iter().any(|r| r.name == "local_status"));
        assert!(module.rules.iter().any(|r| r.name == "targets_allow?"));
        assert!(module.rules.iter().any(|r| r.name == "effective_checked"));
        assert!(module.rules.iter().any(|r| r.name == "target_status"));
        assert!(module.rules.iter().any(|r| r.name == "effective_meta"));
    }

    #[test]
    fn policy_fields() {
        let src = r#"
        (module
            (policy
            (cycle unknown)
            (unknown-status wip)))
        "#;
        let module = parse_module(src).expect("should parse");
        assert_eq!(module.policy.cycle, CyclePolicy::Unknown);
        assert_eq!(module.policy.unknown_status, Status::Wip);
    }

    #[test]
    fn define_params() {
        let src = r#"
        (module
          (define (my_rule a b) (observe_checked a)))
        "#;
        let module = parse_module(src).expect("should parse");
        assert_eq!(module.rules.len(), 1);
        assert_eq!(module.rules[0].name, "my_rule");
        assert_eq!(module.rules[0].params, vec!["a", "b"]);
    }

    #[test]
    fn duplicate_rule_error() {
        let src = r#"
        (module
          (define (foo x) (observe_checked x))
          (define (foo y) (observe_checked y)))
        "#;
        let err = parse_module(src).expect_err("should fail on duplicate");
        assert_eq!(err, ParseError::DuplicateRule("foo".to_string()));
    }

    #[test]
    fn invalid_expr_head() {
        let src = r#"
        (module
          (define (bad x) ("not-a-sym" x)))
        "#;
        let err = parse_module(src).expect_err("string head is invalid");
        assert!(matches!(err, ParseError::InvalidExprHead(_)));
    }

    #[test]
    fn empty_module() {
        let src = "(module)";
        let module = parse_module(src).expect("empty module should parse");
        assert_eq!(module.rules.len(), 0);
        // Default policy
        assert_eq!(module.policy.cycle, CyclePolicy::Error);
        assert_eq!(module.policy.unknown_status, Status::Todo);
    }

    #[test]
    fn string_literal_in_observe_meta() {
        let src = r#"
        (module
          (define (eff n)
            (observe_meta n "checklist-status")))
        "#;
        let module = parse_module(src).expect("should parse");
        assert_eq!(module.rules.len(), 1);
    }

    #[test]
    fn define_two_param_rule() {
        let src = r#"
        (module
          (define (effective_meta n field)
            (observe_meta n field)))
        "#;
        let module = parse_module(src).expect("should parse");
        assert_eq!(module.rules[0].params, vec!["n", "field"]);
    }

    #[test]
    fn children_expr_parses() {
        let src = r#"
        (module
          (define (direct_children c)
            (children c)))
        "#;
        let module = parse_module(src).expect("should parse");
        match &module.rules[0].body {
            Expr::Call { name, args } => {
                assert_eq!(name, "children");
                match &args[0] {
                    Expr::Var(name) => assert_eq!(name, "c"),
                    other => panic!("expected var, got {other:?}"),
                }
            }
            other => panic!("expected children expr, got {other:?}"),
        }
    }
}
