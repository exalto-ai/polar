//! Private credential storage for reviewer routes and built-in providers.
//!
//! Production macOS builds use the login Keychain. Tests and non-macOS
//! development use an explicit user-only file store so CI does not depend on a
//! desktop secret-service session. Reviewer credentials are shared only with
//! the daemon helpers. Provider keys use a separate app-owned Keychain service
//! and never cross into the webview.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "macos")]
const REVIEWER_KEYCHAIN_SERVICE: &str = "ai.exalto.thought.reviewer";
#[cfg(target_os = "macos")]
const REVIEWER_KEYCHAIN_DESCRIPTION: &str = "Proof of Thought reviewer connection";
#[cfg(target_os = "macos")]
const PROVIDER_KEYCHAIN_SERVICE: &str = "ai.exalto.thought.provider";
#[cfg(target_os = "macos")]
const PROVIDER_KEYCHAIN_DESCRIPTION: &str = "Proof of Thought AI provider key";
#[cfg(target_os = "macos")]
const ERR_SEC_DUPLICATE_ITEM: i32 = -25_299;
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25_308;
#[cfg(debug_assertions)]
const FILE_BACKEND_ENV: &str = "THOUGHT_CREDENTIAL_BACKEND";
const MAX_CREDENTIAL_LENGTH: usize = 4096;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
mod macos_keychain {
    use super::{
        CredentialError, ERR_SEC_DUPLICATE_ITEM, ERR_SEC_INTERACTION_NOT_ALLOWED,
        ERR_SEC_ITEM_NOT_FOUND, PROVIDER_KEYCHAIN_DESCRIPTION, PROVIDER_KEYCHAIN_SERVICE,
        REVIEWER_KEYCHAIN_DESCRIPTION, REVIEWER_KEYCHAIN_SERVICE,
    };
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeID, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation::{declare_TCFType, impl_TCFType};
    use std::ffi::{CString, c_char};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};
    use std::ptr;

    pub enum OpaqueTrustedApplication {}
    type TrustedApplicationRef = *mut OpaqueTrustedApplication;
    declare_TCFType!(TrustedApplication, TrustedApplicationRef);
    impl_TCFType!(
        TrustedApplication,
        TrustedApplicationRef,
        SecTrustedApplicationGetTypeID
    );

    pub enum OpaqueAccess {}
    type AccessRef = *mut OpaqueAccess;
    declare_TCFType!(Access, AccessRef);
    impl_TCFType!(Access, AccessRef, SecAccessGetTypeID);

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecTrustedApplicationGetTypeID() -> CFTypeID;
        fn SecTrustedApplicationCreateFromPath(
            path: *const c_char,
            application: *mut TrustedApplicationRef,
        ) -> i32;
        fn SecAccessGetTypeID() -> CFTypeID;
        fn SecAccessCreate(
            descriptor: CFStringRef,
            trusted_list: CFArrayRef,
            access: *mut AccessRef,
        ) -> i32;
        fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
        fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> i32;
        static kSecAttrAccess: CFStringRef;
        static kSecValueData: CFStringRef;
        static kSecReturnData: CFStringRef;
        static kSecUseAuthenticationUI: CFStringRef;
        static kSecUseAuthenticationUIFail: CFStringRef;
    }

    trait ProviderSecurityItemApi {
        fn copy_matching(&self, query: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
        fn update(&self, query: CFDictionaryRef, attributes: CFDictionaryRef) -> i32;
    }

    struct SystemProviderSecurityItemApi;

    impl ProviderSecurityItemApi for SystemProviderSecurityItemApi {
        fn copy_matching(&self, query: CFDictionaryRef, result: *mut CFTypeRef) -> i32 {
            unsafe { SecItemCopyMatching(query, result) }
        }

        fn update(&self, query: CFDictionaryRef, attributes: CFDictionaryRef) -> i32 {
            unsafe { SecItemUpdate(query, attributes) }
        }
    }

    pub(super) fn set_reviewer_password(
        connection_id: &str,
        credential: &[u8],
    ) -> Result<(), CredentialError> {
        let executable = std::env::current_exe().map_err(CredentialError::Io)?;
        let paths = trusted_executable_paths(&executable)?;
        let access = access_for_paths(&paths, REVIEWER_KEYCHAIN_DESCRIPTION)?;
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            REVIEWER_KEYCHAIN_SERVICE,
            connection_id,
        );
        #[allow(deprecated)]
        options.query.extend([
            (
                unsafe { CFString::wrap_under_get_rule(kSecAttrAccess) },
                access.clone().into_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSecValueData) },
                CFData::from_buffer(credential).into_CFType(),
            ),
        ]);
        #[allow(deprecated)]
        let add = CFDictionary::from_CFType_pairs(&options.query);
        let status = unsafe { SecItemAdd(add.as_concrete_TypeRef(), ptr::null_mut()) };
        if status == 0 {
            return Ok(());
        }
        if status != ERR_SEC_DUPLICATE_ITEM {
            return security_status("create reviewer credential", status);
        }

        let query = security_framework::passwords::PasswordOptions::new_generic_password(
            REVIEWER_KEYCHAIN_SERVICE,
            connection_id,
        );
        #[allow(deprecated)]
        let query = CFDictionary::from_CFType_pairs(&query.query);
        let update: [(CFString, CFType); 2] = [
            (
                unsafe { CFString::wrap_under_get_rule(kSecAttrAccess) },
                access.into_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSecValueData) },
                CFData::from_buffer(credential).into_CFType(),
            ),
        ];
        let update = CFDictionary::from_CFType_pairs(&update);
        let status =
            unsafe { SecItemUpdate(query.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
        security_status("update reviewer credential and access", status)
    }

    /// Provider credentials are used only by the signed app process. Reset the
    /// access list on both creation and replacement so another same-user app
    /// cannot pre-create this public service/account and keep read access when
    /// Proof of Thought stores the person's validated key.
    pub(super) fn set_provider_password(
        provider_id: &str,
        credential: &[u8],
    ) -> Result<(), CredentialError> {
        let executable = std::env::current_exe().map_err(CredentialError::Io)?;
        let access = access_for_paths(&[executable], PROVIDER_KEYCHAIN_DESCRIPTION)?;
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            PROVIDER_KEYCHAIN_SERVICE,
            provider_id,
        );
        #[allow(deprecated)]
        options.query.extend([
            (
                unsafe { CFString::wrap_under_get_rule(kSecAttrAccess) },
                access.clone().into_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSecValueData) },
                CFData::from_buffer(credential).into_CFType(),
            ),
        ]);
        #[allow(deprecated)]
        let add = CFDictionary::from_CFType_pairs(&options.query);
        let status = unsafe { SecItemAdd(add.as_concrete_TypeRef(), ptr::null_mut()) };
        if status == 0 {
            return Ok(());
        }
        if status != ERR_SEC_DUPLICATE_ITEM {
            return security_status("create provider credential", status);
        }

        let query = security_framework::passwords::PasswordOptions::new_generic_password(
            PROVIDER_KEYCHAIN_SERVICE,
            provider_id,
        );
        #[allow(deprecated)]
        let query = CFDictionary::from_CFType_pairs(&query.query);
        let update: [(CFString, CFType); 2] = [
            (
                unsafe { CFString::wrap_under_get_rule(kSecAttrAccess) },
                access.into_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSecValueData) },
                CFData::from_buffer(credential).into_CFType(),
            ),
        ];
        let update = CFDictionary::from_CFType_pairs(&update);
        let status = SystemProviderSecurityItemApi
            .update(query.as_concrete_TypeRef(), update.as_concrete_TypeRef());
        security_status("replace provider credential and access", status)
    }

    /// Check for a provider item without asking Security.framework to return
    /// its secret bytes or display authentication UI. Status surfaces can
    /// therefore stay metadata-only and cannot block the editor.
    pub(super) fn provider_password_exists(provider_id: &str) -> Result<bool, CredentialError> {
        provider_password_exists_with_api(&SystemProviderSecurityItemApi, provider_id)
    }

    fn provider_password_exists_with_api(
        api: &impl ProviderSecurityItemApi,
        provider_id: &str,
    ) -> Result<bool, CredentialError> {
        let options = provider_password_no_ui_options(provider_id);
        #[allow(deprecated)]
        let query = CFDictionary::from_CFType_pairs(&options.query);
        let status = api.copy_matching(query.as_concrete_TypeRef(), ptr::null_mut());
        match status {
            0 => Ok(true),
            ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            status => Err(provider_access_error(
                status,
                security_framework::base::Error::from_code(status).to_string(),
            )),
        }
    }

    /// Read a provider key without changing its owner or access list and
    /// without allowing Security.framework to display application-modal UI.
    /// Add and Replace are the only operations that may change access.
    pub(super) fn provider_password(provider_id: &str) -> Result<Vec<u8>, CredentialError> {
        provider_password_with_api(&SystemProviderSecurityItemApi, provider_id)
    }

    fn provider_password_with_api(
        api: &impl ProviderSecurityItemApi,
        provider_id: &str,
    ) -> Result<Vec<u8>, CredentialError> {
        let mut options = provider_password_no_ui_options(provider_id);
        #[allow(deprecated)]
        options.query.push((
            unsafe { CFString::wrap_under_get_rule(kSecReturnData) },
            CFBoolean::true_value().into_CFType(),
        ));
        #[allow(deprecated)]
        let query = CFDictionary::from_CFType_pairs(&options.query);
        let mut result = ptr::null();
        let status = api.copy_matching(query.as_concrete_TypeRef(), &mut result);
        if status == ERR_SEC_ITEM_NOT_FOUND {
            return Err(CredentialError::Missing);
        }
        if status != 0 {
            return Err(provider_access_error(
                status,
                security_framework::base::Error::from_code(status).to_string(),
            ));
        }
        if result.is_null() {
            return Err(CredentialError::Platform(
                "native credential storage returned no provider data".into(),
            ));
        }
        let result = unsafe { CFType::wrap_under_create_rule(result) };
        let data = result.downcast_into::<CFData>().ok_or_else(|| {
            CredentialError::Platform(
                "native credential storage returned invalid provider data".into(),
            )
        })?;
        Ok(data.bytes().to_vec())
    }

    fn provider_access_error(status: i32, message: String) -> CredentialError {
        if status == ERR_SEC_INTERACTION_NOT_ALLOWED {
            CredentialError::InteractionRequired
        } else {
            CredentialError::Platform(message)
        }
    }

    fn provider_password_no_ui_options(
        provider_id: &str,
    ) -> security_framework::passwords::PasswordOptions {
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            PROVIDER_KEYCHAIN_SERVICE,
            provider_id,
        );
        #[allow(deprecated)]
        options.query.push((
            unsafe { CFString::wrap_under_get_rule(kSecUseAuthenticationUI) },
            unsafe { CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail) }.into_CFType(),
        ));
        options
    }

    fn trusted_executable_paths(executable: &Path) -> Result<[PathBuf; 2], CredentialError> {
        let paths = trusted_executable_path_names(executable)?;
        for path in &paths {
            if !path.is_file() {
                return Err(CredentialError::Platform(format!(
                    "trusted reviewer helper is missing: {}",
                    path.display()
                )));
            }
        }
        Ok(paths)
    }

    fn trusted_executable_path_names(executable: &Path) -> Result<[PathBuf; 2], CredentialError> {
        let directory = executable.parent().ok_or_else(|| {
            CredentialError::Platform("current executable has no parent directory".into())
        })?;
        Ok([
            directory.join("thoughtd"),
            directory.join("thought-mcp-stdio"),
        ])
    }

    fn access_for_paths(paths: &[PathBuf], description: &str) -> Result<Access, CredentialError> {
        let trusted = paths
            .iter()
            .map(|path| trusted_application(path))
            .collect::<Result<Vec<_>, _>>()?;
        let trusted_list = CFArray::from_CFTypes(&trusted);
        let descriptor = CFString::new(description);
        let mut access = ptr::null_mut();
        let status = unsafe {
            SecAccessCreate(
                descriptor.as_concrete_TypeRef(),
                trusted_list.as_concrete_TypeRef(),
                &mut access,
            )
        };
        security_status("create credential access", status)?;
        Ok(unsafe { Access::wrap_under_create_rule(access) })
    }

    fn trusted_application(path: &Path) -> Result<TrustedApplication, CredentialError> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            CredentialError::Platform(format!(
                "trusted reviewer helper path contains a null byte: {}",
                path.display()
            ))
        })?;
        let mut application = ptr::null_mut();
        let status =
            unsafe { SecTrustedApplicationCreateFromPath(path.as_ptr(), &mut application) };
        security_status("create trusted reviewer application", status)?;
        Ok(unsafe { TrustedApplication::wrap_under_create_rule(application) })
    }

    fn security_status(operation: &str, status: i32) -> Result<(), CredentialError> {
        if status == 0 {
            Ok(())
        } else {
            Err(CredentialError::Platform(format!(
                "{operation}: {}",
                security_framework::base::Error::from_code(status)
            )))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            ProviderSecurityItemApi, access_for_paths, provider_access_error,
            provider_password_exists_with_api, provider_password_no_ui_options,
            provider_password_with_api, trusted_executable_path_names,
        };
        use core_foundation::base::{CFTypeRef, TCFType as _};
        use core_foundation::data::CFData;
        use core_foundation::dictionary::CFDictionaryRef;
        use core_foundation::string::CFString;
        use std::path::Path;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct RecordingProviderSecurityItemApi {
            copy_calls: AtomicUsize,
            update_calls: AtomicUsize,
        }

        impl ProviderSecurityItemApi for RecordingProviderSecurityItemApi {
            fn copy_matching(&self, _: CFDictionaryRef, result: *mut CFTypeRef) -> i32 {
                self.copy_calls.fetch_add(1, Ordering::Relaxed);
                if !result.is_null() {
                    let data = CFData::from_buffer(b"recorded-provider-secret");
                    let reference = data.as_CFTypeRef();
                    std::mem::forget(data);
                    unsafe { result.write(reference) };
                }
                0
            }

            fn update(&self, _: CFDictionaryRef, _: CFDictionaryRef) -> i32 {
                self.update_calls.fetch_add(1, Ordering::Relaxed);
                0
            }
        }

        #[test]
        fn trusted_helpers_are_siblings_of_the_running_daemon() {
            let paths = trusted_executable_path_names(Path::new(
                "/Applications/Proof of Thought.app/Contents/MacOS/thoughtd",
            ))
            .unwrap();
            assert_eq!(
                [
                    Path::new("/Applications/Proof of Thought.app/Contents/MacOS/thoughtd")
                        .to_path_buf(),
                    Path::new(
                        "/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio"
                    )
                    .to_path_buf()
                ],
                paths
            );
        }

        #[test]
        fn native_access_object_can_be_built_without_keychain_io() {
            let current = std::env::current_exe().unwrap();
            access_for_paths(
                &[current.clone(), current],
                super::REVIEWER_KEYCHAIN_DESCRIPTION,
            )
            .unwrap();
        }

        #[test]
        fn provider_access_object_trusts_only_the_running_app() {
            let current = std::env::current_exe().unwrap();
            access_for_paths(&[current], super::PROVIDER_KEYCHAIN_DESCRIPTION).unwrap();
        }

        #[test]
        fn provider_lookups_disable_ui_and_never_include_an_access_rewrite() {
            let options = provider_password_no_ui_options("openai");
            let authentication_ui =
                unsafe { CFString::wrap_under_get_rule(super::kSecUseAuthenticationUI) };
            let authentication_ui_fail =
                unsafe { CFString::wrap_under_get_rule(super::kSecUseAuthenticationUIFail) }
                    .into_CFType();
            let access = unsafe { CFString::wrap_under_get_rule(super::kSecAttrAccess) };

            #[allow(deprecated)]
            let configured_ui = options
                .query
                .iter()
                .find(|(key, _)| key == &authentication_ui)
                .map(|(_, value)| value);
            assert_eq!(configured_ui, Some(&authentication_ui_fail));
            #[allow(deprecated)]
            let includes_access = options.query.iter().any(|(key, _)| key == &access);
            assert!(!includes_access);
        }

        #[test]
        fn blocked_provider_lookup_has_a_distinct_recovery_error() {
            assert!(matches!(
                provider_access_error(super::ERR_SEC_INTERACTION_NOT_ALLOWED, "ignored".into()),
                super::CredentialError::InteractionRequired
            ));
        }

        #[test]
        fn repeated_provider_reads_use_copy_matching_without_updates() {
            let api = RecordingProviderSecurityItemApi::default();

            for _ in 0..3 {
                assert!(provider_password_exists_with_api(&api, "openai").unwrap());
                assert_eq!(
                    provider_password_with_api(&api, "openai").unwrap(),
                    b"recorded-provider-secret"
                );
            }

            assert_eq!(api.copy_calls.load(Ordering::Relaxed), 6);
            assert_eq!(api.update_calls.load(Ordering::Relaxed), 0);
        }
    }
}

#[derive(Debug)]
pub enum CredentialError {
    InvalidConnectionId,
    InvalidProviderId,
    Missing,
    InteractionRequired,
    Io(io::Error),
    Platform(String),
    InvalidStoredCredential,
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConnectionId => formatter.write_str("invalid reviewer connection ID"),
            Self::InvalidProviderId => formatter.write_str("invalid provider ID"),
            Self::Missing => formatter.write_str("reviewer credential is missing"),
            Self::InteractionRequired => {
                formatter.write_str("credential access requires explicit authorization")
            }
            Self::Io(error) => write!(formatter, "credential storage: {error}"),
            Self::Platform(error) => write!(formatter, "native credential storage: {error}"),
            Self::InvalidStoredCredential => {
                formatter.write_str("stored reviewer credential is invalid")
            }
        }
    }
}

impl std::error::Error for CredentialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CredentialError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone)]
enum Backend {
    #[cfg(target_os = "macos")]
    Keychain,
    Files(PathBuf),
}

#[derive(Debug, Clone)]
pub struct CredentialStore {
    backend: Backend,
}

impl CredentialStore {
    /// Use the native platform store. Debug test binaries can opt into the file
    /// backend with `THOUGHT_CREDENTIAL_BACKEND=file` and an isolated home.
    /// Release binaries do not compile this environment override.
    pub fn platform(application_home: impl AsRef<Path>) -> Self {
        #[cfg(debug_assertions)]
        if debug_file_backend_requested(std::env::var_os(FILE_BACKEND_ENV).as_deref()) {
            return Self::files(application_home.as_ref().join("reviewer-credentials"));
        }

        #[cfg(target_os = "macos")]
        {
            let _ = application_home;
            Self {
                backend: Backend::Keychain,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::files(application_home.as_ref().join("reviewer-credentials"))
        }
    }

    pub fn files(directory: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::Files(directory.into()),
        }
    }

    pub fn set(&self, connection_id: &str, credential: &[u8]) -> Result<(), CredentialError> {
        validate_connection_id(connection_id)?;
        if credential.is_empty() || credential.len() > MAX_CREDENTIAL_LENGTH {
            return Err(CredentialError::InvalidStoredCredential);
        }
        match &self.backend {
            #[cfg(target_os = "macos")]
            Backend::Keychain => macos_keychain::set_reviewer_password(connection_id, credential),
            Backend::Files(directory) => write_private_file(directory, connection_id, credential),
        }
    }

    pub fn get(&self, connection_id: &str) -> Result<Vec<u8>, CredentialError> {
        validate_connection_id(connection_id)?;
        let credential = match &self.backend {
            #[cfg(target_os = "macos")]
            Backend::Keychain => security_framework::passwords::generic_password(
                security_framework::passwords::PasswordOptions::new_generic_password(
                    REVIEWER_KEYCHAIN_SERVICE,
                    connection_id,
                ),
            )
            .map_err(|error| {
                if error.code() == ERR_SEC_ITEM_NOT_FOUND {
                    CredentialError::Missing
                } else {
                    CredentialError::Platform(error.to_string())
                }
            })?,
            Backend::Files(directory) => read_private_file(directory, connection_id)?,
        };
        if credential.is_empty() || credential.len() > MAX_CREDENTIAL_LENGTH {
            return Err(CredentialError::InvalidStoredCredential);
        }
        Ok(credential)
    }

    pub fn delete(&self, connection_id: &str) -> Result<(), CredentialError> {
        validate_connection_id(connection_id)?;
        match &self.backend {
            #[cfg(target_os = "macos")]
            Backend::Keychain => {
                match security_framework::passwords::delete_generic_password(
                    REVIEWER_KEYCHAIN_SERVICE,
                    connection_id,
                ) {
                    Ok(()) => Ok(()),
                    Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
                    Err(error) => Err(CredentialError::Platform(error.to_string())),
                }
            }
            Backend::Files(directory) => {
                let path = credential_path(directory, connection_id);
                match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
enum ProviderBackend {
    #[cfg(target_os = "macos")]
    Keychain,
    #[cfg(not(target_os = "macos"))]
    Unavailable,
    Files(PathBuf),
}

/// Native-only storage for a person's OpenAI or Anthropic API key.
///
/// The production macOS backend uses a service separate from reviewer route
/// credentials. Only the app process needs these keys. The explicit file
/// backend exists for isolated tests and non-macOS development, not as a
/// release-grade desktop vault.
#[derive(Debug, Clone)]
pub struct ProviderCredentialStore {
    backend: ProviderBackend,
}

impl ProviderCredentialStore {
    pub fn platform(application_home: impl AsRef<Path>) -> Self {
        #[cfg(target_os = "macos")]
        {
            let _ = application_home;
            Self {
                backend: ProviderBackend::Keychain,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = application_home;
            Self {
                backend: ProviderBackend::Unavailable,
            }
        }
    }

    pub fn files(directory: impl Into<PathBuf>) -> Self {
        Self {
            backend: ProviderBackend::Files(directory.into()),
        }
    }

    pub fn set(&self, provider_id: &str, credential: &[u8]) -> Result<(), CredentialError> {
        validate_provider_id(provider_id)?;
        validate_credential(credential)?;
        match &self.backend {
            #[cfg(target_os = "macos")]
            ProviderBackend::Keychain => {
                macos_keychain::set_provider_password(provider_id, credential)
            }
            #[cfg(not(target_os = "macos"))]
            ProviderBackend::Unavailable => Err(CredentialError::Platform(
                "secure provider storage is unavailable on this platform".into(),
            )),
            ProviderBackend::Files(directory) => {
                write_private_file(directory, provider_id, credential)
            }
        }
    }

    pub fn get(&self, provider_id: &str) -> Result<zeroize::Zeroizing<Vec<u8>>, CredentialError> {
        validate_provider_id(provider_id)?;
        let credential = match &self.backend {
            #[cfg(target_os = "macos")]
            ProviderBackend::Keychain => macos_keychain::provider_password(provider_id)?,
            #[cfg(not(target_os = "macos"))]
            ProviderBackend::Unavailable => {
                return Err(CredentialError::Platform(
                    "secure provider storage is unavailable on this platform".into(),
                ));
            }
            ProviderBackend::Files(directory) => read_private_file(directory, provider_id)
                .map_err(|error| match error {
                    CredentialError::Io(ref source) if source.kind() == io::ErrorKind::NotFound => {
                        CredentialError::Missing
                    }
                    other => other,
                })?,
        };
        validate_credential(&credential)?;
        Ok(zeroize::Zeroizing::new(credential))
    }

    pub fn contains(&self, provider_id: &str) -> Result<bool, CredentialError> {
        validate_provider_id(provider_id)?;
        match &self.backend {
            #[cfg(target_os = "macos")]
            ProviderBackend::Keychain => macos_keychain::provider_password_exists(provider_id),
            #[cfg(not(target_os = "macos"))]
            ProviderBackend::Unavailable => Err(CredentialError::Platform(
                "secure provider storage is unavailable on this platform".into(),
            )),
            ProviderBackend::Files(directory) => {
                let path = credential_path(directory, provider_id);
                match std::fs::symlink_metadata(path) {
                    Ok(metadata) => {
                        Ok(metadata.file_type().is_file() && !metadata.file_type().is_symlink())
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    pub fn delete(&self, provider_id: &str) -> Result<(), CredentialError> {
        validate_provider_id(provider_id)?;
        match &self.backend {
            #[cfg(target_os = "macos")]
            ProviderBackend::Keychain => {
                match security_framework::passwords::delete_generic_password(
                    PROVIDER_KEYCHAIN_SERVICE,
                    provider_id,
                ) {
                    Ok(()) => Ok(()),
                    Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
                    Err(error) => Err(CredentialError::Platform(error.to_string())),
                }
            }
            #[cfg(not(target_os = "macos"))]
            ProviderBackend::Unavailable => Err(CredentialError::Platform(
                "secure provider storage is unavailable on this platform".into(),
            )),
            ProviderBackend::Files(directory) => {
                let path = credential_path(directory, provider_id);
                match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }
}

#[cfg(debug_assertions)]
fn debug_file_backend_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("file"))
}

fn validate_connection_id(connection_id: &str) -> Result<(), CredentialError> {
    if connection_id.is_empty()
        || connection_id.len() > 64
        || !connection_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CredentialError::InvalidConnectionId);
    }
    Ok(())
}

fn validate_provider_id(provider_id: &str) -> Result<(), CredentialError> {
    if matches!(provider_id, "openai" | "anthropic") {
        Ok(())
    } else {
        Err(CredentialError::InvalidProviderId)
    }
}

fn validate_credential(credential: &[u8]) -> Result<(), CredentialError> {
    if credential.is_empty() || credential.len() > MAX_CREDENTIAL_LENGTH {
        Err(CredentialError::InvalidStoredCredential)
    } else {
        Ok(())
    }
}

fn credential_path(directory: &Path, connection_id: &str) -> PathBuf {
    directory.join(format!("{connection_id}.credential"))
}

fn ensure_private_directory(directory: &Path) -> Result<(), CredentialError> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(
    directory: &Path,
    connection_id: &str,
    credential: &[u8],
) -> Result<(), CredentialError> {
    ensure_private_directory(directory)?;
    let sequence = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(
        ".{connection_id}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let destination = credential_path(directory, connection_id);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(credential)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &destination)?;
        sync_directory(directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn read_private_file(directory: &Path, connection_id: &str) -> Result<Vec<u8>, CredentialError> {
    let path = credential_path(directory, connection_id);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CredentialError::InvalidStoredCredential);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CredentialError::InvalidStoredCredential);
        }
    }
    let mut credential = Vec::new();
    File::open(path)?
        .take((MAX_CREDENTIAL_LENGTH + 1) as u64)
        .read_to_end(&mut credential)?;
    if credential.is_empty() || credential.len() > MAX_CREDENTIAL_LENGTH {
        return Err(CredentialError::InvalidStoredCredential);
    }
    Ok(credential)
}

fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialError, CredentialStore, ProviderCredentialStore, debug_file_backend_requested,
    };

    #[test]
    fn debug_file_backend_override_is_exact() {
        use std::ffi::OsStr;

        assert!(debug_file_backend_requested(Some(OsStr::new("file"))));
        assert!(!debug_file_backend_requested(Some(OsStr::new("FILE"))));
        assert!(!debug_file_backend_requested(None));
    }

    #[test]
    fn file_store_round_trips_rotates_and_deletes() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::files(directory.path());
        let id = "019c1234-abcd-7def-8123-123456789abc";

        store.set(id, b"first").unwrap();
        assert_eq!(store.get(id).unwrap(), b"first");
        store.set(id, b"second").unwrap();
        assert_eq!(store.get(id).unwrap(), b"second");
        store.delete(id).unwrap();
        assert!(matches!(store.get(id), Err(CredentialError::Io(_))));
        store.delete(id).unwrap();
    }

    #[test]
    fn connection_ids_cannot_escape_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::files(directory.path());
        for invalid in ["", "../escape", "UPPER", "has space", "/absolute"] {
            assert!(matches!(
                store.set(invalid, b"secret"),
                Err(CredentialError::InvalidConnectionId)
            ));
        }
    }

    #[test]
    fn credentials_have_a_small_native_storage_bound() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::files(directory.path());
        let id = "019c1234-abcd-7def-8123-123456789abc";
        assert!(matches!(
            store.set(id, &[b'x'; 4097]),
            Err(CredentialError::InvalidStoredCredential)
        ));
    }

    #[test]
    fn provider_file_store_has_a_separate_fixed_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::files(directory.path());

        assert!(!store.contains("openai").unwrap());
        store.set("openai", b"first-provider-secret").unwrap();
        assert!(store.contains("openai").unwrap());
        assert_eq!(&*store.get("openai").unwrap(), b"first-provider-secret");
        store.set("openai", b"replacement-provider-secret").unwrap();
        assert_eq!(
            &*store.get("openai").unwrap(),
            b"replacement-provider-secret"
        );
        store.delete("openai").unwrap();
        assert!(!store.contains("openai").unwrap());
        store.delete("openai").unwrap();

        assert!(matches!(
            store.set("custom-provider", b"secret"),
            Err(CredentialError::InvalidProviderId)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_refuses_credentials_with_broad_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::files(directory.path());
        let id = "019c1234-abcd-7def-8123-123456789abc";
        store.set(id, b"secret").unwrap();
        let path = directory.path().join(format!("{id}.credential"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.get(id),
            Err(CredentialError::InvalidStoredCredential)
        ));
    }
}
