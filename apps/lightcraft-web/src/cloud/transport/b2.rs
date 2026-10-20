//! The native personal signer owns B2 account authorization and authority CAS.
use super::*;
use lightcraft_engine::cloud_records::b2_control::{self, Action, Reply as ControlReply, Snapshot};
use lightcraft_engine::cloud_status::Recovery;

fn service_error() -> CloudFailure {
    CloudFailure::new(FailureKind::Unsupported, "the personal cloud service does not support this library operation; pending work retained")
        .with_remedy(Recovery::CheckCompatibility)
        .with_recovery("Update LightCraft in this browser and the personal cloud signer, and allow its /authorize/control route, then retry. Check its browser-origin/CORS settings if it cannot be reached.")
}
impl Provider {
    async fn b2_control(&self, action: Action) -> Result<ControlReply, CloudFailure> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(CloudFailure::new(FailureKind::Cancelled, "cloud operation cancelled; pending work retained"));
        }
        if !self.connection.supports_b2_authority() {
            return Err(service_error());
        }
        let endpoint = url(&self.connection.authorize_endpoint)?;
        if !endpoint.search().is_empty() {
            return Err(service_error());
        }
        let endpoint = format!("{}/control", self.connection.authorize_endpoint.trim_end_matches('/'));
        let payload = serde_json::to_string(&b2_control::Request {
            connection: self.connection.id.clone(),
            prefix: self.connection.prefix.clone(),
            bucket_root: self.connection.bucket_root.clone().ok_or_else(service_error)?,
            action,
        })
        .map_err(CloudFailure::from)?;
        if payload.len() > b2_control::REQUEST_LIMIT {
            return Err(CloudFailure::new(FailureKind::Capacity, "cloud control request exceeds its bounded size; pending work retained"));
        }
        let options = RequestInit::new();
        options.set_method("POST");
        options.set_credentials(web_sys::RequestCredentials::Omit);
        options.set_cache(web_sys::RequestCache::NoStore);
        options.set_redirect(web_sys::RequestRedirect::Error);
        options.set_body(&JsValue::from_str(&payload));
        let _deadline = TransferDeadline::new(&options, self.cancel.clone())?;
        let request = Request::new_with_str_and_init(&endpoint, &options).map_err(js_error)?;
        request.headers().set("Content-Type", "application/json").map_err(js_error)?;
        if let Some(token) = self.token.borrow().as_ref() {
            request.headers().set("Authorization", &format!("Bearer {token}")).map_err(js_error)?;
        }
        if let Some(budget) = &self.verification_budget {
            budget.admit(b2_control::RESPONSE_LIMIT).map_err(CloudFailure::from)?;
        }
        let response = fetch(&request, &self.cancel).await.map_err(|mut failure| {
            if failure.kind == FailureKind::Connectivity {
                failure.recovery = service_error().recovery;
            }
            failure
        })?;
        let status = response.status();
        let bytes = body(&response, b2_control::RESPONSE_LIMIT, &self.cancel).await?;
        if let Some(budget) = &self.verification_budget {
            budget.read(b2_control::RESPONSE_LIMIT, bytes.len());
        }
        match ControlReply::decode(&bytes) {
            Ok(ControlReply::Failed { failure }) => Err(failure),
            Ok(reply) if status == 200 => Ok(reply),
            _ if matches!(status, 400 | 404 | 405 | 501) => Err(service_error()),
            _ if status != 200 => Err(CloudFailure::http("personal cloud control", status)),
            Err(failure) if failure.remedy != Some(Recovery::CheckCompatibility) => Err(failure),
            _ => Err(service_error()),
        }
    }
    pub async fn b2_read(&self, for_upgrade: bool) -> Result<Option<Snapshot>, CloudFailure> {
        match self.b2_control(Action::Read { for_upgrade }).await? {
            ControlReply::Read { current } => {
                if current.as_ref().is_some_and(|s| s.token.len() > 128 || !s.token.starts_with("b2:") || s.token.chars().any(char::is_control)) {
                    return Err(CloudFailure::integrity("personal cloud service returned an invalid authority token"));
                }
                Ok(current)
            }
            _ => Err(service_error()),
        }
    }
    pub async fn b2_initialize(&self, head: &lightcraft_engine::cloud_records::Head) -> Result<(), CloudFailure> {
        self.b2_updated(Action::Initialize { head: Box::new(head.clone()) }).await
    }
    pub async fn b2_write(&self, expected: &str, head: &lightcraft_engine::cloud_records::Head) -> Result<(), CloudFailure> {
        self.b2_updated(Action::Write { expected: expected.into(), head: Box::new(head.clone()) }).await
    }
    pub async fn b2_fence(&self, expected: &str, library: &str) -> Result<(), CloudFailure> {
        self.b2_updated(Action::Fence { expected: expected.into(), library: library.into() }).await
    }
    async fn b2_updated(&self, action: Action) -> Result<(), CloudFailure> {
        match self.b2_control(action).await? {
            ControlReply::Updated {} => Ok(()),
            _ => Err(service_error()),
        }
    }
}
