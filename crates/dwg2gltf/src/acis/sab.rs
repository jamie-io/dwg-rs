// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tokenizer for ACIS/ASM model data: binary SAB and text SAT.
//!
//! Both become a list of [`Record`]s, each a type name ("plane-surface",
//! "coedge", ...) and its fields. Pointers are `Field::Ptr(index)` into the
//! record list (-1 = null); subtype blocks (`{ ... }`) become nested lists.

use std::fmt;
use std::rc::Rc;

use crate::limits::MAX_ACIS_NESTING;

#[derive(Debug, Clone, PartialEq)]
pub struct AcisError(pub String);

impl fmt::Display for AcisError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AcisError {}

fn err<T>(msg: impl Into<String>) -> Result<T, AcisError> {
    Err(AcisError(msg.into()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Int(i64),
    Float(f64),
    /// A string, or (SAT) a bare word outside subtype blocks.
    Str(String),
    Bool(bool),
    Ptr(i64),
    Enum(i64),
    /// A name inside a record: subtype names, or (SAT) words inside `{ }`.
    Type(String),
    Vec3([f64; 3]),
    Vec2([f64; 2]),
    List(Rc<Vec<Field>>),
}

impl Field {
    /// A number (not a logical).
    pub fn num(&self) -> Option<f64> {
        match *self {
            Field::Int(i) => Some(i as f64),
            Field::Float(f) => Some(f),
            _ => None,
        }
    }

    pub fn is_num(&self) -> bool {
        self.num().is_some()
    }

    pub fn is_type(&self, name: &str) -> bool {
        matches!(self, Field::Type(t) if t == name)
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub index: usize,
    pub kind: String,
    pub fields: Vec<Field>,
}

#[derive(Debug)]
pub struct Model {
    pub version: i64,
    /// Model unit in millimeters (as declared; not reliable).
    pub units_mm: f64,
    pub records: Vec<Record>,
    /// SAT: vectors and flags are loose numbers and words.
    pub text: bool,
}

impl Model {
    pub fn get(&self, field: Option<&Field>) -> Option<&Record> {
        match field {
            Some(&Field::Ptr(i)) if i >= 0 => self.records.get(i as usize),
            _ => None,
        }
    }
}

/// SAB bytes, or SAT text.
#[derive(Debug, Clone)]
pub enum AcisData {
    Sab(Vec<u8>),
    Sat(String),
}

const END_MARKERS: [&str; 2] = ["End-of-ACIS-data", "End-of-ASM-data"];

pub fn parse(data: &AcisData) -> Result<Model, AcisError> {
    match data {
        AcisData::Sat(text) => parse_sat(text),
        AcisData::Sab(bytes)
            if bytes.starts_with(b"ACIS BinaryFile") || bytes.starts_with(b"ASM BinaryFile") =>
        {
            parse_sab(bytes)
        }
        AcisData::Sab(bytes) => parse_sat(&latin1(bytes)),
    }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Value(Field),
    Ident(String),
    SubIdent(String),
    Open,
    Close,
    End,
}

// -- binary -------------------------------------------------------------------

fn parse_sab(data: &[u8]) -> Result<Model, AcisError> {
    if !(data.starts_with(b"ASM BinaryFile4") || data.starts_with(b"ACIS BinaryFile")) {
        return err("not a SAB stream");
    }
    let mut pos = 15;
    // "ASM BinaryFile4" (AutoCAD 2018+) widens only the version field
    let vsize = if data.starts_with(b"ASM BinaryFile4") {
        8
    } else {
        4
    };
    let Some(vbytes) = data.get(pos..pos + vsize) else {
        return err("truncated SAB header");
    };
    let version = if vsize == 8 {
        i64::from_le_bytes(vbytes.try_into().unwrap())
    } else {
        i32::from_le_bytes(vbytes.try_into().unwrap()) as i64
    };
    pos += vsize;
    // then a few 32-bit counts (3 in ACIS, 2 in ASM v4) up to the product-id string
    for _ in 0..4 {
        let Some(&tag) = data.get(pos) else {
            return err("truncated SAB header");
        };
        let n = data.get(pos + 1).copied().unwrap_or(0) as usize;
        let text = data
            .get(pos + 2..(pos + 2 + n).min(data.len()))
            .unwrap_or(&[]);
        if tag == 7 && n > 0 && text.iter().all(|c| (32..127).contains(c)) {
            break;
        }
        pos += 4;
    }

    let mut tokens = SabTokens { data, pos };
    // header: product id, acis version, date strings, then units and tolerances
    let mut floats = vec![];
    while floats.len() < 3 {
        match tokens.next() {
            None => break,
            Some(Err(e)) => return Err(e),
            Some(Ok(Token::Value(Field::Float(f)))) => floats.push(f),
            Some(Ok(_)) => {}
        }
    }
    let units = floats.first().copied().filter(|&u| u > 0.0).unwrap_or(1.0);
    Ok(Model {
        version,
        units_mm: units,
        records: records(tokens)?,
        text: false,
    })
}

struct SabTokens<'a> {
    data: &'a [u8],
    pos: usize,
}

impl SabTokens<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], AcisError> {
        let Some(bytes) = self.data.get(self.pos..self.pos + n) else {
            return err(format!("truncated SAB data at {}", self.pos));
        };
        self.pos += n;
        Ok(bytes)
    }

    fn f64(&mut self) -> Result<f64, AcisError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i64, AcisError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()) as i64)
    }

    fn token(&mut self, tag: u8) -> Result<Token, AcisError> {
        let value = match tag {
            0x04 => Field::Int(self.i32()?),
            0x0C => Field::Ptr(self.i32()?),
            0x15 => Field::Enum(self.i32()?),
            0x06 => Field::Float(self.f64()?),
            0x07 | 0x08 | 0x09 | 0x12 => {
                let size = match tag {
                    0x07 => 1,
                    0x08 => 2,
                    _ => 4,
                };
                let len = self
                    .take(size)?
                    .iter()
                    .rev()
                    .fold(0usize, |acc, &b| acc << 8 | b as usize);
                // a short string at the very end is cut, not an error
                let end = (self.pos + len).min(self.data.len());
                let text = latin1(&self.data[self.pos..end]);
                self.pos = end;
                Field::Str(text)
            }
            0x0D | 0x0E => {
                let len = self.take(1)?[0] as usize;
                let end = (self.pos + len).min(self.data.len());
                let name = latin1(&self.data[self.pos..end]);
                self.pos = end;
                return Ok(if tag == 0x0D {
                    Token::Ident(name)
                } else {
                    Token::SubIdent(name)
                });
            }
            0x13 | 0x14 => Field::Vec3([self.f64()?, self.f64()?, self.f64()?]),
            0x0A => Field::Bool(true),
            0x0B => Field::Bool(false),
            0x0F => return Ok(Token::Open),
            0x10 => return Ok(Token::Close),
            0x11 => return Ok(Token::End),
            // 64-bit int (ASM 2013+ history / ids)
            0x16 => Field::Int(i64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            // 2D vector (ASM)
            0x17 => Field::Vec2([self.f64()?, self.f64()?]),
            _ => return err(format!("unknown SAB tag 0x{tag:02x} at {}", self.pos - 1)),
        };
        Ok(Token::Value(value))
    }
}

impl Iterator for SabTokens<'_> {
    type Item = Result<Token, AcisError>;

    fn next(&mut self) -> Option<Self::Item> {
        let tag = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(self.token(tag))
    }
}

/// Group tokens into records; stops at the end marker (later bytes are never
/// read, so trailing garbage is harmless).
fn records(
    tokens: impl Iterator<Item = Result<Token, AcisError>>,
) -> Result<Vec<Record>, AcisError> {
    let mut records: Vec<Record> = vec![];
    let mut name_parts: Vec<String> = vec![];
    // the open record's fields, then one list per open subtype block
    let mut stack: Vec<Vec<Field>> = vec![];
    let mut current: Option<(String, usize)> = None;

    for tok in tokens {
        let tok = tok?;
        if current.is_none() {
            match tok {
                Token::SubIdent(name) => name_parts.push(name),
                Token::Ident(name) => {
                    name_parts.push(name);
                    let kind = name_parts.join("-");
                    name_parts.clear();
                    if END_MARKERS.contains(&kind.as_str()) {
                        return Ok(records);
                    }
                    current = Some((kind, records.len()));
                    stack.push(vec![]);
                }
                Token::Value(Field::Str(s)) if END_MARKERS.contains(&s.as_str()) => {
                    return Ok(records);
                }
                _ => {}
            }
            continue;
        }
        match tok {
            // e.g. subtype names inside { }
            Token::Ident(name) | Token::SubIdent(name) => {
                stack.last_mut().unwrap().push(Field::Type(name))
            }
            Token::Open => {
                // nested lists are read, walked and freed recursively
                if stack.len() > MAX_ACIS_NESTING {
                    return err(format!(
                        "subtype blocks nested more than {MAX_ACIS_NESTING} deep"
                    ));
                }
                stack.push(vec![])
            }
            Token::Close => {
                if stack.len() > 1 {
                    let inner = stack.pop().unwrap();
                    stack.last_mut().unwrap().push(Field::List(Rc::new(inner)));
                }
            }
            Token::End => finish(&mut current, &mut stack, &mut records),
            Token::Value(v) => stack.last_mut().unwrap().push(v),
        }
    }
    finish(&mut current, &mut stack, &mut records);
    Ok(records)
}

/// Close the open record, attaching any unclosed subtype blocks.
fn finish(
    current: &mut Option<(String, usize)>,
    stack: &mut Vec<Vec<Field>>,
    records: &mut Vec<Record>,
) {
    if let Some((kind, index)) = current.take() {
        while stack.len() > 1 {
            let inner = stack.pop().unwrap();
            stack.last_mut().unwrap().push(Field::List(Rc::new(inner)));
        }
        records.push(Record {
            index,
            kind,
            fields: stack.pop().unwrap_or_default(),
        });
    }
}

// -- text ---------------------------------------------------------------------

fn parse_sat(text: &str) -> Result<Model, AcisError> {
    let text = text.replace('\r', "");
    let lines: Vec<&str> = text.split('\n').collect();
    let head = lines[0].split_whitespace().next().unwrap_or("");
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_digit()) {
        return err("not a SAT stream");
    }
    let version: i64 = head
        .parse()
        .map_err(|_| AcisError("bad SAT version".into()))?;
    // ACIS 2.0+ follows the version line with product and tolerance lines;
    // 1.x (R13/R14 DWGs) starts the records right after it
    let (units, first) = if version < 200 {
        (1.0, 1)
    } else {
        let units = lines
            .get(2)
            .and_then(|l| l.split_whitespace().next())
            .and_then(|w| w.parse().ok())
            .unwrap_or(1.0);
        (units, 3)
    };
    let body: Vec<char> = lines
        .get(first..)
        .unwrap_or(&[])
        .join("\n")
        .chars()
        .collect();
    let tokens = SatTokens {
        text: &body,
        i: 0,
        at_start: true,
        depth: 0,
    };
    Ok(Model {
        version,
        units_mm: units,
        records: records(tokens)?,
        text: true,
    })
}

/// Python's str.isspace() for the characters SAT text can contain.
fn is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

struct SatTokens<'a> {
    text: &'a [char],
    i: usize,
    /// the next word names a record type
    at_start: bool,
    depth: i32,
}

impl Iterator for SatTokens<'_> {
    type Item = Result<Token, AcisError>;

    fn next(&mut self) -> Option<Self::Item> {
        let text = self.text;
        loop {
            let &c = text.get(self.i)?;
            if is_space(c) {
                self.i += 1;
                continue;
            }
            self.i += 1;
            match c {
                '#' => {
                    self.at_start = true;
                    return Some(Ok(Token::End));
                }
                '{' => {
                    self.depth += 1;
                    return Some(Ok(Token::Open));
                }
                '}' => {
                    self.depth -= 1;
                    return Some(Ok(Token::Close));
                }
                _ => {}
            }
            let start = self.i - 1;
            while self.i < text.len() && !is_space(text[self.i]) && !"#{}".contains(text[self.i]) {
                self.i += 1;
            }
            let word: String = text[start..self.i].iter().collect();
            if self.at_start {
                if word.len() > 1
                    && word.starts_with('-')
                    && word[1..].chars().all(|c| c.is_ascii_digit())
                {
                    continue; // optional "-<n>" record number
                }
                self.at_start = false;
                return Some(Ok(Token::Ident(word)));
            }
            if let Some(n) = word.strip_prefix('$') {
                return Some(
                    n.parse()
                        .map(|n| Token::Value(Field::Ptr(n)))
                        .map_err(|_| AcisError(format!("bad SAT pointer {word}"))),
                );
            }
            if let Some(n) = word.strip_prefix('@') {
                // @<len> <string>
                let Ok(len) = n.parse::<usize>() else {
                    return Some(err(format!("bad SAT string length {word}")));
                };
                self.i += 1;
                // a crafted length must not wrap around
                let from = self.i.min(text.len());
                let to = self.i.saturating_add(len).min(text.len());
                self.i = to;
                return Some(Ok(Token::Value(Field::Str(
                    text[from..to].iter().collect(),
                ))));
            }
            if let Ok(i) = word.parse::<i64>() {
                return Some(Ok(Token::Value(Field::Int(i))));
            }
            if let Ok(f) = word.parse::<f64>() {
                return Some(Ok(Token::Value(Field::Float(f))));
            }
            return Some(Ok(Token::Value(if self.depth > 0 {
                Field::Type(word)
            } else {
                Field::Str(word)
            })));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "400 0 1 0\nproduct\n1 9.9999999999999995e-07 1e-10\n";

    fn sab_nested(depth: usize) -> Vec<u8> {
        let mut sab = b"ACIS BinaryFile".to_vec();
        sab.extend(21500i32.to_le_bytes());
        sab.extend(b"\x07\x04test");
        for _ in 0..3 {
            sab.push(6);
            sab.extend(1f64.to_le_bytes());
        }
        sab.extend(b"\x0d\x04body");
        sab.extend(std::iter::repeat_n(0x0f, depth));
        sab.push(0x11);
        sab
    }

    /// Runs on cargo test's 2 MiB thread: reading, walking (B-rep) and
    /// freeing the deepest allowed nesting must fit.
    #[test]
    fn subtype_nesting_is_limited() {
        for depth in [MAX_ACIS_NESTING, MAX_ACIS_NESTING + 1] {
            let sat = format!(
                "{HEADER}body $-1 {}#\nEnd-of-ACIS-data\n",
                "{ ".repeat(depth)
            );
            for data in [AcisData::Sat(sat), AcisData::Sab(sab_nested(depth))] {
                match parse(&data) {
                    Ok(model) => {
                        assert_eq!(depth, MAX_ACIS_NESTING);
                        crate::acis::brep::Brep::new(&model).bodies();
                    }
                    Err(e) => {
                        assert!(depth > MAX_ACIS_NESTING);
                        assert!(e.0.contains("nested more than"), "{e}");
                    }
                }
            }
        }
    }

    #[test]
    fn sat_string_length_past_the_end() {
        // @<usize::MAX>: the end of the string wrapped around
        let sat = format!("{HEADER}body $-1 @18446744073709551615 x #\nEnd-of-ACIS-data\n");
        let model = parse(&AcisData::Sat(sat)).unwrap();
        assert_eq!(model.records[0].kind, "body");
        let sat = format!("{HEADER}body $-1 @3 abc $-1 #\nEnd-of-ACIS-data\n");
        let model = parse(&AcisData::Sat(sat)).unwrap();
        assert_eq!(model.records[0].fields[1], Field::Str("abc".into()));
    }
}
