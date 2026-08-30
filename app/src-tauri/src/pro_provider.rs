//! Minimal built-in provider configuration.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::provider_credentials;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Openai,
    Anthropic,
}

impl Provider {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderConfiguration {
    provider: Provider,
    configured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderActionOutcome {
    Saved,
    Removed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderActionResult {
    outcome: ProviderActionOutcome,
    configuration: ProviderConfiguration,
}

fn configuration(provider: Provider) -> Result<ProviderConfiguration, String> {
    Ok(ProviderConfiguration {
        provider,
        configured: provider_credentials::contains(provider.id())?,
    })
}

pub(crate) fn credential(provider: Provider) -> Result<Zeroizing<Vec<u8>>, String> {
    provider_credentials::get(provider.id())
}

#[tauri::command]
pub async fn provider_configurations() -> Result<Vec<ProviderConfiguration>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        [Provider::Openai, Provider::Anthropic]
            .into_iter()
            .map(configuration)
            .collect()
    })
    .await
    .map_err(|_| "Could not read provider configuration.".to_string())?
}

#[tauri::command]
pub async fn configure_provider_key(
    app: tauri::AppHandle,
    provider: Provider,
) -> Result<ProviderActionResult, String> {
    #[cfg(target_os = "macos")]
    let key = crate::macos_secure_input::prompt_key(
        app,
        provider.name(),
        configuration(provider)?.configured,
    )
    .await?;
    #[cfg(not(target_os = "macos"))]
    let key: Option<Zeroizing<Vec<u8>>> = {
        let _ = app;
        return Err("Provider keys are available only in the macOS app.".into());
    };

    let outcome = if let Some(key) = key {
        tauri::async_runtime::spawn_blocking(move || {
            provider_credentials::set(provider.id(), &key)
        })
        .await
        .map_err(|_| "Could not save the provider key.".to_string())??;
        ProviderActionOutcome::Saved
    } else {
        ProviderActionOutcome::Cancelled
    };
    Ok(ProviderActionResult {
        outcome,
        configuration: configuration(provider)?,
    })
}

#[tauri::command]
pub async fn remove_provider_key(
    app: tauri::AppHandle,
    provider: Provider,
) -> Result<ProviderActionResult, String> {
    #[cfg(target_os = "macos")]
    let confirmed = crate::macos_secure_input::confirm_remove(app, provider.name()).await?;
    #[cfg(not(target_os = "macos"))]
    let confirmed = {
        let _ = app;
        return Err("Provider keys are available only in the macOS app.".into());
    };

    let outcome = if confirmed {
        tauri::async_runtime::spawn_blocking(move || provider_credentials::delete(provider.id()))
            .await
            .map_err(|_| "Could not remove the provider key.".to_string())??;
        ProviderActionOutcome::Removed
    } else {
        ProviderActionOutcome::Cancelled
    };
    Ok(ProviderActionResult {
        outcome,
        configuration: configuration(provider)?,
    })
}
