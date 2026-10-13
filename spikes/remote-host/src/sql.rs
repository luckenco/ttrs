//! Static SQL, exact text/null cells, synchronous cursor consumption.
use std::cell::Cell;
use wasm_bindgen::JsValue;

pub(crate) type Rows = Vec<Vec<Option<String>>>;

pub(crate) struct Sql {
    inner: worker::SqlStorage,
    pub crossings: Cell<u64>,
}

impl Sql {
    pub fn new(inner: worker::SqlStorage) -> Self {
        Self {
            inner,
            crossings: Cell::new(0),
        }
    }

    pub fn rows(&self, statement: &'static str, binds: &[Option<String>]) -> Result<Rows, JsValue> {
        self.crossings.set(self.crossings.get() + 1);
        let binds = binds
            .iter()
            .map(|v| match v {
                Some(text) => worker::SqlStorageValue::String(text.clone()),
                None => worker::SqlStorageValue::Null,
            })
            .collect::<Vec<_>>();
        self.inner
            .exec(statement, binds)
            .map_err(error)?
            .raw()
            .map(|row| {
                row.map_err(error)?
                    .into_iter()
                    .map(|value| match value {
                        worker::SqlStorageValue::Null => Ok(None),
                        worker::SqlStorageValue::String(text) => Ok(Some(text)),
                        _ => Err(JsValue::from_str(
                            "numeric/blob result rejected: missing text cast",
                        )),
                    })
                    .collect()
            })
            .collect()
    }

    pub fn size(&self) -> usize {
        self.inner.database_size()
    }
}

pub(crate) fn error(error: worker::Error) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

pub(crate) fn integer(text: &str) -> Result<i64, &'static str> {
    let value: i64 = text.parse().map_err(|_| "invalid i64")?;
    if value.to_string() != text {
        return Err("noncanonical i64");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_integer() {
        for value in [
            i64::MIN,
            -9_007_199_254_740_993,
            0,
            9_007_199_254_740_993,
            i64::MAX,
        ] {
            assert_eq!(super::integer(&value.to_string()), Ok(value));
        }
        for text in ["+1", "01", "-0", " 1", "1.0", "9223372036854775808"] {
            assert!(super::integer(text).is_err(), "{text}");
        }
    }
}
