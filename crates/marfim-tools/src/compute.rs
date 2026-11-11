//! Compute: safe recursive-descent arithmetic evaluator (no eval, no
//! shell). Supports + - * / % ^ parentheses, unary minus, floats.

pub fn eval(expr: &str) -> Result<f64, String> {
    let tokens = tokenize(expr)?;
    let mut pos = 0usize;
    let v = parse_add(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(format!("unexpected token at {pos}: {:?}", tokens[pos]));
    }
    if v.is_nan() || v.is_infinite() {
        return Err("result is not a finite number".into());
    }
    Ok(v)
}

#[derive(Debug, PartialEq, Clone)]
enum Tok {
    Num(f64),
    Op(char),
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {}
            '0'..='9' | '.' => {
                let mut num = String::new();
                num.push(c);
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_digit() || n == '.' {
                        num.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(Tok::Num(
                    num.parse::<f64>()
                        .map_err(|_| format!("bad number: {num}"))?,
                ));
            }
            '+' | '-' | '*' | '/' | '%' | '^' => out.push(Tok::Op(c)),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            _ => return Err(format!("unexpected char: {c}")),
        }
    }
    Ok(out)
}

fn parse_add(t: &[Tok], pos: &mut usize) -> Result<f64, String> {
    let mut left = parse_mul(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            Tok::Op('+') => {
                *pos += 1;
                left += parse_mul(t, pos)?;
            }
            Tok::Op('-') => {
                *pos += 1;
                left -= parse_mul(t, pos)?;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_mul(t: &[Tok], pos: &mut usize) -> Result<f64, String> {
    let mut left = parse_pow(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            Tok::Op('*') => {
                *pos += 1;
                left *= parse_pow(t, pos)?;
            }
            Tok::Op('/') => {
                *pos += 1;
                let r = parse_pow(t, pos)?;
                if r == 0.0 {
                    return Err("division by zero".into());
                }
                left /= r;
            }
            Tok::Op('%') => {
                *pos += 1;
                let r = parse_pow(t, pos)?;
                if r == 0.0 {
                    return Err("modulo by zero".into());
                }
                left %= r;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_pow(t: &[Tok], pos: &mut usize) -> Result<f64, String> {
    let base = parse_unary(t, pos)?;
    if *pos < t.len() && t[*pos] == Tok::Op('^') {
        *pos += 1;
        let exp = parse_pow(t, pos)?; // right-assoc
        return Ok(base.powf(exp));
    }
    Ok(base)
}

fn parse_unary(t: &[Tok], pos: &mut usize) -> Result<f64, String> {
    if *pos < t.len() {
        match &t[*pos] {
            Tok::Op('-') => {
                *pos += 1;
                return Ok(-parse_unary(t, pos)?);
            }
            Tok::Op('+') => {
                *pos += 1;
                return parse_unary(t, pos);
            }
            _ => {}
        }
    }
    parse_atom(t, pos)
}

fn parse_atom(t: &[Tok], pos: &mut usize) -> Result<f64, String> {
    match t.get(*pos) {
        Some(Tok::Num(n)) => {
            let v = *n;
            *pos += 1;
            Ok(v)
        }
        Some(Tok::LParen) => {
            *pos += 1;
            let v = parse_add(t, pos)?;
            match t.get(*pos) {
                Some(Tok::RParen) => {
                    *pos += 1;
                    Ok(v)
                }
                other => Err(format!("expected ), got {other:?}")),
            }
        }
        other => Err(format!("expected number or (, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn arithmetic() {
        assert!(close(eval("1 + 2 * 3").unwrap(), 7.0));
        assert!(close(eval("(1 + 2) * 3").unwrap(), 9.0));
        assert!(close(eval("2 ^ 3 ^ 2").unwrap(), 512.0));
        assert!(close(eval("-5 + 3").unwrap(), -2.0));
        assert!(close(eval("10 % 3").unwrap(), 1.0));
        assert!(close(eval("10 / 4").unwrap(), 2.5));
    }

    #[test]
    fn errors_are_clean() {
        assert!(eval("1 / 0").is_err());
        assert!(eval("1 % 0").is_err());
        assert!(eval("1 + abc").is_err());
        assert!(eval("(1 + 2").is_err());
        assert!(eval("__import__").is_err());
    }
}
