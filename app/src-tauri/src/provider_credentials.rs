//! The one native boundary for built-in provider keys.

use zeroize::Zeroize as _;

const MAX_KEY_BYTES: usize = 4096;
#[cfg(target_os = "macos")]
const SERVICE: &str = "ai.exalto.thought.provider";
#[cfg(target_os = "macos")]
const ITEM_NOT_FOUND: i32 = -25_300;

fn valid(key: &[u8]) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_BYTES && !key.contains(&0)
}

#[cfg(target_os = "macos")]
pub fn contains(provider: &str) -> Result<bool, String> {
    match security_framework::passwords::get_generic_password(SERVICE, provider) {
        Ok(mut key) => {
            let configured = valid(&key);
            key.zeroize();
            Ok(configured)
        }
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(false),
        Err(_) => Err("Could not access the Mac login Keychain.".into()),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn contains(_: &str) -> Result<bool, String> {
    Err("Provider keys are available only in the macOS app.".into())
}

#[cfg(target_os = "macos")]
pub fn set(provider: &str, key: &[u8]) -> Result<(), String> {
    if !valid(key) {
        return Err("Enter a non-empty API key smaller than 4 KiB.".into());
    }
    security_framework::passwords::set_generic_password(SERVICE, provider, key)
        .map_err(|_| "Could not save the provider key in Keychain.".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn set(_: &str, _: &[u8]) -> Result<(), String> {
    Err("Provider keys are available only in the macOS app.".into())
}

#[cfg(target_os = "macos")]
pub fn delete(provider: &str) -> Result<(), String> {
    match security_framework::passwords::delete_generic_password(SERVICE, provider) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
        Err(_) => Err("Could not remove the provider key from Keychain.".into()),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn delete(_: &str) -> Result<(), String> {
    Err("Provider keys are available only in the macOS app.".into())
}
