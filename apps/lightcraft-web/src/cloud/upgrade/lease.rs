//! Poll renewal concurrently with the expensive migration future. Dropping either
//! branch aborts its Fetch; ownership failure never sets the user's cancel flag.
use super::{Provider, transition_control};
use lightcraft_engine::cloud_control::lease::Lease;
use lightcraft_engine::cloud_status::CloudFailure;
use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    task::Poll,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

struct Timer {
    window: web_sys::Window,
    id: i32,
    _callback: Closure<dyn FnMut()>,
}
impl Drop for Timer {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.id);
    }
}
async fn tick() -> Result<(), CloudFailure> {
    let window = web_sys::window().ok_or_else(|| CloudFailure::local("browser timer unavailable for upgrade ownership"))?;
    let mut callback = None;
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        callback = Some(Closure::<dyn FnMut()>::new(move || {
            let _ = resolve.call0(&JsValue::NULL);
        }));
    });
    let callback = callback.ok_or_else(|| CloudFailure::local("could not create upgrade renewal timer"))?;
    let id = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), 200)
        .map_err(|_| CloudFailure::local("could not start upgrade renewal timer"))?;
    let _timer = Timer { window, id, _callback: callback };
    JsFuture::from(promise).await.map_err(|_| CloudFailure::local("upgrade renewal timer failed"))?;
    Ok(())
}

pub(super) async fn run<T>(
    provider: &Provider,
    library: &str,
    lease: &RefCell<Lease>,
    work: impl Future<Output = Result<T, CloudFailure>>,
) -> Result<T, CloudFailure> {
    let heartbeat = async {
        loop {
            if let Err(error) = tick().await {
                return error;
            }
            if !lease.borrow().due(super::now_ms()) {
                continue;
            }
            let renewed = match transition_control(provider, library, |_, c| lease.borrow().renewal(c, super::now_ms())).await {
                Ok(head) => head,
                Err(error) => return error.message(),
            };
            let control = match renewed.control() {
                Ok(c) => c,
                Err(error) => return CloudFailure::from(error),
            };
            if control.migration.is_none() {
                // Only the registered successful terminal control is accepted here.
                std::future::pending::<()>().await;
            }
            lease.borrow_mut().confirmed(&control);
        }
    };
    let mut heartbeat = std::pin::pin!(heartbeat);
    let mut work = std::pin::pin!(work);
    poll_fn(|cx| {
        if let Poll::Ready(error) = heartbeat.as_mut().poll(cx) {
            return Poll::Ready(Err(error));
        }
        work.as_mut().poll(cx)
    })
    .await
}
