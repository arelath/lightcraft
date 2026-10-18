//! Cloud setup: File ▸ Cloud Synchronization… on a library that doesn't sync yet, and File ▸
//! Cloud Storage…, as a dialog in steps.
//!
//! 1. **Where**: Backblaze B2, Amazon S3 or another S3-compatible service.
//! 2. **Connect**: the bucket and an explicit authentication source (saved/new keys, a named
//!    profile or environment/default credentials). Next checks them first: a
//!    read-only listing on a worker thread, with its row in the activity stack, that also finds
//!    the libraries already in the folder. Cloud Storage… saves the connection here.
//! 3. **Library**: start a new cloud library from this catalog, or join one found in the folder
//!    or named by its ID. The protocol follows: a found library's own, else the provider's
//!    (append-only on B2 and other services, conditional writes on AWS S3).
//!
//! Typed keys reach only the check and, on finishing, the AWS shared credentials file (profile
//! a unique `lightcraft-key-<id>`, `lightcraft_engine::assets::s3::save_profile`): never command
//! parameters, settings or the catalog.

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;
use lightcraft_catalog::replication::SyncProtocol;
use lightcraft_engine::cloud_setup::{Probe, Provider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Authentication {
    SavedKeys,
    #[default]
    NewKeys,
    Profile,
    Environment,
}
impl Authentication {
    fn label(self) -> &'static str {
        match self {
            Self::SavedKeys => "Saved keys",
            Self::NewKeys => "New access keys",
            Self::Profile => "Named AWS profile",
            Self::Environment => "Environment / default profile",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::SavedKeys => "savedKeys",
            Self::NewKeys => "newKeys",
            Self::Profile => "profile",
            Self::Environment => "environment",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    #[default]
    Provider,
    Connect,
    Library,
}

/// A typed secret key: never printed or serialized.
#[derive(Clone, Default, PartialEq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A fresh, device-local credential binding for new keys. Cloning the form keeps
/// this binding stable through probing and saving; restoring a form requires a new probe.
#[derive(Clone, Debug, PartialEq)]
pub struct NewProfile(pub Option<String>);
impl Default for NewProfile {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let profile = lightcraft_engine::cloud_setup::fresh_profile().ok();
        #[cfg(target_arch = "wasm32")]
        let profile = None;
        Self(profile)
    }
}

/// The connection check of step 2.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Check {
    #[default]
    Untested,
    Running,
    Passed {
        found: Probe,
    },
    Failed {
        error: String,
    },
}

/// The library step's choice.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Choice {
    #[default]
    New,
    /// A library the check found in the folder.
    Found(String),
    /// A library named by its ID (shown in Info on a device that syncs it).
    ById,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Setup {
    pub step: Step,
    pub provider: Provider,
    /// Only the storage connection: File ▸ Cloud Storage… (it ends after step 2).
    pub storage_only: bool,
    pub name: String,
    pub bucket: String,
    /// B2: the bucket's endpoint as B2 shows it. Other: the service's endpoint URL.
    pub endpoint: String,
    pub region: String,
    pub prefix: String,
    pub key_id: String,
    #[serde(skip)]
    pub secret: Secret,
    /// An AWS profile to sign with instead of typed keys.
    pub profile: String,
    /// An earlier setup saved this connection's keys: blank key fields keep them.
    pub saved_keys: bool,
    pub authentication: Authentication,
    /// The original saved credential binding survives a connection-name edit.
    pub saved_profile: Option<String>,
    #[serde(skip)]
    pub new_profile: NewProfile,
    pub check: Check,
    /// What the check checked ([`fingerprint`]): a passed check is for these settings only.
    pub checked: u64,
    /// Go on to the next step once the running check passes.
    pub advance: bool,
    pub choice: Choice,
    pub join_id: String,
    /// The check passed for Cloud Storage…: the dialog saves the connection (`dialogs::show`).
    pub finish: bool,
}

impl Setup {
    /// The check passed for the settings as they are now.
    pub fn passed(&self) -> Option<&Probe> {
        match &self.check {
            Check::Passed { found } if self.checked == fingerprint(self) => Some(found),
            _ => None,
        }
    }

    /// What the last passed check found (the library step lists it).
    fn found(&self) -> Option<&Probe> {
        match &self.check {
            Check::Passed { found } => Some(found),
            _ => None,
        }
    }

    pub fn title(&self) -> &'static str {
        if self.storage_only { "Cloud Storage" } else { "Cloud Synchronization" }
    }

    /// The dialog's OK button.
    pub fn ok_label(&self) -> &'static str {
        match self.step {
            Step::Provider => "Next",
            Step::Connect if self.storage_only => "Save Connection",
            Step::Connect => "Next",
            Step::Library => "Enable & Synchronize",
        }
    }

    pub fn can_confirm(&self) -> bool {
        match self.step {
            Step::Provider => true,
            Step::Connect => self.check != Check::Running,
            Step::Library => match &self.choice {
                Choice::New => !self.new_blocked(),
                Choice::Found(_) => true,
                Choice::ById => lightcraft_catalog::assets::identifier(self.join_id.trim()),
            },
        }
    }

    /// OK finishes the dialog (rather than going on to the next step).
    pub fn finishes(&self) -> bool {
        match self.step {
            Step::Provider => false,
            Step::Connect => self.storage_only && self.passed().is_some(),
            Step::Library => true,
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.step != Step::Provider
    }

    pub fn back(&mut self) {
        self.step = match self.step {
            Step::Library => Step::Connect,
            _ => Step::Provider,
        };
        self.advance = false;
    }

    /// A conditional-head library takes a folder of its own.
    fn new_blocked(&self) -> bool {
        self.provider.new_library_protocol() == SyncProtocol::ConditionalHead && self.found().is_some_and(|found| found.head_taken)
    }

    fn default_name(&self) -> &'static str {
        match self.provider {
            Provider::Backblaze => "b2",
            Provider::Aws => "s3",
            Provider::Other => "storage",
        }
    }

    /// Only the selected authentication source can supply credentials to probe/save.
    fn profile_name(&self) -> Result<Option<String>, String> {
        match self.authentication {
            Authentication::SavedKeys if self.saved_keys => self
                .saved_profile
                .clone()
                .filter(|profile| !profile.trim().is_empty())
                .map(Some)
                .ok_or_else(|| crate::i18n::tr("The saved credential binding is missing; choose new keys or a named profile.").into()),
            Authentication::SavedKeys => {
                Err(crate::i18n::tr("Choose new keys, a named profile or default credentials; this connection has no saved keys.").into())
            }
            Authentication::NewKeys => {
                if self.key_id.trim().is_empty() || self.secret.0.trim().is_empty() {
                    return Err(crate::i18n::tr("Enter both the key ID and its secret key.").into());
                }
                self.new_profile
                    .0
                    .clone()
                    .filter(|profile| lightcraft_engine::cloud_setup::managed_profile(profile))
                    .map(Some)
                    .ok_or_else(|| crate::i18n::tr("Could not create a credentials profile; reopen cloud setup and retry.").into())
            }
            Authentication::Profile => {
                let profile = self.profile.trim();
                if profile.is_empty() {
                    return Err(crate::i18n::tr("Enter the AWS profile name.").into());
                }
                Ok(Some(profile.into()))
            }
            Authentication::Environment => Ok(None),
        }
    }

    /// `cloud.configure` parameters (no keys), or what to fix first.
    pub fn params(&self) -> Result<Value, String> {
        let name = self.name.trim();
        if !lightcraft_catalog::assets::identifier(name) {
            return Err(crate::i18n::tr("Name the connection with letters, digits, - or _ (More options).").to_string());
        }
        let bucket = self.bucket.trim();
        if bucket.is_empty() {
            return Err(crate::i18n::tr("Enter the bucket's name.").to_string());
        }
        let region = |fallback: &str| Some(self.region.trim()).filter(|r| !r.is_empty()).unwrap_or(fallback).to_ascii_lowercase();
        let (endpoint, region) = match self.provider {
            Provider::Backblaze => match lightcraft_engine::cloud_setup::b2_endpoint(&self.endpoint) {
                Some((endpoint, region)) => (Some(endpoint), region),
                None => {
                    return Err(
                        crate::i18n::tr("Enter the bucket's endpoint as Backblaze shows it, such as s3.us-west-004.backblazeb2.com.").to_string()
                    );
                }
            },
            Provider::Aws => (None, region("us-east-1")),
            Provider::Other => {
                let typed = self.endpoint.trim().trim_end_matches('/');
                if typed.is_empty() {
                    return Err(crate::i18n::tr("Enter the service's S3 endpoint.").to_string());
                }
                let url = if typed.contains("://") { typed.to_owned() } else { format!("https://{typed}") };
                (Some(url), region("us-east-1"))
            }
        };
        let mut params = json!({"id": name, "bucket": bucket, "region": region, "prefix": self.prefix.trim().trim_matches('/')});
        if let Some(endpoint) = endpoint {
            params["endpoint"] = json!(endpoint);
        }
        if let Some(profile) = self.profile_name()? {
            params["profile"] = json!(profile);
        }
        Ok(params)
    }

    /// The library step's protocol and library ID (None: a new library).
    fn library(&self) -> (SyncProtocol, Option<String>) {
        let own = self.provider.new_library_protocol();
        match &self.choice {
            Choice::New => (own, None),
            Choice::Found(id) => {
                let protocol = self.found().and_then(|found| found.libraries.iter().find(|l| &l.id == id)).map_or(own, |l| l.protocol);
                (protocol, Some(id.clone()))
            }
            Choice::ById => (own, Some(self.join_id.trim().to_owned())),
        }
    }

    /// After the check passed: the library step, or saving (Cloud Storage…).
    fn go_on(&mut self) {
        if self.storage_only {
            self.finish = true;
            return;
        }
        self.step = Step::Library;
        if let Some(only) = self.found().filter(|found| found.libraries.len() == 1).and_then(|found| found.libraries.first()) {
            self.choice = Choice::Found(only.id.clone());
        } else if self.new_blocked() {
            self.choice = Choice::ById;
        }
    }
}

/// What a check depends on: a change to any of it makes a passed check stale.
pub fn fingerprint(setup: &Setup) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (setup.provider as u8, &setup.name, &setup.bucket, &setup.endpoint, &setup.region, &setup.prefix).hash(&mut hasher);
    (&setup.key_id, &setup.secret.0, &setup.profile, setup.saved_keys).hash(&mut hasher);
    (setup.authentication as u8, &setup.saved_profile, &setup.new_profile.0).hash(&mut hasher);
    hasher.finish()
}

/// A connection check running on a worker thread (see [`poll`]).
pub struct CheckTask {
    rx: std::sync::mpsc::Receiver<Result<Probe, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fingerprint: u64,
    /// Its row in the activity stack ("Checking cloud storage").
    _guard: lightcraft_engine::activity::TaskGuard,
}

/// Open the setup, filled in from the saved connection `connection` (else the first saved one).
#[cfg(not(target_arch = "wasm32"))]
pub fn open(app: &mut LightcraftApp, connection: Option<&str>, storage_only: bool) -> Result<Value, String> {
    let saved = app.run("cloud.connections", json!({}))?;
    let connections = saved.get("connections").and_then(Value::as_array).cloned().unwrap_or_default();
    let found = connections
        .iter()
        .find(|c| connection.is_none_or(|id| c.get("id").and_then(Value::as_str) == Some(id)))
        .and_then(|c| serde_json::from_value::<lightcraft_engine::assets::s3::S3Connection>(c.clone()).ok());
    let mut setup = Setup { storage_only, prefix: "lightcraft".into(), ..Default::default() };
    if let Some(c) = found {
        setup.provider = Provider::of(&c);
        setup.step = Step::Connect;
        setup.saved_keys = c.profile.as_deref().is_some_and(|profile| lightcraft_engine::cloud_setup::saved_profile(&c.id, profile));
        setup.authentication = if setup.saved_keys {
            setup.saved_profile = c.profile.clone();
            Authentication::SavedKeys
        } else if c.profile.is_some() {
            Authentication::Profile
        } else {
            Authentication::Environment
        };
        if !setup.saved_keys {
            setup.profile = c.profile.clone().unwrap_or_default();
        }
        setup.endpoint = c.endpoint.clone().unwrap_or_default().trim_start_matches("https://").to_owned();
        (setup.name, setup.bucket, setup.region, setup.prefix) = (c.id, c.bucket, c.region, c.prefix);
    } else {
        setup.name = setup.default_name().into();
    }
    app.ui.dialog = Some(Dialog::CloudSetup { setup: Box::new(setup) });
    Ok(Value::Null)
}

/// OK on a step that doesn't finish: go on, checking the connection first on step 2.
pub fn next(app: &mut LightcraftApp, setup: &mut Setup) {
    match setup.step {
        Step::Provider => {
            setup.step = Step::Connect;
            if setup.name.trim().is_empty() || ["b2", "s3", "storage"].contains(&setup.name.trim()) {
                setup.name = setup.default_name().into();
            }
        }
        Step::Connect if setup.passed().is_some() => setup.go_on(),
        Step::Connect => start_check(app, setup, true),
        Step::Library => {}
    }
}

/// Check the connection as typed on a worker thread; `advance` goes on when it passes.
pub fn start_check(app: &mut LightcraftApp, setup: &mut Setup, advance: bool) {
    setup.checked = fingerprint(setup);
    setup.advance = false;
    match spawn_check(app, setup) {
        Ok(task) => {
            app.cloud_check = Some(task);
            setup.check = Check::Running;
            setup.advance = advance;
        }
        Err(error) => setup.check = Check::Failed { error },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_check(app: &LightcraftApp, setup: &Setup) -> Result<CheckTask, String> {
    use lightcraft_engine::assets::s3::S3Connection;
    let mut connection: S3Connection = serde_json::from_value(setup.params()?).map_err(|e| e.to_string())?;
    if let Some(keys) = typed_keys(setup)? {
        connection = connection.with_keys(keys);
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = app.session.activity.start("cloudCheck", "Checking cloud storage", lightcraft_engine::activity::Cancel::Flag(cancel.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("cloud-check".into())
        .spawn(move || {
            let _ = tx.send(lightcraft_engine::cloud_setup::probe(&connection, &flag));
        })
        .map_err(|e| e.to_string())?;
    Ok(CheckTask { rx, cancel, fingerprint: fingerprint(setup), _guard: guard })
}

#[cfg(target_arch = "wasm32")]
fn spawn_check(_: &LightcraftApp, _: &Setup) -> Result<CheckTask, String> {
    Err("cloud setup is available in the desktop app".into())
}

/// The keys typed into step 2, if any.
#[cfg(not(target_arch = "wasm32"))]
fn typed_keys(setup: &Setup) -> Result<Option<lightcraft_engine::assets::s3::AccessKeys>, String> {
    if setup.authentication != Authentication::NewKeys {
        return Ok(None);
    }
    let (id, secret) = (setup.key_id.trim(), setup.secret.0.trim());
    match (id.is_empty(), secret.is_empty()) {
        (false, false) => lightcraft_engine::assets::s3::AccessKeys::new(id, secret).map(Some).map_err(|e| e.to_string()),
        _ => Err(crate::i18n::tr("Enter both the key ID and its secret key.").to_string()),
    }
}

/// Cancel the view-owned probe when its dialog or library is discarded.
pub(crate) fn cancel_check(app: &mut LightcraftApp) {
    if let Some(task) = app.cloud_check.take() {
        task.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Every frame: take a finished check's result into the dialog. A check whose dialog closed
/// stops, and one for settings changed since is dropped.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.cloud_check else { return };
    let Some(Dialog::CloudSetup { setup }) = &mut app.ui.dialog else {
        task.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        app.cloud_check = None;
        return;
    };
    let result = match task.rx.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) if task.fingerprint == fingerprint(setup) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            task.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Err(String::new())
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(crate::i18n::tr("The connection check stopped unexpectedly.").to_string()),
    };
    let current = task.fingerprint == fingerprint(setup);
    app.cloud_check = None;
    if !current {
        setup.check = Check::Untested;
        setup.advance = false;
        return;
    }
    let advance = std::mem::take(&mut setup.advance);
    match result {
        Ok(found) => {
            setup.check = Check::Passed { found };
            if advance {
                setup.go_on();
            }
        }
        Err(error) => setup.check = Check::Failed { error },
    }
    ctx.request_repaint();
}

/// The last step's OK: save typed keys to the credentials profile, save the connection, then
/// (Cloud Synchronization…) enable synchronization and start it.
#[cfg(not(target_arch = "wasm32"))]
pub fn finish(app: &mut LightcraftApp, setup: &Setup) -> Result<Value, String> {
    if setup.passed().is_none() {
        return Err(crate::i18n::tr("Test the current connection and authentication before saving.").into());
    }
    let params = setup.params()?;
    lightcraft_engine::cloud_setup::validate_destination(&mut app.session, &params)?;
    if let Some(keys) = typed_keys(setup)? {
        lightcraft_engine::assets::s3::save_profile(params["profile"].as_str().ok_or("new keys have no credential binding")?, &keys)
            .map_err(|e| e.to_string())?;
    }
    let saved = app.run("cloud.configure", params)?;
    if setup.storage_only {
        return Ok(saved);
    }
    let (protocol, library) = setup.library();
    let mut enable = json!({"connection": setup.name.trim(), "protocol": protocol});
    if let Some(library) = library {
        enable["library"] = json!(library);
    }
    app.run("cloud.syncEnable", enable)?;
    app.run("cloud.sync", json!({"wait": false}))
}

#[cfg(target_arch = "wasm32")]
pub fn finish(_: &mut LightcraftApp, _: &Setup) -> Result<Value, String> {
    Err("cloud setup is available in the desktop app".into())
}

/// The dialog's body for the current step.
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, setup: &mut Setup) {
    match setup.step {
        Step::Provider => provider_step(ui, setup),
        Step::Connect => connect_step(app, ui, setup),
        Step::Library => library_step(ui, setup),
    }
}

fn dim(ui: &mut egui::Ui, text: impl Into<String>) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(text.into()).color(t.text_dim));
}

fn provider_step(ui: &mut egui::Ui, setup: &mut Setup) {
    ui.label(crate::i18n::tr("Where do you want to keep your cloud library?"));
    ui.add_space(4.0);
    for (provider, id, title, text) in [
        (Provider::Backblaze, "backblaze", "Backblaze B2", "Low-cost storage. You need a private bucket and an application key."),
        (Provider::Aws, "aws", "Amazon S3", "You need a bucket and an access key from AWS IAM."),
        (Provider::Other, "other", "Other S3-compatible storage", "Wasabi, Cloudflare R2, MinIO and other services with an S3 endpoint."),
    ] {
        let r = ui.radio_value(&mut setup.provider, provider, crate::i18n::tr(title));
        crate::widgets::register(ui.ctx(), format!("radio:cloudSetupProvider:{id}"), r.rect);
        note(ui, crate::i18n::tr(text));
        ui.add_space(2.0);
    }
    ui.add_space(4.0);
    dim(ui, crate::i18n::tr("Photos, edits and albums are kept in a bucket in your own account, billed by the provider."));
    dim(
        ui,
        crate::i18n::tr(
            "Enabling cloud sync uploads all photos added to this library, including photos you add later. Originals already in cloud storage are verified and reused. Your local files are kept.",
        ),
    );
}

/// Text under a choice, lined up with its label.
fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 22, ..Default::default() }).show(ui, |ui| dim(ui, text));
}

/// Labelled text fields lined up in two columns, however long a translated label is.
fn rows(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([10.0, 6.0]).show(ui, add);
}

fn text_row(ui: &mut egui::Ui, label: &str, widget: &str, text: &mut String, hint: &str, secret: bool) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
    let width = ui.available_width().max(160.0);
    crate::text_field::TextField::singleline(widget, text).hint(crate::i18n::tr(hint)).password(secret).width(width).show(ui);
    ui.end_row();
}

fn connect_step(app: &mut LightcraftApp, ui: &mut egui::Ui, setup: &mut Setup) {
    let (heading, help) = match setup.provider {
        Provider::Backblaze => (
            "Connect to Backblaze B2",
            "In Backblaze, create a private bucket and an application key that may read and write it. The bucket's page shows its endpoint.",
        ),
        Provider::Aws => ("Connect to Amazon S3", "In AWS, create a bucket and an IAM access key that may list, read and write it."),
        Provider::Other => ("Connect to S3-compatible storage", "Use the S3 endpoint, bucket and access key your service gives you."),
    };
    ui.strong(crate::i18n::tr(heading));
    dim(ui, crate::i18n::tr(help));
    ui.add_space(4.0);
    let (id_label, secret_label) =
        if setup.provider == Provider::Backblaze { ("Key ID", "Application key") } else { ("Access key ID", "Secret access key") };
    rows(ui, "cloudSetupFields", |ui| {
        text_row(ui, "Bucket", "cloudSetupBucket", &mut setup.bucket, "my-photos", false);
        match setup.provider {
            Provider::Backblaze => text_row(ui, "Endpoint", "cloudSetupEndpoint", &mut setup.endpoint, "s3.us-west-004.backblazeb2.com", false),
            Provider::Aws => text_row(ui, "Region", "cloudSetupRegion", &mut setup.region, "us-east-1", false),
            Provider::Other => {
                text_row(ui, "Endpoint", "cloudSetupEndpoint", &mut setup.endpoint, "https://s3.example.com", false);
                text_row(ui, "Region", "cloudSetupRegion", &mut setup.region, "us-east-1", false);
            }
        }
    });
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Authentication"));
        let response = crate::widgets::dropdown(ui, "cloudSetupAuthentication", crate::i18n::tr(setup.authentication.label()), t.font(12.0), t.text);
        egui::Popup::menu(&response).show(|ui| {
            for choice in [Authentication::SavedKeys, Authentication::NewKeys, Authentication::Profile, Authentication::Environment] {
                if choice == Authentication::SavedKeys && !setup.saved_keys {
                    continue;
                }
                let response = ui.selectable_label(setup.authentication == choice, crate::i18n::tr(choice.label()));
                crate::widgets::register(ui.ctx(), format!("cloudSetupAuthentication:{}", choice.id()), response.rect);
                if response.clicked() {
                    setup.authentication = choice;
                    ui.close();
                }
            }
        });
    });
    match setup.authentication {
        Authentication::NewKeys => {
            rows(ui, "cloudSetupKeys", |ui| {
                text_row(ui, id_label, "cloudSetupKeyId", &mut setup.key_id, "", false);
                text_row(ui, secret_label, "cloudSetupSecret", &mut setup.secret.0, "", true);
            });
            dim(ui, crate::i18n::tr("Keys are saved in this device's credentials file, outside the catalog and cloud."));
        }
        Authentication::Profile => rows(ui, "cloudSetupProfileFields", |ui| {
            text_row(ui, "AWS profile", "cloudSetupProfile", &mut setup.profile, "profile name", false);
        }),
        Authentication::SavedKeys => dim(ui, crate::i18n::tr("Using the keys previously saved for this connection.")),
        Authentication::Environment => dim(ui, crate::i18n::tr("Uses environment credentials or the default AWS profile.")),
    }
    let more = egui::CollapsingHeader::new(crate::i18n::tr("More options")).id_salt("cloudSetupMore").show(ui, |ui| {
        let default_name = setup.default_name();
        rows(ui, "cloudSetupMoreFields", |ui| {
            text_row(ui, "Folder in bucket", "cloudSetupPrefix", &mut setup.prefix, "lightcraft", false);
            text_row(ui, "Connection name", "cloudSetupName", &mut setup.name, default_name, false);
        });
    });
    crate::widgets::register(ui.ctx(), "button:cloudSetupMore", more.header_response.rect);
    ui.add_space(4.0);
    let t = Tokens::get(ui.ctx());
    let current = setup.checked == fingerprint(setup);
    match &setup.check {
        Check::Running if current => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Checking the connection…"));
            });
        }
        Check::Passed { found } if current => {
            let n = found.libraries.len();
            let text = match n {
                0 => crate::i18n::tr("Connected. There is no library in this folder yet.").to_string(),
                1 => crate::i18n::tr("Connected. Found 1 library in this folder.").to_string(),
                n => crate::i18n::tr_format!("Connected. Found {n} libraries in this folder.", n = n),
            };
            let response = ui.label(egui::RichText::new(format!("✓ {text}")).color(t.accent));
            crate::widgets::register(ui.ctx(), "status:cloudSetupConnected", response.rect);
        }
        Check::Failed { error } if current => {
            ui.label(egui::RichText::new(error.as_str()).color(t.caution));
        }
        _ => {
            let r = ui.button(crate::i18n::tr("Test Connection"));
            crate::widgets::register(ui.ctx(), "button:cloudSetupTest", r.rect);
            if r.clicked() {
                start_check(app, setup, false);
            }
        }
    }
}

fn library_step(ui: &mut egui::Ui, setup: &mut Setup) {
    ui.label(crate::i18n::tr_format!("Connected to “{bucket}”. Choose the cloud library for this catalog.", bucket = setup.bucket.trim()));
    ui.add_space(4.0);
    let blocked = setup.new_blocked();
    let found = setup.found().map(|found| found.libraries.clone()).unwrap_or_default();
    ui.add_enabled_ui(!blocked, |ui| {
        let r = ui.radio_value(&mut setup.choice, Choice::New, crate::i18n::tr("Start a new cloud library"));
        crate::widgets::register(ui.ctx(), "radio:cloudSetupNew", r.rect);
    });
    note(
        ui,
        crate::i18n::tr(if blocked {
            "This folder already holds a library. Join it, or go back and choose another folder under More options."
        } else {
            "Uploads this catalog's photos, edits and albums. Other devices join it with its library ID, shown in Info."
        }),
    );
    for library in &found {
        let short = library.id.get(..12).unwrap_or(&library.id);
        let kind = match library.protocol {
            SyncProtocol::AppendLog => crate::i18n::tr("append-only"),
            SyncProtocol::ConditionalHead => crate::i18n::tr("conditional writes"),
        };
        let r = ui.radio_value(
            &mut setup.choice,
            Choice::Found(library.id.clone()),
            crate::i18n::tr_format!("Join library {id}… ({kind})", id = short, kind = kind),
        );
        crate::widgets::register(ui.ctx(), format!("radio:cloudSetupJoin:{}", library.id), r.rect);
    }
    let r = ui.radio_value(&mut setup.choice, Choice::ById, crate::i18n::tr("Join a library by its ID"));
    crate::widgets::register(ui.ctx(), "radio:cloudSetupById", r.rect);
    if setup.choice == Choice::ById {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 22, ..Default::default() }).show(ui, |ui| {
            crate::text_field::TextField::singleline("cloudSetupJoinId", &mut setup.join_id)
                .hint(crate::i18n::tr("Library ID"))
                .width(f32::INFINITY)
                .show(ui);
        });
    }
    ui.add_space(4.0);
    if setup.choice != Choice::New {
        dim(
            ui,
            crate::i18n::tr(
                "Joining merges this catalog's photos, edits and albums with the existing cloud library. Identical original bytes are reused; separate photo identities and conflicting edits are retained.",
            ),
        );
    }
    dim(
        ui,
        crate::i18n::tr_format!(
            "Enabling sync for existing photos and albums, or editing one entity, requires at most {limit} changed fields and 1 MiB per entity. Album metadata and managed memberships share this budget; split large albums before setup.",
            limit = lightcraft_engine::cloud_sync::MAX_ENTITY_CHANGES
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b2() -> Setup {
        Setup {
            step: Step::Connect,
            name: "b2".into(),
            bucket: "my-photos".into(),
            endpoint: "s3.us-west-004.backblazeb2.com".into(),
            prefix: "lightcraft".into(),
            new_profile: NewProfile(Some(format!("lightcraft-key-{}", "ab".repeat(32)))),
            key_id: "0042".into(),
            secret: Secret("K004secret".into()),
            ..Default::default()
        }
    }

    #[test]
    fn independent_setup_forms_never_share_a_name_derived_credential_record() {
        let make = || Setup {
            name: "b2".into(),
            bucket: "my-photos".into(),
            endpoint: "s3.us-west-004.backblazeb2.com".into(),
            prefix: "lightcraft".into(),
            key_id: "KEY".into(),
            secret: Secret("SECRET".into()),
            ..Default::default()
        };
        let a = make();
        let mut b = make();
        b.key_id = "OTHER_KEY".into();
        b.secret = Secret("OTHER_SECRET".into());
        assert_ne!(
            a.params().unwrap()["profile"],
            b.params().unwrap()["profile"],
            "independent libraries must not overwrite each other's credentials"
        );
    }
    #[test]
    fn providers_fill_in_endpoint_region_and_profile() {
        let params = b2().params().unwrap();
        assert_eq!(
            params,
            json!({"id":"b2","bucket":"my-photos","region":"us-west-004","prefix":"lightcraft",
                "endpoint":"https://s3.us-west-004.backblazeb2.com","profile":format!("lightcraft-key-{}", "ab".repeat(32))})
        );
        let aws = Setup {
            provider: Provider::Aws,
            endpoint: String::new(),
            region: "EU-West-1".into(),
            key_id: String::new(),
            secret: Secret::default(),
            profile: "work".into(),
            authentication: Authentication::Profile,
            ..b2()
        };
        assert_eq!(aws.params().unwrap(), json!({"id":"b2","bucket":"my-photos","region":"eu-west-1","prefix":"lightcraft","profile":"work"}));
        let other = Setup { provider: Provider::Other, endpoint: "minio.example.com/".into(), ..b2() };
        assert_eq!(other.params().unwrap()["endpoint"], "https://minio.example.com");
        assert_eq!(other.params().unwrap()["region"], "us-east-1");
        assert!(Setup { endpoint: "example.com".into(), ..b2() }.params().is_err());
        assert!(Setup { name: "my name".into(), ..b2() }.params().is_err());
    }

    #[test]
    fn a_passed_check_is_for_the_settings_it_checked() {
        let mut setup = b2();
        setup.checked = fingerprint(&setup);
        setup.check = Check::Passed { found: Probe::default() };
        assert!(setup.passed().is_some());
        setup.secret.0.push('x');
        assert!(setup.passed().is_none(), "a changed key needs a new check");
        assert!(!format!("{setup:?}").contains("K004secret"));
        assert!(!serde_json::to_string(&setup).unwrap().contains("K004secret"));
    }

    #[test]
    fn selected_authentication_controls_profile_and_ignores_inactive_keys() {
        let mut setup = Setup { saved_keys: true, saved_profile: Some("lightcraft-b2".into()), profile: "replacement".into(), ..b2() };
        setup.authentication = Authentication::Profile;
        // Saved keys and even incomplete leftover replacement keys cannot override a
        // deliberately selected named profile.
        setup.secret = Secret::default();
        assert_eq!(setup.params().unwrap()["profile"], "replacement");
        assert!(typed_keys(&setup).unwrap().is_none());
        setup.authentication = Authentication::Environment;
        assert!(setup.params().unwrap().get("profile").is_none());
        assert!(typed_keys(&setup).unwrap().is_none());
        setup.authentication = Authentication::SavedKeys;
        setup.name = "renamed".into();
        assert_eq!(setup.params().unwrap()["profile"], "lightcraft-b2", "saved binding survives a connection-name edit");
        assert!(typed_keys(&setup).unwrap().is_none());
        setup.authentication = Authentication::NewKeys;
        assert!(setup.params().is_err(), "incomplete new keys cannot fall back to saved keys");
        setup.secret = Secret("replacement-secret".into());
        assert_eq!(setup.params().unwrap()["profile"], format!("lightcraft-key-{}", "ab".repeat(32)));
        assert!(typed_keys(&setup).unwrap().is_some());
    }

    #[test]
    fn authentication_changes_invalidate_probe_and_missing_sources_never_fall_back() {
        let mut setup = Setup { saved_keys: true, profile: "replacement".into(), ..b2() };
        setup.checked = fingerprint(&setup);
        setup.check = Check::Passed { found: Probe::default() };
        setup.authentication = Authentication::Profile;
        assert!(setup.passed().is_none());
        setup.profile.clear();
        assert!(setup.params().is_err());
        setup.authentication = Authentication::SavedKeys;
        setup.saved_keys = false;
        assert!(setup.params().is_err());
        setup.authentication = Authentication::NewKeys;
        setup.key_id.clear();
        setup.secret = Secret::default();
        assert!(setup.params().is_err());
    }

    #[test]
    fn finishing_requires_a_successful_probe_for_the_selected_authentication() {
        let mut session = lightcraft_engine::Session::new();
        session
            .open_library_in(
                lightcraft_engine::library::LibraryStores {
                    dir: "authentication-save-test".into(),
                    catalog: Box::new(lightcraft_catalog::MemStore::new()),
                    files: Box::new(lightcraft_catalog::MemStore::new()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        let mut app = LightcraftApp::new(session, crate::Services::default());
        let mut setup = b2();
        assert!(finish(&mut app, &setup).unwrap_err().contains("Test the current connection"));
        setup.checked = fingerprint(&setup);
        setup.check = Check::Passed { found: Probe::default() };
        setup.authentication = Authentication::Environment;
        assert!(finish(&mut app, &setup).unwrap_err().contains("Test the current connection"));
        assert_eq!(app.run("cloud.connections", json!({})).unwrap()["connections"], json!([]));
    }

    #[test]
    fn independent_new_keys_do_not_alias_and_missing_saved_bindings_never_fall_back() {
        let a = Setup { new_profile: NewProfile::default(), ..b2() };
        let mut b = Setup { new_profile: NewProfile::default(), ..b2() };
        assert_ne!(a.params().unwrap()["profile"], b.params().unwrap()["profile"]);
        let profile = b.params().unwrap()["profile"].clone();
        b.name = "renamed".into();
        assert_eq!(b.params().unwrap()["profile"], profile);
        b.authentication = Authentication::SavedKeys;
        b.saved_keys = true;
        b.saved_profile = None;
        assert!(b.params().is_err());
        b.saved_profile = profile.as_str().map(str::to_owned);
        assert_eq!(b.params().unwrap()["profile"], profile);
        b.authentication = Authentication::NewKeys;
        b.new_profile = NewProfile(None);
        assert!(b.params().is_err());
    }
    #[test]
    fn reopened_managed_connections_keep_their_exact_binding_and_rotation_gets_a_new_one() {
        for profile in ["lightcraft-b2".to_owned(), format!("lightcraft-key-{}", "ab".repeat(32))] {
            let saved = profile.clone();
            let services = crate::Services {
                cloud_action: Some(Box::new(move |_, id, _| {
                    (id=="cloud.connections").then(||Ok(json!({"connections":[{"id":"b2","bucket":"photos","region":"us-west-004","endpoint":"https://s3.us-west-004.backblazeb2.com","prefix":"lightcraft","profile":saved}]})))
                })),
                ..Default::default()
            };
            let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
            open(&mut app, Some("b2"), true).unwrap();
            let Some(Dialog::CloudSetup { setup }) = &mut app.ui.dialog else {
                panic!("setup not open");
            };
            assert_eq!(setup.authentication, Authentication::SavedKeys);
            assert_eq!(setup.params().unwrap()["profile"], profile);
            setup.authentication = Authentication::NewKeys;
            setup.key_id = "ROTATED".into();
            setup.secret = Secret("rotation-secret".into());
            assert_ne!(setup.params().unwrap()["profile"], profile);
        }
    }

    #[test]
    fn the_library_step_follows_what_the_check_found() {
        use lightcraft_engine::cloud_setup::FoundLibrary;
        let mut setup = Setup { provider: Provider::Aws, ..b2() };
        setup.checked = fingerprint(&setup);
        let head = FoundLibrary { id: "h".repeat(64), protocol: SyncProtocol::ConditionalHead };
        setup.check = Check::Passed { found: Probe { libraries: vec![head.clone()], head_taken: true } };
        setup.go_on();
        assert_eq!((setup.step, &setup.choice), (Step::Library, &Choice::Found(head.id.clone())));
        assert_eq!(setup.library(), (SyncProtocol::ConditionalHead, Some(head.id.clone())));
        setup.choice = Choice::New;
        assert!(!setup.can_confirm(), "one conditional-head library per folder");
        let mut b2 = b2();
        b2.checked = fingerprint(&b2);
        b2.check = Check::Passed { found: Probe::default() };
        b2.go_on();
        assert_eq!(b2.choice, Choice::New);
        assert_eq!(b2.library(), (SyncProtocol::AppendLog, None));
        b2.storage_only = true;
        b2.step = Step::Connect;
        assert!(b2.finishes());
    }
}
