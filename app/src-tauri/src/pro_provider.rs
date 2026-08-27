//! Secure provider configuration for the built-in Pro path.
//!
//! This module owns the complete API-key boundary. Commands accept only a
//! fixed provider and disclosure version. Key bytes enter through AppKit,
//! remain in native Rust, are checked against fixed TLS endpoints, and are
//! stored in an app-only Keychain service only after authentication succeeds.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thought_credentials::{CredentialError, ProviderCredentialStore};
use zeroize::Zeroizing;

pub const DISCLOSURE_VERSION: u32 = 1;
const SETTINGS_VERSION: u32 = 1;
const MAX_KEY_BYTES: usize = 4096;
const MAX_SETTINGS_BYTES: usize = 128 * 1024;
const MAX_MODEL_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ERROR_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_MODEL_COUNT: usize = 10_000;
const MAX_MODEL_ID_BYTES: usize = 512;
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(12);
static SETTINGS_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Openai,
    Anthropic,
}

impl Provider {
    const ALL: [Self; 2] = [Self::Openai, Self::Anthropic];

    fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }

    fn models_url(self) -> &'static str {
        match self {
            Self::Openai => "https://api.openai.com/v1/models",
            Self::Anthropic => "https://api.anthropic.com/v1/models?limit=100",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    NotChecked,
    ModelAccessChecked,
    InvalidKeyFormat,
    CredentialOrAccessInvalid,
    PermissionDenied,
    BillingUnavailable,
    SpendOrUsageLimit,
    RateLimited,
    UnsupportedRegion,
    ProviderUnavailable,
    Timeout,
    NetworkOrTlsFailure,
    InvalidProviderResponse,
    ModelUnavailable,
    CredentialMissing,
}

impl ValidationStatus {
    fn authenticated(self) -> bool {
        matches!(self, Self::ModelAccessChecked | Self::ModelUnavailable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderConfiguration {
    provider: Provider,
    configured: bool,
    removal_pending: bool,
    validation_status: ValidationStatus,
    last_checked_at: Option<i64>,
    last_validated_at: Option<i64>,
    model_count: Option<usize>,
    request_id: Option<String>,
    disclosure_version: Option<u32>,
    charges_acknowledged_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderActionOutcome {
    Saved,
    Cancelled,
    ValidationFailed,
    Checked,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderActionResult {
    outcome: ProviderActionOutcome,
    configuration: ProviderConfiguration,
    attempt_status: Option<ValidationStatus>,
    request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationAttempt {
    status: ValidationStatus,
    model_count: Option<usize>,
    request_id: Option<String>,
}

impl ValidationAttempt {
    fn failed(status: ValidationStatus, request_id: Option<String>) -> Self {
        Self {
            status,
            model_count: None,
            request_id,
        }
    }
}

trait ProviderValidator: Send + Sync {
    fn validate(&self, provider: Provider, key: &[u8]) -> ValidationAttempt;
}

#[derive(Debug, Default)]
struct HttpProviderValidator;

impl ProviderValidator for HttpProviderValidator {
    fn validate(&self, provider: Provider, key: &[u8]) -> ValidationAttempt {
        validate_provider_at(provider, key, provider.models_url(), true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderRecord {
    provider: Provider,
    validation_status: ValidationStatus,
    last_checked_at: i64,
    last_validated_at: Option<i64>,
    model_count: Option<usize>,
    request_id: Option<String>,
    disclosure_version: Option<u32>,
    charges_acknowledged_at: Option<i64>,
    #[serde(default)]
    removal_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderSettings {
    version: u32,
    providers: Vec<ProviderRecord>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            providers: Vec::new(),
        }
    }
}

impl ProviderSettings {
    fn record(&self, provider: Provider) -> Option<&ProviderRecord> {
        self.providers
            .iter()
            .find(|record| record.provider == provider)
    }

    fn upsert(&mut self, record: ProviderRecord) {
        self.providers
            .retain(|candidate| candidate.provider != record.provider);
        self.providers.push(record);
        self.providers.sort_by_key(|candidate| candidate.provider);
    }

    fn remove(&mut self, provider: Provider) {
        self.providers
            .retain(|candidate| candidate.provider != provider);
    }
}

struct ProviderStateInner {
    credentials: Arc<dyn ProviderCredentials>,
    settings_path: PathBuf,
    validator: Arc<dyn ProviderValidator>,
    state_lock: Mutex<()>,
    operation_busy: AtomicBool,
}

trait ProviderCredentials: Send + Sync {
    fn set(&self, provider_id: &str, credential: &[u8]) -> Result<(), CredentialError>;
    fn get(&self, provider_id: &str) -> Result<Zeroizing<Vec<u8>>, CredentialError>;
    fn contains(&self, provider_id: &str) -> Result<bool, CredentialError>;
    fn delete(&self, provider_id: &str) -> Result<(), CredentialError>;
}

impl ProviderCredentials for ProviderCredentialStore {
    fn set(&self, provider_id: &str, credential: &[u8]) -> Result<(), CredentialError> {
        ProviderCredentialStore::set(self, provider_id, credential)
    }

    fn get(&self, provider_id: &str) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
        ProviderCredentialStore::get(self, provider_id)
    }

    fn contains(&self, provider_id: &str) -> Result<bool, CredentialError> {
        ProviderCredentialStore::contains(self, provider_id)
    }

    fn delete(&self, provider_id: &str) -> Result<(), CredentialError> {
        ProviderCredentialStore::delete(self, provider_id)
    }
}

enum PreviousProviderCredential {
    Absent,
    Readable(Zeroizing<Vec<u8>>),
    Inaccessible,
}

#[derive(Clone)]
pub struct ProviderState(Arc<ProviderStateInner>);

impl ProviderState {
    pub fn platform(application_home: impl AsRef<Path>) -> Self {
        let application_home = application_home.as_ref();
        Self::new(
            ProviderCredentialStore::platform(application_home),
            application_home.join("pro-provider-settings-v1.json"),
            Arc::new(HttpProviderValidator),
        )
    }

    fn new(
        credentials: ProviderCredentialStore,
        settings_path: PathBuf,
        validator: Arc<dyn ProviderValidator>,
    ) -> Self {
        Self::new_with_credentials(Arc::new(credentials), settings_path, validator)
    }

    fn new_with_credentials(
        credentials: Arc<dyn ProviderCredentials>,
        settings_path: PathBuf,
        validator: Arc<dyn ProviderValidator>,
    ) -> Self {
        Self(Arc::new(ProviderStateInner {
            credentials,
            settings_path,
            validator,
            state_lock: Mutex::new(()),
            operation_busy: AtomicBool::new(false),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.0
            .state_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin_operation(&self) -> Result<ProviderOperationGuard, String> {
        self.0
            .operation_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Another API key action is already open.".to_string())?;
        Ok(ProviderOperationGuard(self.0.clone()))
    }

    fn configurations(&self) -> Result<Vec<ProviderConfiguration>, String> {
        let _lock = self.lock();
        let settings = load_settings(&self.0.settings_path)?;
        Provider::ALL
            .into_iter()
            .map(|provider| self.configuration_locked(provider, &settings))
            .collect()
    }

    #[cfg(test)]
    fn configuration(&self, provider: Provider) -> Result<ProviderConfiguration, String> {
        let _lock = self.lock();
        let settings = load_settings(&self.0.settings_path)?;
        self.configuration_locked(provider, &settings)
    }

    fn unchanged_configuration(&self, provider: Provider) -> Result<ProviderConfiguration, String> {
        let _lock = self.lock();
        let settings = load_settings(&self.0.settings_path)?;
        self.unchanged_configuration_locked(provider, &settings)
    }

    fn prompt_is_replacement(&self, provider: Provider) -> Result<bool, String> {
        let _lock = self.lock();
        let settings = load_settings(&self.0.settings_path)?;
        Ok(settings.record(provider).is_some_and(|record| {
            record.disclosure_version == Some(DISCLOSURE_VERSION)
                && record.charges_acknowledged_at.is_some()
                && !record.removal_pending
        }))
    }

    fn configuration_locked(
        &self,
        provider: Provider,
        settings: &ProviderSettings,
    ) -> Result<ProviderConfiguration, String> {
        let credential_present = self
            .0
            .credentials
            .contains(provider.id())
            .map_err(credential_store_error)?;
        Ok(Self::configuration_with_presence(
            provider,
            settings,
            credential_present,
        ))
    }

    fn unchanged_configuration_locked(
        &self,
        provider: Provider,
        settings: &ProviderSettings,
    ) -> Result<ProviderConfiguration, String> {
        let credential_present = match self.0.credentials.contains(provider.id()) {
            Ok(present) => present,
            // Cancel and validation failure do not change the existing item.
            // Preserve its prior metadata in the action result even when the
            // current build cannot read it silently.
            Err(CredentialError::InteractionRequired) => true,
            Err(error) => return Err(credential_store_error(error)),
        };
        Ok(Self::configuration_with_presence(
            provider,
            settings,
            credential_present,
        ))
    }

    fn configuration_with_presence(
        provider: Provider,
        settings: &ProviderSettings,
        credential_present: bool,
    ) -> ProviderConfiguration {
        let record = settings.record(provider);
        // A Keychain item without our acknowledged metadata may be an orphan
        // from an interrupted save or a same-user preseed. Do not present it as
        // configured until the native setup flow succeeds.
        let disclosure_acknowledged = record.is_some_and(|record| {
            record.disclosure_version == Some(DISCLOSURE_VERSION)
                && record.charges_acknowledged_at.is_some()
                && !record.removal_pending
        });
        let configured = credential_present && disclosure_acknowledged;
        let validation_status = if configured {
            record
                .map(|record| record.validation_status)
                .unwrap_or(ValidationStatus::NotChecked)
        } else if disclosure_acknowledged && !credential_present {
            ValidationStatus::CredentialMissing
        } else {
            ValidationStatus::NotChecked
        };
        ProviderConfiguration {
            provider,
            configured,
            removal_pending: record.is_some_and(|record| record.removal_pending),
            validation_status,
            last_checked_at: record.map(|record| record.last_checked_at),
            last_validated_at: record.and_then(|record| record.last_validated_at),
            model_count: record.and_then(|record| record.model_count),
            request_id: record.and_then(|record| record.request_id.clone()),
            disclosure_version: record.and_then(|record| record.disclosure_version),
            charges_acknowledged_at: record.and_then(|record| record.charges_acknowledged_at),
        }
    }

    fn configure(
        &self,
        provider: Provider,
        key: Zeroizing<Vec<u8>>,
        disclosure_version: u32,
    ) -> Result<ProviderActionResult, String> {
        if disclosure_version != DISCLOSURE_VERSION {
            return Err("Review the current API cost and privacy disclosure first.".to_string());
        }
        let _lock = self.lock();
        let attempt = self.0.validator.validate(provider, &key);
        if !attempt.status.authenticated() {
            let settings = load_settings(&self.0.settings_path)?;
            return Ok(ProviderActionResult {
                outcome: ProviderActionOutcome::ValidationFailed,
                configuration: self.unchanged_configuration_locked(provider, &settings)?,
                attempt_status: Some(attempt.status),
                request_id: attempt.request_id,
            });
        }

        let mut settings = load_settings(&self.0.settings_path)?;
        let had_previous = match self.0.credentials.contains(provider.id()) {
            Ok(present) => present,
            // InteractionRequired means Security.framework found a matching
            // item that cannot be used silently. Treat it as an existing item
            // so the explicit Replace flow can repair its access policy.
            Err(CredentialError::InteractionRequired) => true,
            Err(error) => return Err(credential_store_error(error)),
        };
        let previous = if had_previous {
            match self.0.credentials.get(provider.id()) {
                Ok(previous) => PreviousProviderCredential::Readable(previous),
                Err(CredentialError::InteractionRequired) => {
                    PreviousProviderCredential::Inaccessible
                }
                Err(error) => return Err(credential_store_error(error)),
            }
        } else {
            PreviousProviderCredential::Absent
        };
        if matches!(&previous, PreviousProviderCredential::Inaccessible) {
            // The existing bytes cannot be snapshotted for rollback. Persist a
            // conservative state first so a crash or later metadata failure
            // can leave only an unconfigured orphan, never acknowledged
            // metadata pointing at an inaccessible or partially replaced item.
            settings.remove(provider);
            save_settings(&self.0.settings_path, &settings)?;
        }
        if let Err(error) = self.0.credentials.set(provider.id(), &key) {
            let message = credential_store_error(error);
            if matches!(&previous, PreviousProviderCredential::Inaccessible) {
                return Err(format!(
                    "{message} The prior inaccessible key remains unconfigured; try Replace again."
                ));
            }
            return Err(message);
        }

        let now = epoch_seconds();
        settings.upsert(ProviderRecord {
            provider,
            validation_status: attempt.status,
            last_checked_at: now,
            last_validated_at: Some(now),
            model_count: attempt.model_count,
            request_id: attempt.request_id.clone(),
            disclosure_version: Some(disclosure_version),
            charges_acknowledged_at: Some(now),
            removal_pending: false,
        });
        if let Err(error) = save_settings(&self.0.settings_path, &settings) {
            let rollback = match previous {
                PreviousProviderCredential::Readable(previous) => {
                    self.0.credentials.set(provider.id(), &previous)
                }
                PreviousProviderCredential::Absent => self.0.credentials.delete(provider.id()),
                PreviousProviderCredential::Inaccessible => {
                    return Err(
                        "The API key was saved securely, but its local settings could not be saved. It remains disabled; replace it again."
                            .to_string(),
                    );
                }
            };
            if rollback.is_err() {
                return Err(
                    "Provider settings could not be saved, and the earlier Keychain value could not be restored. Remove and add the key again."
                        .to_string(),
                );
            }
            return Err(error);
        }

        Ok(ProviderActionResult {
            outcome: ProviderActionOutcome::Saved,
            configuration: self.configuration_locked(provider, &settings)?,
            attempt_status: Some(attempt.status),
            request_id: attempt.request_id,
        })
    }

    fn revalidate(&self, provider: Provider) -> Result<ProviderActionResult, String> {
        let _lock = self.lock();
        let current_settings = load_settings(&self.0.settings_path)?;
        if current_settings
            .record(provider)
            .is_some_and(|record| record.removal_pending)
        {
            return Ok(ProviderActionResult {
                outcome: ProviderActionOutcome::Checked,
                configuration: self.configuration_locked(provider, &current_settings)?,
                attempt_status: Some(ValidationStatus::CredentialMissing),
                request_id: None,
            });
        }
        if !self
            .0
            .credentials
            .contains(provider.id())
            .map_err(credential_store_error)?
        {
            let settings = load_settings(&self.0.settings_path)?;
            return Ok(ProviderActionResult {
                outcome: ProviderActionOutcome::Checked,
                configuration: self.configuration_locked(provider, &settings)?,
                attempt_status: Some(ValidationStatus::CredentialMissing),
                request_id: None,
            });
        }
        let key = self
            .0
            .credentials
            .get(provider.id())
            .map_err(credential_store_error)?;
        let attempt = self.0.validator.validate(provider, &key);
        let mut settings = load_settings(&self.0.settings_path)?;
        let now = epoch_seconds();
        let previous = settings.record(provider).cloned();
        settings.upsert(ProviderRecord {
            provider,
            validation_status: attempt.status,
            last_checked_at: now,
            last_validated_at: if attempt.status.authenticated() {
                Some(now)
            } else {
                previous
                    .as_ref()
                    .and_then(|record| record.last_validated_at)
            },
            model_count: if attempt.status.authenticated() {
                attempt.model_count
            } else {
                previous.as_ref().and_then(|record| record.model_count)
            },
            request_id: attempt.request_id.clone(),
            disclosure_version: previous
                .as_ref()
                .and_then(|record| record.disclosure_version),
            charges_acknowledged_at: previous
                .as_ref()
                .and_then(|record| record.charges_acknowledged_at),
            removal_pending: false,
        });
        save_settings(&self.0.settings_path, &settings)?;
        Ok(ProviderActionResult {
            outcome: ProviderActionOutcome::Checked,
            configuration: self.configuration_locked(provider, &settings)?,
            attempt_status: Some(attempt.status),
            request_id: attempt.request_id,
        })
    }

    fn remove(&self, provider: Provider) -> Result<ProviderActionResult, String> {
        let _lock = self.lock();
        let mut settings = load_settings(&self.0.settings_path)?;
        let now = epoch_seconds();
        settings.upsert(ProviderRecord {
            provider,
            validation_status: ValidationStatus::NotChecked,
            last_checked_at: now,
            last_validated_at: None,
            model_count: None,
            request_id: None,
            disclosure_version: None,
            charges_acknowledged_at: None,
            removal_pending: true,
        });
        save_settings(&self.0.settings_path, &settings)?;
        // Persist the conservative state first. A crash can now leave only an
        // unacknowledged orphan key, never acknowledged metadata pointing at a
        // missing or same-user-preseeded Keychain item.
        self.0
            .credentials
            .delete(provider.id())
            .map_err(credential_store_error)?;
        settings.remove(provider);
        save_settings(&self.0.settings_path, &settings)?;
        Ok(ProviderActionResult {
            outcome: ProviderActionOutcome::Removed,
            configuration: self.configuration_locked(provider, &settings)?,
            attempt_status: None,
            request_id: None,
        })
    }
}

struct ProviderOperationGuard(Arc<ProviderStateInner>);

impl Drop for ProviderOperationGuard {
    fn drop(&mut self) {
        self.0.operation_busy.store(false, Ordering::Release);
    }
}

#[tauri::command]
pub async fn provider_configurations(
    state: tauri::State<'_, ProviderState>,
) -> Result<Vec<ProviderConfiguration>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.configurations())
        .await
        .map_err(|_| "Provider settings stopped unexpectedly.".to_string())?
}

#[tauri::command]
pub async fn configure_provider_key(
    app: tauri::AppHandle,
    state: tauri::State<'_, ProviderState>,
    provider: Provider,
    disclosure_version: u32,
) -> Result<ProviderActionResult, String> {
    if disclosure_version != DISCLOSURE_VERSION {
        return Err("Review the current API cost and privacy disclosure first.".to_string());
    }
    let state = state.inner().clone();
    let _operation = state.begin_operation()?;
    let status_state = state.clone();
    let replacing =
        tauri::async_runtime::spawn_blocking(move || status_state.prompt_is_replacement(provider))
            .await
            .map_err(|_| "Provider settings stopped unexpectedly.".to_string())??;

    let key = native_prompt(app, provider, replacing).await?;
    let Some(key) = key else {
        let status_state = state.clone();
        let configuration = tauri::async_runtime::spawn_blocking(move || {
            status_state.unchanged_configuration(provider)
        })
        .await
        .map_err(|_| "Provider settings stopped unexpectedly.".to_string())??;
        return Ok(ProviderActionResult {
            outcome: ProviderActionOutcome::Cancelled,
            configuration,
            attempt_status: None,
            request_id: None,
        });
    };
    tauri::async_runtime::spawn_blocking(move || state.configure(provider, key, disclosure_version))
        .await
        .map_err(|_| "Provider validation stopped unexpectedly.".to_string())?
}

#[tauri::command]
pub async fn revalidate_provider_key(
    state: tauri::State<'_, ProviderState>,
    provider: Provider,
) -> Result<ProviderActionResult, String> {
    let state = state.inner().clone();
    let _operation = state.begin_operation()?;
    tauri::async_runtime::spawn_blocking(move || state.revalidate(provider))
        .await
        .map_err(|_| "Provider validation stopped unexpectedly.".to_string())?
}

#[tauri::command]
pub async fn remove_provider_key(
    state: tauri::State<'_, ProviderState>,
    provider: Provider,
) -> Result<ProviderActionResult, String> {
    let state = state.inner().clone();
    let _operation = state.begin_operation()?;
    tauri::async_runtime::spawn_blocking(move || state.remove(provider))
        .await
        .map_err(|_| "Provider removal stopped unexpectedly.".to_string())?
}

#[cfg(target_os = "macos")]
async fn native_prompt(
    app: tauri::AppHandle,
    provider: Provider,
    replacing: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    crate::macos_secure_input::prompt(app, provider.display_name(), replacing).await
}

#[cfg(not(target_os = "macos"))]
async fn native_prompt(
    _: tauri::AppHandle,
    _: Provider,
    _: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    Err("Secure API key entry is currently available in the macOS app.".to_string())
}

fn validate_provider_at(
    provider: Provider,
    key: &[u8],
    endpoint: &str,
    enforce_https: bool,
) -> ValidationAttempt {
    if key.len() < 8
        || key.len() > MAX_KEY_BYTES
        || !key.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return ValidationAttempt::failed(ValidationStatus::InvalidKeyFormat, None);
    }
    let Ok(key) = std::str::from_utf8(key) else {
        return ValidationAttempt::failed(ValidationStatus::InvalidKeyFormat, None);
    };

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(VALIDATION_TIMEOUT))
        .https_only(enforce_https)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .build()
        .into();
    let mut request = agent.get(endpoint).header("Accept", "application/json");
    match provider {
        Provider::Openai => {
            let mut value = Zeroizing::new(String::with_capacity("Bearer ".len() + key.len()));
            value.push_str("Bearer ");
            value.push_str(key);
            request = request
                .header("Authorization", value.as_str())
                .header("X-Client-Request-Id", &client_request_id());
        }
        Provider::Anthropic => {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        }
    }

    let mut response = match request.call() {
        Ok(response) => response,
        Err(error) => return ValidationAttempt::failed(classify_transport_error(error), None),
    };
    let request_id = response_request_id(provider, &response, key.as_bytes());
    let retry_after = bounded_header_present(&response, "retry-after");
    let status = response.status().as_u16();
    if status != 200 {
        let body = read_bounded_body(&mut response, MAX_ERROR_RESPONSE_BYTES).unwrap_or_default();
        return ValidationAttempt::failed(
            classify_provider_error(provider, status, &body, retry_after),
            request_id,
        );
    }
    let body = match read_bounded_body(&mut response, MAX_MODEL_RESPONSE_BYTES) {
        Ok(body) => body,
        Err(_) => {
            return ValidationAttempt::failed(
                ValidationStatus::InvalidProviderResponse,
                request_id,
            );
        }
    };
    #[derive(Deserialize)]
    struct ModelCatalog {
        data: Vec<Model>,
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    let Ok(catalog) = serde_json::from_slice::<ModelCatalog>(&body) else {
        return ValidationAttempt::failed(ValidationStatus::InvalidProviderResponse, request_id);
    };
    if catalog.data.len() > MAX_MODEL_COUNT
        || catalog
            .data
            .iter()
            .any(|model| model.id.is_empty() || model.id.len() > MAX_MODEL_ID_BYTES)
    {
        return ValidationAttempt::failed(ValidationStatus::InvalidProviderResponse, request_id);
    }
    let model_count = catalog.data.len();
    ValidationAttempt {
        status: if model_count == 0 {
            ValidationStatus::ModelUnavailable
        } else {
            ValidationStatus::ModelAccessChecked
        },
        model_count: Some(model_count),
        request_id,
    }
}

fn read_bounded_body(
    response: &mut ureq::http::Response<ureq::Body>,
    limit: u64,
) -> io::Result<Vec<u8>> {
    // Wrap the fully decoded reader. `ureq`'s own body limit sits before its
    // gzip decoder, which does not bound the bytes ultimately allocated.
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit.saturating_add(1))
        .read_to_end(&mut body)?;
    if body.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "provider response exceeds the native size limit",
        ));
    }
    Ok(body)
}

fn classify_provider_error(
    provider: Provider,
    status: u16,
    body: &[u8],
    retry_after: bool,
) -> ValidationStatus {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: Option<ErrorDetail>,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        code: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        message: Option<String>,
        details: Option<ErrorDetails>,
    }
    #[derive(Deserialize)]
    struct ErrorDetails {
        error_code: Option<String>,
    }

    let detail = serde_json::from_slice::<ErrorEnvelope>(body)
        .ok()
        .and_then(|envelope| envelope.error);
    let code = detail.as_ref().and_then(|detail| detail.code.as_deref());
    let kind = detail.as_ref().and_then(|detail| detail.kind.as_deref());
    let message = detail.as_ref().and_then(|detail| detail.message.as_deref());
    let detail_code = detail
        .as_ref()
        .and_then(|detail| detail.details.as_ref())
        .and_then(|details| details.error_code.as_deref());
    if provider == Provider::Openai {
        if matches!(
            code,
            Some(
                "insufficient_quota"
                    | "billing_hard_limit_reached"
                    | "usage_limit_reached"
                    | "exceeded_quota"
                    | "credit_balance_exhausted"
                    | "organization_spend_limit_exceeded"
                    | "project_spend_limit_exceeded"
                    | "organization_usage_limit_exceeded"
            )
        ) || matches!(kind, Some("insufficient_quota"))
        {
            return ValidationStatus::SpendOrUsageLimit;
        }
        if matches!(code, Some("billing_not_active")) || matches!(kind, Some("billing_not_active"))
        {
            return ValidationStatus::BillingUnavailable;
        }
        if matches!(
            code,
            Some("unsupported_country_region_territory" | "unsupported_region")
        ) || matches!(
            kind,
            Some("unsupported_country_region_territory" | "unsupported_region")
        ) {
            return ValidationStatus::UnsupportedRegion;
        }
    } else {
        if matches!(kind, Some("billing_error")) {
            return ValidationStatus::BillingUnavailable;
        }
        if matches!(detail_code, Some("enforced_spend_limit_reached"))
            || (status == 400
                && matches!(kind, Some("invalid_request_error"))
                && message.is_some_and(|message| {
                    message.starts_with("You have reached your specified API usage limits")
                        || message.starts_with(
                            "You have reached your specified workspace API usage limits",
                        )
                }))
        {
            return ValidationStatus::SpendOrUsageLimit;
        }
        if status == 429 && !retry_after {
            return ValidationStatus::InvalidProviderResponse;
        }
    }

    match status {
        401 => ValidationStatus::CredentialOrAccessInvalid,
        402 => ValidationStatus::BillingUnavailable,
        403 => ValidationStatus::PermissionDenied,
        429 => ValidationStatus::RateLimited,
        500..=599 => ValidationStatus::ProviderUnavailable,
        _ => ValidationStatus::InvalidProviderResponse,
    }
}

fn classify_transport_error(error: ureq::Error) -> ValidationStatus {
    match error {
        ureq::Error::Timeout(_) => ValidationStatus::Timeout,
        ureq::Error::HostNotFound
        | ureq::Error::Io(_)
        | ureq::Error::ConnectionFailed
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => ValidationStatus::NetworkOrTlsFailure,
        _ => ValidationStatus::InvalidProviderResponse,
    }
}

fn response_request_id(
    provider: Provider,
    response: &ureq::http::Response<ureq::Body>,
    secret: &[u8],
) -> Option<String> {
    let header = match provider {
        Provider::Openai => "x-request-id",
        Provider::Anthropic => "request-id",
    };
    response
        .headers()
        .get(header)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !secret.is_empty()
                && !value.is_empty()
                && value.len() <= 256
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
                && !value
                    .as_bytes()
                    .windows(secret.len())
                    .any(|window| window == secret)
        })
        .map(ToOwned::to_owned)
}

fn bounded_header_present(response: &ureq::http::Response<ureq::Body>, name: &str) -> bool {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        })
}

fn client_request_id() -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "pot-provider-check-{}-{nanos}-{counter}",
        std::process::id()
    )
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn credential_store_error(error: CredentialError) -> String {
    match error {
        CredentialError::Missing => "The saved API key is missing from Keychain.".to_string(),
        CredentialError::InteractionRequired => {
            "Keychain access needs attention. Open Settings and replace this API key for the current app."
                .to_string()
        }
        CredentialError::InvalidStoredCredential => {
            "The saved API key could not be read safely.".to_string()
        }
        _ => "Proof of Thought could not use secure API key storage.".to_string(),
    }
}

fn load_settings(path: &Path) -> Result<ProviderSettings, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ProviderSettings::default());
        }
        Err(_) => return Err("Provider settings could not be read.".to_string()),
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SETTINGS_BYTES as u64
    {
        return Err("Provider settings are not a safe local file.".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Provider settings have unsafe file permissions.".to_string());
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .map_err(|_| "Provider settings could not be opened.".to_string())?
        .take((MAX_SETTINGS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Provider settings could not be read.".to_string())?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err("Provider settings are too large.".to_string());
    }
    let settings: ProviderSettings =
        serde_json::from_slice(&bytes).map_err(|_| "Provider settings are damaged.".to_string())?;
    if settings.version != SETTINGS_VERSION
        || settings.providers.len() > Provider::ALL.len()
        || settings
            .providers
            .iter()
            .enumerate()
            .any(|(index, record)| {
                settings.providers[..index]
                    .iter()
                    .any(|earlier| earlier.provider == record.provider)
            })
    {
        return Err("Provider settings use an unsupported format.".to_string());
    }
    Ok(settings)
}

fn save_settings(path: &Path, settings: &ProviderSettings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Provider settings have no local directory.".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|_| "Provider settings directory could not be created.".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Provider settings directory could not be protected.".to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|_| "Provider settings could not be encoded.".to_string())?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err("Provider settings are too large.".to_string());
    }
    let name = path
        .file_name()
        .ok_or_else(|| "Provider settings have no file name.".to_string())?;
    let counter = SETTINGS_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        counter
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok::<(), io::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|_| "Provider settings could not be saved.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    struct StubValidator(Mutex<VecDeque<ValidationAttempt>>);

    impl StubValidator {
        fn new(attempts: impl IntoIterator<Item = ValidationAttempt>) -> Self {
            Self(Mutex::new(attempts.into_iter().collect()))
        }
    }

    impl ProviderValidator for StubValidator {
        fn validate(&self, _: Provider, _: &[u8]) -> ValidationAttempt {
            self.0.lock().unwrap().pop_front().unwrap()
        }
    }

    #[derive(Default)]
    struct StubCredentials {
        credential: Mutex<Option<Vec<u8>>>,
        deny_reads: AtomicBool,
        fail_next_set: AtomicBool,
        after_next_set: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl StubCredentials {
        fn deny_reads(&self) {
            self.deny_reads.store(true, Ordering::Release);
        }

        fn current(&self) -> Option<Vec<u8>> {
            self.credential.lock().unwrap().clone()
        }

        fn fail_next_set(&self) {
            self.fail_next_set.store(true, Ordering::Release);
        }

        fn after_next_set(&self, callback: impl FnOnce() + Send + 'static) {
            *self.after_next_set.lock().unwrap() = Some(Box::new(callback));
        }
    }

    impl ProviderCredentials for StubCredentials {
        fn set(&self, _: &str, credential: &[u8]) -> Result<(), CredentialError> {
            if self.fail_next_set.swap(false, Ordering::AcqRel) {
                return Err(CredentialError::Platform(
                    "simulated Keychain replacement failure".into(),
                ));
            }
            *self.credential.lock().unwrap() = Some(credential.to_vec());
            self.deny_reads.store(false, Ordering::Release);
            if let Some(callback) = self.after_next_set.lock().unwrap().take() {
                callback();
            }
            Ok(())
        }

        fn get(&self, _: &str) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
            if self.deny_reads.load(Ordering::Acquire) {
                return Err(CredentialError::InteractionRequired);
            }
            self.credential
                .lock()
                .unwrap()
                .clone()
                .map(Zeroizing::new)
                .ok_or(CredentialError::Missing)
        }

        fn contains(&self, _: &str) -> Result<bool, CredentialError> {
            if self.deny_reads.load(Ordering::Acquire) {
                return Err(CredentialError::InteractionRequired);
            }
            Ok(self.credential.lock().unwrap().is_some())
        }

        fn delete(&self, _: &str) -> Result<(), CredentialError> {
            *self.credential.lock().unwrap() = None;
            Ok(())
        }
    }

    fn valid_attempt() -> ValidationAttempt {
        ValidationAttempt {
            status: ValidationStatus::ModelAccessChecked,
            model_count: Some(3),
            request_id: Some("request-123".into()),
        }
    }

    fn test_state(
        directory: &tempfile::TempDir,
        attempts: impl IntoIterator<Item = ValidationAttempt>,
    ) -> ProviderState {
        ProviderState::new(
            ProviderCredentialStore::files(directory.path().join("keys")),
            directory.path().join("settings.json"),
            Arc::new(StubValidator::new(attempts)),
        )
    }

    #[test]
    fn keychain_interaction_error_directs_people_to_explicit_replacement() {
        assert_eq!(
            credential_store_error(CredentialError::InteractionRequired),
            "Keychain access needs attention. Open Settings and replace this API key for the current app."
        );
    }

    #[test]
    fn explicit_replacement_repairs_an_inaccessible_key() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(StubCredentials::default());
        let state = ProviderState::new_with_credentials(
            credentials.clone(),
            directory.path().join("settings.json"),
            Arc::new(StubValidator::new([valid_attempt(), valid_attempt()])),
        );

        state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"original-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();
        credentials.deny_reads();
        assert!(state.prompt_is_replacement(Provider::Anthropic).unwrap());

        let repaired = state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"replacement-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        assert_eq!(repaired.outcome, ProviderActionOutcome::Saved);
        assert!(repaired.configuration.configured);
        assert_eq!(
            credentials.current().as_deref(),
            Some(b"replacement-provider-secret".as_slice())
        );
    }

    #[test]
    fn cancelled_or_invalid_repair_can_preserve_inaccessible_key_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(StubCredentials::default());
        let state = ProviderState::new_with_credentials(
            credentials.clone(),
            directory.path().join("settings.json"),
            Arc::new(StubValidator::new([
                valid_attempt(),
                ValidationAttempt::failed(ValidationStatus::InvalidKeyFormat, None),
            ])),
        );
        state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"original-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();
        credentials.deny_reads();

        let cancelled_snapshot = state.unchanged_configuration(Provider::Openai).unwrap();
        assert!(cancelled_snapshot.configured);
        let invalid = state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"invalid-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        assert_eq!(invalid.outcome, ProviderActionOutcome::ValidationFailed);
        assert!(invalid.configuration.configured);
        assert_eq!(
            invalid.attempt_status,
            Some(ValidationStatus::InvalidKeyFormat)
        );
        assert_eq!(
            credentials.current().as_deref(),
            Some(b"original-provider-secret".as_slice())
        );
    }

    #[test]
    fn failed_inaccessible_replacement_leaves_the_old_key_unconfigured() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(StubCredentials::default());
        let state = ProviderState::new_with_credentials(
            credentials.clone(),
            directory.path().join("settings.json"),
            Arc::new(StubValidator::new([valid_attempt(), valid_attempt()])),
        );
        state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"original-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();
        credentials.deny_reads();
        credentials.fail_next_set();

        let error = state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"replacement-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap_err();

        assert!(error.contains("remains unconfigured"));
        assert_eq!(
            credentials.current().as_deref(),
            Some(b"original-provider-secret".as_slice())
        );
        assert!(
            load_settings(&state.0.settings_path)
                .unwrap()
                .record(Provider::Anthropic)
                .is_none()
        );
    }

    #[test]
    fn failed_metadata_commit_keeps_a_repaired_key_disabled() {
        let directory = tempfile::tempdir().unwrap();
        let settings_parent = directory.path().join("provider-settings");
        std::fs::create_dir(&settings_parent).unwrap();
        let settings_path = settings_parent.join("settings.json");
        let credentials = Arc::new(StubCredentials::default());
        let state = ProviderState::new_with_credentials(
            credentials.clone(),
            settings_path.clone(),
            Arc::new(StubValidator::new([valid_attempt(), valid_attempt()])),
        );
        state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"original-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();
        credentials.deny_reads();

        let blocked_parent = settings_parent.clone();
        let backup_parent = directory.path().join("provider-settings-backup");
        let callback_backup = backup_parent.clone();
        credentials.after_next_set(move || {
            std::fs::rename(&blocked_parent, &callback_backup).unwrap();
            std::fs::write(&blocked_parent, b"not a directory").unwrap();
        });

        let error = state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"replacement-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap_err();

        std::fs::remove_file(&settings_parent).unwrap();
        std::fs::rename(&backup_parent, &settings_parent).unwrap();
        assert!(error.contains("It remains disabled"));
        assert_eq!(
            credentials.current().as_deref(),
            Some(b"replacement-provider-secret".as_slice())
        );
        assert!(
            load_settings(&settings_path)
                .unwrap()
                .record(Provider::Openai)
                .is_none()
        );
    }

    #[test]
    fn successful_keys_persist_only_native_status_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(&directory, [valid_attempt()]);
        let sentinel = b"sentinel-provider-secret";

        let result = state
            .configure(
                Provider::Openai,
                Zeroizing::new(sentinel.to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        assert_eq!(result.outcome, ProviderActionOutcome::Saved);
        assert!(result.configuration.configured);
        assert_eq!(
            result.configuration.validation_status,
            ValidationStatus::ModelAccessChecked
        );
        let settings = std::fs::read(directory.path().join("settings.json")).unwrap();
        assert!(
            !settings
                .windows(sentinel.len())
                .any(|window| window == sentinel)
        );
        let serialized = serde_json::to_vec(&result).unwrap();
        assert!(
            !serialized
                .windows(sentinel.len())
                .any(|window| window == sentinel)
        );
    }

    #[test]
    fn failed_and_cancelled_replacements_cannot_overwrite_a_saved_key() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(
            &directory,
            [
                valid_attempt(),
                ValidationAttempt::failed(
                    ValidationStatus::CredentialOrAccessInvalid,
                    Some("request-denied".into()),
                ),
            ],
        );
        state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"original-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        let failed = state
            .configure(
                Provider::Anthropic,
                Zeroizing::new(b"replacement-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        assert_eq!(failed.outcome, ProviderActionOutcome::ValidationFailed);
        assert_eq!(
            &*state.0.credentials.get("anthropic").unwrap(),
            b"original-provider-secret"
        );
    }

    #[test]
    fn recheck_records_sanitized_failure_and_remove_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(
            &directory,
            [
                valid_attempt(),
                ValidationAttempt::failed(ValidationStatus::NetworkOrTlsFailure, None),
            ],
        );
        state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"saved-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();

        let checked = state.revalidate(Provider::Openai).unwrap();
        assert_eq!(
            checked.configuration.validation_status,
            ValidationStatus::NetworkOrTlsFailure
        );
        assert!(checked.configuration.configured);
        assert!(checked.configuration.last_validated_at.is_some());

        assert_eq!(
            state.remove(Provider::Openai).unwrap().outcome,
            ProviderActionOutcome::Removed
        );
        assert_eq!(
            state.remove(Provider::Openai).unwrap().outcome,
            ProviderActionOutcome::Removed
        );
        assert!(!state.configuration(Provider::Openai).unwrap().configured);
    }

    #[test]
    fn concurrent_provider_actions_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(&directory, []);
        let first = state.begin_operation().unwrap();
        assert!(state.begin_operation().is_err());
        drop(first);
        assert!(state.begin_operation().is_ok());
    }

    #[test]
    fn orphaned_keychain_items_are_not_configuration_or_cost_consent() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(
            &directory,
            [ValidationAttempt::failed(
                ValidationStatus::NetworkOrTlsFailure,
                None,
            )],
        );
        state
            .0
            .credentials
            .set("openai", b"orphaned-provider-secret")
            .unwrap();

        let before = state.configuration(Provider::Openai).unwrap();
        assert!(!before.configured);
        assert!(before.disclosure_version.is_none());
        assert!(before.charges_acknowledged_at.is_none());

        let checked = state.revalidate(Provider::Openai).unwrap();
        assert!(!checked.configuration.configured);
        assert!(checked.configuration.disclosure_version.is_none());
        assert!(checked.configuration.charges_acknowledged_at.is_none());
    }

    #[test]
    fn removal_failures_remain_visible_and_retryable() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(&directory, [valid_attempt()]);
        state
            .configure(
                Provider::Openai,
                Zeroizing::new(b"saved-provider-secret".to_vec()),
                DISCLOSURE_VERSION,
            )
            .unwrap();
        let credential_path = directory.path().join("keys/openai.credential");
        std::fs::remove_file(&credential_path).unwrap();
        std::fs::create_dir(&credential_path).unwrap();

        assert!(state.remove(Provider::Openai).is_err());
        let pending = state.configuration(Provider::Openai).unwrap();
        assert!(!pending.configured);
        assert!(pending.removal_pending);
        assert!(pending.disclosure_version.is_none());
        assert!(pending.charges_acknowledged_at.is_none());

        std::fs::remove_dir(&credential_path).unwrap();
        let removed = state.remove(Provider::Openai).unwrap();
        assert_eq!(removed.outcome, ProviderActionOutcome::Removed);
        assert!(!removed.configuration.removal_pending);
    }

    #[test]
    fn stale_disclosure_versions_do_not_validate_or_store() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(&directory, [valid_attempt()]);

        assert!(
            state
                .configure(
                    Provider::Openai,
                    Zeroizing::new(b"provider-secret-not-stored".to_vec()),
                    DISCLOSURE_VERSION + 1,
                )
                .is_err()
        );
        assert!(!state.0.credentials.contains("openai").unwrap());
    }

    #[test]
    fn settings_reject_links_and_broad_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let other = directory.path().join("other.json");
        std::fs::write(&other, b"{}").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&other, &path).unwrap();
            assert!(load_settings(&path).is_err());
            std::fs::remove_file(&path).unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(&path, b"{}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_settings(&path).is_err());
        }
    }

    fn one_response(response: String) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let length = stream.read(&mut request).unwrap();
            sender
                .send(String::from_utf8_lossy(&request[..length]).into_owned())
                .unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}/v1/models"), receiver)
    }

    #[test]
    fn provider_validation_uses_fixed_auth_shapes_and_parses_catalogs() {
        let body = "{\"data\":[{\"id\":\"model-one\"},{\"id\":\"model-two\"}]}";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nx-request-id: req-openai\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (endpoint, request) = one_response(response);
        let attempt =
            validate_provider_at(Provider::Openai, b"sentinel-openai-key", &endpoint, false);
        assert_eq!(attempt.status, ValidationStatus::ModelAccessChecked);
        assert_eq!(attempt.model_count, Some(2));
        assert_eq!(attempt.request_id.as_deref(), Some("req-openai"));
        let request = request.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer sentinel-openai-key"));
        assert!(request.contains("x-client-request-id: pot-provider-check-"));

        let body = "{\"data\":[]}";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nrequest-id: req-anthropic\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let length = stream.read(&mut request).unwrap();
            sender
                .send(String::from_utf8_lossy(&request[..length]).into_owned())
                .unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        let attempt = validate_provider_at(
            Provider::Anthropic,
            b"sentinel-anthropic-key",
            &format!("http://{address}/v1/models?limit=100"),
            false,
        );
        assert_eq!(attempt.status, ValidationStatus::ModelUnavailable);
        assert_eq!(attempt.model_count, Some(0));
        let request = receiver.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("x-api-key: sentinel-anthropic-key"));
        assert!(request.contains("anthropic-version: 2023-06-01"));
        assert!(!request.contains("authorization:"));
    }

    #[test]
    fn redirects_and_provider_failures_are_closed_sanitized_categories() {
        for (status_line, expected) in [
            (
                "401 Unauthorized",
                ValidationStatus::CredentialOrAccessInvalid,
            ),
            ("402 Payment Required", ValidationStatus::BillingUnavailable),
            ("403 Forbidden", ValidationStatus::PermissionDenied),
            ("429 Too Many Requests", ValidationStatus::RateLimited),
            ("529 Site Overloaded", ValidationStatus::ProviderUnavailable),
            ("302 Found", ValidationStatus::InvalidProviderResponse),
        ] {
            let response = format!(
                "HTTP/1.1 {status_line}\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\n\r\n"
            );
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                stream.write_all(response.as_bytes()).unwrap();
            });
            let attempt = validate_provider_at(
                Provider::Openai,
                b"sentinel-valid-format",
                &format!("http://{address}/v1/models"),
                false,
            );
            assert_eq!(attempt.status, expected);
        }
    }

    #[test]
    fn malformed_keys_and_bodies_fail_without_raw_error_text() {
        assert_eq!(
            validate_provider_at(Provider::Openai, b"has a space", "https://invalid", true).status,
            ValidationStatus::InvalidKeyFormat
        );
        let body = "not-json";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            stream.write_all(response.as_bytes()).unwrap();
        });
        assert_eq!(
            validate_provider_at(
                Provider::Openai,
                b"valid-format-key",
                &format!("http://{address}/v1/models"),
                false,
            )
            .status,
            ValidationStatus::InvalidProviderResponse
        );
    }

    #[test]
    fn structured_billing_and_quota_codes_are_not_retry_advice() {
        assert_eq!(
            classify_provider_error(
                Provider::Openai,
                429,
                br#"{"error":{"code":"insufficient_quota","type":"insufficient_quota","message":"private provider text"}}"#,
                false,
            ),
            ValidationStatus::SpendOrUsageLimit
        );
        assert_eq!(
            classify_provider_error(
                Provider::Openai,
                429,
                br#"{"error":{"code":"rate_limit_exceeded","type":"rate_limit_error"}}"#,
                false,
            ),
            ValidationStatus::RateLimited
        );
        assert_eq!(
            classify_provider_error(
                Provider::Anthropic,
                400,
                br#"{"type":"error","error":{"type":"billing_error","message":"private provider text"}}"#,
                false,
            ),
            ValidationStatus::BillingUnavailable
        );
        assert_eq!(
            classify_provider_error(
                Provider::Openai,
                403,
                br#"{"error":{"code":"unsupported_country_region_territory"}}"#,
                false,
            ),
            ValidationStatus::UnsupportedRegion
        );

        for code in [
            "credit_balance_exhausted",
            "organization_spend_limit_exceeded",
            "project_spend_limit_exceeded",
            "organization_usage_limit_exceeded",
        ] {
            let body = format!(r#"{{"error":{{"code":"{code}"}}}}"#);
            assert_eq!(
                classify_provider_error(Provider::Openai, 429, body.as_bytes(), false),
                ValidationStatus::SpendOrUsageLimit
            );
        }
        assert_eq!(
            classify_provider_error(
                Provider::Anthropic,
                429,
                br#"{"type":"error","error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}"#,
                false,
            ),
            ValidationStatus::SpendOrUsageLimit
        );
        assert_eq!(
            classify_provider_error(
                Provider::Anthropic,
                400,
                br#"{"type":"error","error":{"type":"invalid_request_error","message":"You have reached your specified workspace API usage limits. Access resumes later."}}"#,
                false,
            ),
            ValidationStatus::SpendOrUsageLimit
        );
        assert_eq!(
            classify_provider_error(
                Provider::Anthropic,
                429,
                br#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
                true,
            ),
            ValidationStatus::RateLimited
        );
        assert_eq!(
            classify_provider_error(
                Provider::Anthropic,
                429,
                br#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
                false,
            ),
            ValidationStatus::InvalidProviderResponse
        );
    }

    #[test]
    fn decoded_response_and_request_id_bounds_fail_closed() {
        let body = ureq::Body::builder().data(vec![b'x'; 17]);
        let mut response = ureq::http::Response::builder()
            .status(200)
            .body(body)
            .unwrap();
        assert!(read_bounded_body(&mut response, 16).is_err());

        let body = ureq::Body::builder().data(Vec::<u8>::new());
        let response = ureq::http::Response::builder()
            .status(200)
            .header("x-request-id", "r".repeat(257))
            .body(body)
            .unwrap();
        assert_eq!(
            response_request_id(Provider::Openai, &response, b"sentinel-key"),
            None
        );

        let body = ureq::Body::builder().data(Vec::<u8>::new());
        let response = ureq::http::Response::builder()
            .status(200)
            .header("x-request-id", "prefix-sentinel-key-suffix")
            .body(body)
            .unwrap();
        assert_eq!(
            response_request_id(Provider::Openai, &response, b"sentinel-key"),
            None
        );
    }
}
