//! Private synchronous SQL contract. Hosts materialize each statement in one turn.
//! Only text and null cross this seam; integers are canonical i64 decimal text.
use crate::{Error, Result};

pub type Cell = Option<String>;

/// Executes a read-only statement, rejecting mutation before stepping it.
/// Results must be materialized text/null cells, never SQLite numeric values.
pub trait ReadSql {
    fn rows(&self, sql: &str, binds: &[Cell]) -> Result<Vec<Row>>;

    /// Hosts may retain their compatible storage-error source for a missing row.
    fn missing_row(&self) -> Error {
        invalid("query returned no rows")
    }
}

/// Executes a mutation with RETURNING inside the host's atomic write closure.
/// Any closure error must roll back all preceding statements.
pub trait WriteSql: ReadSql {
    fn write(&mut self, sql: &str, binds: &[Cell]) -> Result<Vec<Row>>;
}

impl<S: ReadSql + ?Sized> ReadSql for &S {
    fn rows(&self, sql: &str, binds: &[Cell]) -> Result<Vec<Row>> {
        (**self).rows(sql, binds)
    }
    fn missing_row(&self) -> Error {
        (**self).missing_row()
    }
}
impl<S: ReadSql + ?Sized> ReadSql for &mut S {
    fn rows(&self, sql: &str, binds: &[Cell]) -> Result<Vec<Row>> {
        (**self).rows(sql, binds)
    }
    fn missing_row(&self) -> Error {
        (**self).missing_row()
    }
}
impl<S: WriteSql + ?Sized> WriteSql for &mut S {
    fn write(&mut self, sql: &str, binds: &[Cell]) -> Result<Vec<Row>> {
        (**self).write(sql, binds)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid SQL value: {0}")]
struct Invalid(&'static str);

pub(crate) fn invalid(message: &'static str) -> Error {
    Error::Storage(Box::new(Invalid(message)))
}

pub(crate) fn integer(text: &str) -> Result<i64> {
    let value = text
        .parse::<i64>()
        .map_err(|_| invalid("expected i64 decimal"))?;
    if value.to_string() != text {
        return Err(invalid("noncanonical integer"));
    }
    Ok(value)
}

// Validation is shared with typed binding: never let SQLite's permissive CAST
// decide whether a decimal string is an integer.
pub(crate) fn integer_bind(text: &str) -> Result<Cell> {
    integer(text)?;
    Ok(Some(text.to_owned()))
}

pub(crate) trait Bind {
    fn bind(&self) -> Result<Cell>;
}
impl Bind for str {
    fn bind(&self) -> Result<Cell> {
        Ok(Some(self.to_owned()))
    }
}
impl Bind for String {
    fn bind(&self) -> Result<Cell> {
        self.as_str().bind()
    }
}
impl<T: Bind + ?Sized> Bind for &T {
    fn bind(&self) -> Result<Cell> {
        (**self).bind()
    }
}
impl Bind for i64 {
    fn bind(&self) -> Result<Cell> {
        integer_bind(&self.to_string())
    }
}
impl Bind for u32 {
    fn bind(&self) -> Result<Cell> {
        i64::from(*self).bind()
    }
}
impl Bind for bool {
    fn bind(&self) -> Result<Cell> {
        i64::from(*self).bind()
    }
}
impl<T: Bind> Bind for Option<T> {
    fn bind(&self) -> Result<Cell> {
        self.as_ref()
            .map(Bind::bind)
            .transpose()
            .map(Option::flatten)
    }
}

macro_rules! params {
    ($($value:expr),* $(,)?) => { &[$($crate::sql::Bind::bind(&$value)?),*] };
}
pub(crate) use params;

#[derive(Debug)]
pub struct Row(pub Vec<Cell>);
impl Row {
    pub(crate) fn get<T: Decode>(&self, index: usize) -> Result<T> {
        T::decode(
            self.0
                .get(index)
                .ok_or_else(|| invalid("column index"))?
                .as_deref(),
        )
    }
}
pub(crate) trait Decode: Sized {
    fn decode(cell: Option<&str>) -> Result<Self>;
}
impl Decode for String {
    fn decode(cell: Option<&str>) -> Result<Self> {
        cell.map(str::to_owned)
            .ok_or_else(|| invalid("unexpected null"))
    }
}
impl Decode for i64 {
    fn decode(cell: Option<&str>) -> Result<Self> {
        integer(cell.ok_or_else(|| invalid("unexpected null"))?)
    }
}
impl Decode for u32 {
    fn decode(cell: Option<&str>) -> Result<Self> {
        u32::try_from(i64::decode(cell)?).map_err(|_| invalid("u32 out of range"))
    }
}
impl Decode for bool {
    fn decode(cell: Option<&str>) -> Result<Self> {
        match i64::decode(cell)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("expected boolean 0 or 1")),
        }
    }
}
impl<T: Decode> Decode for Option<T> {
    fn decode(cell: Option<&str>) -> Result<Self> {
        cell.map(|text| T::decode(Some(text))).transpose()
    }
}

pub(crate) fn query<T>(
    sql: &(impl ReadSql + ?Sized),
    statement: &str,
    binds: &[Cell],
    decode: impl FnMut(&Row) -> Result<T>,
) -> Result<Vec<T>> {
    sql.rows(statement, binds)?.iter().map(decode).collect()
}
pub(crate) fn optional<T>(
    sql: &(impl ReadSql + ?Sized),
    statement: &str,
    binds: &[Cell],
    decode: impl FnOnce(&Row) -> Result<T>,
) -> Result<Option<T>> {
    sql.rows(statement, binds)?.first().map(decode).transpose()
}
pub(crate) fn one<T>(
    sql: &(impl ReadSql + ?Sized),
    statement: &str,
    binds: &[Cell],
    decode: impl FnOnce(&Row) -> Result<T>,
) -> Result<T> {
    let rows = sql.rows(statement, binds)?;
    let row = rows.first().ok_or_else(|| sql.missing_row())?;
    decode(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_decoding_requires_canonical_i64_text() -> Result<()> {
        for value in [
            i64::MIN,
            -9_007_199_254_740_993,
            0,
            9_007_199_254_740_993,
            i64::MAX,
        ] {
            let row = Row(vec![value.bind()?]);
            assert_eq!(row.get::<i64>(0)?, value);
        }
        for text in [
            "",
            "+1",
            "01",
            "-0",
            " 1",
            "1 ",
            "1.0",
            "1e2",
            "1x",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            assert!(
                Row(vec![Some(text.into())]).get::<i64>(0).is_err(),
                "{text:?}"
            );
        }
        let row = Row(vec![None]);
        assert_eq!(row.get::<Option<i64>>(0)?, None);
        assert!(row.get::<i64>(0).is_err());
        assert!(row.get::<String>(1).is_err());
        Ok(())
    }

    #[test]
    fn boolean_and_u32_decoding_are_bounded() -> Result<()> {
        for (text, expected) in [("0", false), ("1", true)] {
            assert_eq!(Row(vec![Some(text.into())]).get::<bool>(0)?, expected);
        }
        for text in ["-1", "2", "01", "true"] {
            assert!(Row(vec![Some(text.into())]).get::<bool>(0).is_err());
        }
        for (text, expected) in [("0", 0), ("4294967295", u32::MAX)] {
            assert_eq!(Row(vec![Some(text.into())]).get::<u32>(0)?, expected);
        }
        for text in ["-1", "4294967296", "01", "1.0"] {
            assert!(Row(vec![Some(text.into())]).get::<u32>(0).is_err());
        }
        let row = Row(vec![None]);
        assert!(row.get::<bool>(0).is_err());
        assert!(row.get::<u32>(0).is_err());
        assert_eq!(row.get::<Option<u32>>(0)?, None);
        Ok(())
    }
}
