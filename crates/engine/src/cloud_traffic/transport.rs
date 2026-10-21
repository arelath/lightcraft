use super::*;
use lightcraft_fetch::http::{self, HttpError, Limits, Method, RequestBody, Url};

struct Attempt {
    context: Context,
    record: Record,
    start: Instant,
    hour: u64,
}
impl Attempt {
    fn new(destination: &str, resource: &str, transport: &'static str, kind: &'static str, method: Method) -> Self {
        let context = CONTEXT.with(|c| c.borrow().clone());
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        let id = registry.serial().unwrap_or(u64::MAX);
        let hour = registry.start.elapsed().as_secs() / 3600;
        registry.add(
            &context,
            Some(destination),
            hour,
            &Totals { attempts: 1, background_poll_attempts: u64::from(context.background_poll), ..Default::default() },
        );
        let ordinal = context.path.last().and_then(|id| registry.operations.iter().find(|o| o.id == *id)).map(|o| o.totals.attempts);
        let record = Record {
            background_poll: context.background_poll,
            id,
            operation_path: context.path.clone(),
            purpose: context.purpose,
            library: context.library.clone(),
            import: context.import.clone(),
            task: context.task,
            destination: destination.into(),
            resource: identity(resource),
            transport,
            kind,
            method: method.as_str(),
            attempt_in_operation: ordinal,
            status: None,
            provider_refusal: None,
            body_bytes_sent: 0,
            body_bytes_read: 0,
            body_complete: false,
            transport_failure: false,
            elapsed_ms: 0,
        };
        Self { context, record, start: Instant::now(), hour }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.record.elapsed_ms = milliseconds(self.start.elapsed());
        let totals = Totals {
            finished_attempts: 1,
            background_poll_body_bytes_read: if self.context.background_poll { self.record.body_bytes_read } else { 0 },
            responses: u64::from(self.record.status.is_some()),
            body_bytes_sent: self.record.body_bytes_sent,
            body_bytes_read: self.record.body_bytes_read,
            complete_bodies: u64::from(self.record.body_complete),
            transport_failures: u64::from(self.record.transport_failure),
            http_errors: u64::from(self.record.status.is_some_and(|s| s >= 400)),
            provider_caps: u64::from(self.record.provider_refusal.is_some()),
            elapsed_ms: self.record.elapsed_ms,
            ..Default::default()
        };
        {
            let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
            registry.add(&self.context, Some(&self.record.destination), self.hour, &totals);
            if registry.recent.len() >= MAX_REQUESTS {
                registry.recent.pop_front();
            }
            registry.recent.push_back(self.record.clone());
        }
        if log::log_enabled!(target:"lightcraft_cloud_requests", log::Level::Debug)
            && let Ok(json) = serde_json::to_string(&self.record)
        {
            log::debug!(target:"lightcraft_cloud_requests", "{json}");
        }
    }
}

/// Owns the complete streaming request lifetime, including error-body inspection.
pub(crate) struct Response {
    raw: http::Response,
    attempt: Attempt,
    admission: Option<crate::cloud_admission::Scope>,
}
impl std::ops::Deref for Response {
    type Target = http::Response;
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}
impl Response {
    pub fn read(&mut self, output: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        match self.raw.read(output, limits) {
            Ok(n) => {
                self.attempt.record.body_bytes_read = self.attempt.record.body_bytes_read.saturating_add(n as u64);
                if n == 0 || self.raw.content_length() == Some(0) {
                    self.attempt.record.body_complete = true;
                }
                Ok(n)
            }
            Err(error) => {
                self.attempt.record.transport_failure = true;
                Err(error)
            }
        }
    }
    pub(crate) fn admitted(mut self, scope: crate::cloud_admission::Scope) -> Self {
        self.admission = Some(scope);
        self
    }
    pub(crate) fn throttled(&self) -> Option<crate::assets::AssetError> {
        matches!(self.status, 429 | 503).then(|| {
            self.admission.map_or(crate::assets::AssetError::Throttled { status: self.status, retry_after_ms: 2000 }, |scope| {
                scope.throttle(self.status, self.header("retry-after"))
            })
        })
    }
    pub fn provider_cap(&mut self) {
        self.provider_cap_code("b2DownloadOrClassBCapExceeded");
    }
    pub(crate) fn provider_cap_code(&mut self, code: &'static str) {
        if let Some(scope) = self.admission {
            scope.cap(self.status);
        }
        self.attempt.record.provider_refusal = Some(code);
    }
}

pub(crate) struct Target<'a> {
    pub destination: &'a str,
    pub resource: &'a str,
    pub transport: &'static str,
    pub kind: &'static str,
    pub method: Method,
}
pub(crate) fn request(
    target: Target<'_>,
    url: &Url,
    headers: &[(&str, String)],
    body: Option<RequestBody<'_>>,
    limits: &Limits,
    progress: &mut dyn FnMut(u64),
) -> Result<Response, HttpError> {
    let mut attempt = Attempt::new(target.destination, target.resource, target.transport, target.kind, target.method);
    match http::request(target.method, url, headers, body, limits, &mut |done| {
        attempt.record.body_bytes_sent = attempt.record.body_bytes_sent.max(done);
        progress(done);
    }) {
        Ok(raw) => {
            attempt.record.status = Some(raw.status);
            attempt.record.body_complete = target.method == Method::Head || raw.content_length() == Some(0) || matches!(raw.status, 204 | 304);
            Ok(Response { raw, attempt, admission: None })
        }
        Err(error) => {
            attempt.record.transport_failure = true;
            Err(error)
        }
    }
}
