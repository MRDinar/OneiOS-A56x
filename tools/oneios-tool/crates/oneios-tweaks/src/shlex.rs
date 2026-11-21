//! Python's `shlex.split(text)` (POSIX mode, no comments): ONEIOS_ROOTTWEAK_ARGS, as image-hook splits it.

pub fn split(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut have = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if have {
                    out.push(std::mem::take(&mut token));
                    have = false;
                }
            }
            '\\' => {
                let n = chars.next().ok_or("No escaped character")?;
                token.push(n);
                have = true;
            }
            '\'' => {
                have = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(n) => token.push(n),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                have = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n @ ('"' | '\\')) => token.push(n),
                            Some(n) => {
                                token.push('\\');
                                token.push(n);
                            }
                            None => return Err("No closing quotation".into()),
                        },
                        Some(n) => token.push(n),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            c => {
                token.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(token);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn as_python_splits() {
        assert_eq!(super::split("--keep com.apple.momentsd").unwrap(), ["--keep", "com.apple.momentsd"]);
        assert_eq!(super::split(" a 'b c' \"d \\\" \\e\" f\\ g '' ").unwrap(), ["a", "b c", "d \" \\e", "f g", ""]);
        assert!(super::split("'a").is_err());
        assert!(super::split("a\\").is_err());
        assert!(super::split("").unwrap().is_empty());
    }
}
