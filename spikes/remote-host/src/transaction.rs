//! The only missing SDK binding. Callback results throw via wasm-bindgen's
//! Result ABI after Rust locals are dropped; never use throw_val in Rust frames.
use wasm_bindgen::{JsCast, JsValue, closure::ScopedClosure, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = js_sys::Object)]
    type SyncStorage;

    #[wasm_bindgen(method, catch, js_name = transactionSync)]
    fn transaction_sync(
        this: &SyncStorage,
        callback: &ScopedClosure<dyn FnMut() -> Result<JsValue, JsValue>>,
    ) -> Result<JsValue, JsValue>;
}

pub(crate) fn lifetime(
    storage: &worker_sys::DurableObjectStorage,
    mut action: impl FnMut() -> Result<JsValue, JsValue>,
) -> Result<Result<JsValue, JsValue>, JsValue> {
    let saved: js_sys::Function;
    {
        let callback = ScopedClosure::borrow_mut_aborting(&mut action);
        saved = callback
            .as_ref()
            .unchecked_ref::<js_sys::Function>()
            .clone();
        storage
            .unchecked_ref::<SyncStorage>()
            .transaction_sync(&callback)?;
    }
    // The platform (or other JS) may retain the function, but the Rust borrow
    // must already be invalid. This must throw before invoking Rust captures.
    Ok(saved.call0(&JsValue::NULL))
}

pub(crate) fn sync(
    storage: &worker_sys::DurableObjectStorage,
    mut action: impl FnMut() -> Result<JsValue, JsValue>,
) -> Result<JsValue, JsValue> {
    let callback = ScopedClosure::borrow_mut_aborting(&mut action);
    storage
        .unchecked_ref::<SyncStorage>()
        .transaction_sync(&callback)
}
