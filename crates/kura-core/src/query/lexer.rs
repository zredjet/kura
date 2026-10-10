//! 検索言語の字句解析。全角の記号は半角と同じに扱い、位置は入力の UTF-16 の文字位置で持つ
//! （パレットの入力欄で下線を引く位置と合わせるため）。

use super::parser::ParseError;

/// 字句の種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    LParen,
    RParen,
    Pipe,
    /// 語の先頭の `-`（除外）。
    Minus,
    /// 語（記号は正規化済み。NFC にはまだしていない＝位置を保つため）。
    Word(String),
    /// 引用符の中身（記号の正規化はしない）。
    Quoted(String),
}

/// 位置（UTF-16 の文字位置とバイト位置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

impl Span {
    /// 2 つの位置をまたぐ範囲。
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start,
            end: other.end,
            byte_start: self.byte_start,
            byte_end: other.byte_end,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// 全角の記号を半角にする（§2：`｜－＞＝：` ほか、かっこ・引用符・空白・読点）。
fn normalize(c: char) -> char {
    match c {
        '｜' => '|',
        '－' | '−' => '-',
        '＞' => '>',
        '＜' => '<',
        '＝' => '=',
        '：' => ':',
        '！' => '!',
        '（' => '(',
        '）' => ')',
        '“' | '”' | '＂' => '"',
        '，' | '、' => ',',
        '\u{3000}' => ' ',
        other => other,
    }
}

/// 位置付きの文字（正規化済み）。
struct Chars {
    chars: Vec<(char, usize, usize)>,
    end16: usize,
    end8: usize,
}

impl Chars {
    fn new(input: &str) -> Self {
        let mut chars = Vec::new();
        let mut pos16 = 0;
        for (byte, c) in input.char_indices() {
            chars.push((normalize(c), pos16, byte));
            pos16 += c.len_utf16();
        }
        Self {
            chars,
            end16: pos16,
            end8: input.len(),
        }
    }

    fn get(&self, i: usize) -> Option<char> {
        self.chars.get(i).map(|(c, _, _)| *c)
    }

    fn pos(&self, i: usize) -> (usize, usize) {
        self.chars
            .get(i)
            .map_or((self.end16, self.end8), |(_, p16, p8)| (*p16, *p8))
    }

    fn span(&self, from: usize, to: usize) -> Span {
        let (start, byte_start) = self.pos(from);
        let (end, byte_end) = self.pos(to);
        Span {
            start,
            end,
            byte_start,
            byte_end,
        }
    }
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '|' | '"')
}

/// 字句に分ける。
pub(crate) fn lex(input: &str) -> Result<Vec<Token>, ParseError> {
    let chars = Chars::new(input);
    let mut tokens = Vec::new();
    let mut i = 0;
    // 直前が語の区切り（先頭・空白・かっこ・|）か（`-` を除外として読むため）
    let mut at_boundary = true;
    while let Some(c) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
            at_boundary = true;
            continue;
        }
        match c {
            '(' | ')' | '|' => {
                let tok = match c {
                    '(' => Tok::LParen,
                    ')' => Tok::RParen,
                    _ => Tok::Pipe,
                };
                tokens.push(Token {
                    tok,
                    span: chars.span(i, i + 1),
                });
                i += 1;
                at_boundary = true;
            }
            '"' => {
                let start = i;
                let mut j = i + 1;
                while chars.get(j).is_some_and(|c| c != '"') {
                    j += 1;
                }
                if chars.get(j).is_none() {
                    let span = chars.span(start, j);
                    return Err(ParseError {
                        start: span.start,
                        end: span.end,
                        message: "引用符が閉じていません".into(),
                    });
                }
                let inner = chars.span(start + 1, j);
                let text = input[inner.byte_start..inner.byte_end].to_string();
                tokens.push(Token {
                    tok: Tok::Quoted(text),
                    span: chars.span(start, j + 1),
                });
                i = j + 1;
                at_boundary = false;
            }
            '-' if at_boundary => {
                tokens.push(Token {
                    tok: Tok::Minus,
                    span: chars.span(i, i + 1),
                });
                i += 1;
                at_boundary = true;
            }
            _ => {
                let start = i;
                let mut word = String::new();
                while let Some(c) = chars.get(i) {
                    if is_separator(c) {
                        break;
                    }
                    word.push(c);
                    i += 1;
                }
                tokens.push(Token {
                    tok: Tok::Word(word),
                    span: chars.span(start, i),
                });
                at_boundary = false;
            }
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(input: &str) -> Vec<Tok> {
        lex(input).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn splits_words_symbols_and_quotes() {
        assert_eq!(
            toks("(海｜港) -夜 \"駅前 夜\" IMG-001"),
            vec![
                Tok::LParen,
                Tok::Word("海".into()),
                Tok::Pipe,
                Tok::Word("港".into()),
                Tok::RParen,
                Tok::Minus,
                Tok::Word("夜".into()),
                Tok::Quoted("駅前 夜".into()),
                Tok::Word("IMG-001".into()),
            ]
        );
        // 値の引用符は語のすぐ後ろに続く
        let tokens = lex("作者:\"もりの 森\"").unwrap();
        assert_eq!(tokens[0].span.end, tokens[1].span.start);
    }

    #[test]
    fn positions_are_utf16() {
        let tokens = lex("😀 夜").unwrap();
        assert_eq!((tokens[1].span.start, tokens[1].span.end), (3, 4));
        assert_eq!(tokens[1].span.byte_start, 5);
    }
}
