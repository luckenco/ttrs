//! Synthetic capability core. No SDK, async runtime, product rules, or billing.
//! ABI: `host.sql(id, pointer, length)` consumes borrowed UTF-8 synchronously.
//! IDs 1..4 are mutation steps; 5..10 emit exact numeric/calendar text; 11 sets
//! a synthetic SQLite sequence. The host must never retain pointers or reenter.
//! `attempt(mode)` returns 1 for domain rejection after partial writes; the host
//! MUST throw inside transactionSync to roll back. Modes 2/3 throw/trap instead.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "host")]
unsafe extern "C" {
    fn sql(id: u32, pointer: *const u8, length: usize);
}

fn emit(id: u32, text: &str) {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: host consumes the live string during this synchronous call only.
    unsafe {
        sql(id, text.as_ptr(), text.len());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (id, text);
}

/// Make a synthetic partial mutation. 0 commits, 1 rejects, 2 invokes a failing
/// SQL statement, 3 executes Wasm unreachable. A trapped instance is disposable.
#[unsafe(no_mangle)]
pub extern "C" fn attempt(mode: u32) -> u32 {
    assert!(mode <= 3);
    emit(1, "dirty");
    emit(2, "");
    emit(3, "attempt");
    match mode {
        1 => 1,
        2 => {
            emit(4, "");
            0
        }
        3 => {
            #[cfg(target_arch = "wasm32")]
            core::arch::wasm32::unreachable();
            #[cfg(not(target_arch = "wasm32"))]
            panic!("native trap equivalent");
        }
        _ => 0,
    }
}

fn calendar(months: u32) -> i128 {
    let zone = jiff::tz::TimeZone::get("America/New_York").expect("bundled zone");
    let mut date = jiff::civil::date(1900, 1, 1);
    let mut nanos = 0;
    for _ in 0..months {
        let next = date.checked_add(jiff::Span::new().months(1)).unwrap();
        let from = date.to_zoned(zone.clone()).unwrap();
        let to = next.to_zoned(zone.clone()).unwrap();
        nanos += to.timestamp().as_nanosecond() - from.timestamp().as_nanosecond();
        date = next;
    }
    nanos
}

/// Emit the parent lab's exact i64, i128, Timestamp::MAX and bundled-zone
/// calendar capabilities. Decimal parsing assertions happen in Rust, not JS.
#[unsafe(no_mangle)]
pub extern "C" fn exact() {
    for value in [
        i64::MIN,
        -9_007_199_254_740_993,
        0,
        9_007_199_254_740_993,
        i64::MAX,
    ] {
        let text = value.to_string();
        assert_eq!(text.parse::<i64>(), Ok(value));
        emit(5, &text);
    }
    let wide = i128::from(i64::MAX) * i128::from(i64::MAX);
    let text = wide.to_string();
    assert_eq!(text.parse::<i128>(), Ok(wide));
    emit(6, &text);
    let timestamp = jiff::Timestamp::MAX;
    let text = timestamp.to_string();
    assert_eq!(text.parse::<jiff::Timestamp>().unwrap(), timestamp);
    emit(7, &text);
    emit(8, &timestamp.as_nanosecond().to_string());
    emit(9, &timestamp.as_microsecond().to_string());
    emit(10, &calendar(2400).to_string());
    emit(11, "9007199254740993");
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_capabilities() {
        super::exact();
        assert_eq!(super::calendar(1), 2_678_400_000_000_000);
        assert_eq!(super::calendar(2400), 6_311_433_600_000_000_000);
        assert_eq!(super::attempt(0), 0);
        assert_eq!(super::attempt(1), 1);
    }
}
