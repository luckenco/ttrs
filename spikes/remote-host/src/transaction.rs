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

pub(crate) fn sync(
    storage: &worker_sys::DurableObjectStorage,
    mut action: impl FnMut() -> Result<JsValue, JsValue>,
) -> Result<JsValue, JsValue> {
    let callback = ScopedClosure::borrow_mut_aborting(&mut action);
    storage
        .unchecked_ref::<SyncStorage>()
        .transaction_sync(&callback)
}
