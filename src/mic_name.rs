//! Rename the VB-Cable recording endpoint so apps list the phone mic under a
//! recognizable name ("QuicMic") instead of "CABLE Output (VB-Audio Virtual
//! Cable)".
//!
//! Windows only. The rename writes the endpoint's `PKEY_Device_FriendlyName`
//! into the MMDevices property store:
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}\Properties`,
//! value `{a45c254e-df1c-4efd-8020-67d146a8f9be},2` (REG_SZ).
//! That hive is machine-wide, so the write needs an elevated process — without
//! admin rights the command fails with a plain-English hint instead of a raw
//! access-denied error.

/// Default name the `mic-name` console command applies.
pub const DEFAULT_MIC_NAME: &str = "QuicMic";

/// How the phone-mic capture endpoint's display name is managed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MicRenameMode {
    /// Rename the endpoint to the paired phone's device name on every pairing.
    Auto,
    /// Rename once at startup to the fixed name.
    Fixed(String),
    /// Never rename automatically.
    Off,
}

impl MicRenameMode {
    /// Human-readable label for the console status panel.
    pub fn label(&self) -> String {
        match self {
            MicRenameMode::Auto => "auto (phone name)".to_string(),
            MicRenameMode::Fixed(name) => format!("fixed ({name})"),
            MicRenameMode::Off => "off".to_string(),
        }
    }
}

/// Parse the `--rename-mic` argument: `auto`, `off`, or a fixed name.
/// No argument means auto on Windows (where renames work), off elsewhere.
pub fn parse_rename_mic(arg: Option<&str>) -> MicRenameMode {
    match arg {
        None => {
            #[cfg(windows)]
            {
                MicRenameMode::Auto
            }
            #[cfg(not(windows))]
            {
                MicRenameMode::Off
            }
        }
        Some(s) if s.eq_ignore_ascii_case("auto") => MicRenameMode::Auto,
        Some(s) if s.eq_ignore_ascii_case("off") => MicRenameMode::Off,
        Some(s) => MicRenameMode::Fixed(s.to_string()),
    }
}

/// Substring identifying VB-Cable's recording endpoint (case-insensitive).
/// Only meaningful on Windows (WASAPI enumeration); compiled for tests
/// everywhere so the matcher stays unit-tested.
#[cfg(any(windows, test))]
const CABLE_CAPTURE_MATCH: &str = "cable output";

/// Substring of VB-Cable's driver-set device description (case-insensitive).
/// Unlike the friendly name, the description survives `mic-name` renames, so
/// matching it keeps repeat renames finding the same endpoint.
#[cfg(any(windows, test))]
const CABLE_DESC_MATCH: &str = "vb-audio virtual cable";

/// Registry value holding `PKEY_Device_FriendlyName` in an endpoint's
/// `...\MMDevices\Audio\Capture\{guid}\Properties` key.
/// Windows-only; see above.
#[cfg(windows)]
const FRIENDLY_NAME_VALUE: &str = "{a45c254e-df1c-4efd-8020-67d146a8f9be},2";

/// Rename the VB-Cable recording endpoint to `new_name` (`None` → "QuicMic").
///
/// Returns the name that was applied. On non-Windows builds this is a stub
/// that explains the command is Windows-only.
pub fn rename_mic(new_name: Option<&str>) -> anyhow::Result<String> {
    let name = new_name.unwrap_or(DEFAULT_MIC_NAME).trim();
    if name.is_empty() {
        anyhow::bail!("Usage: mic-name [name]  (empty names are not allowed)");
    }
    #[cfg(windows)]
    {
        rename_mic_windows(name)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        anyhow::bail!("mic-name is only available on the Windows build.");
    }
}

/// Extract the endpoint GUID from a WASAPI endpoint ID string.
///
/// IDs look like `{0.0.1.00000000}.{5f23ab69-6181-4f4a-81a4-45414013aac8}`;
/// the MMDevices registry path uses the GUID in the second brace group.
/// Production use is Windows-only; compiled everywhere for the unit tests.
#[cfg(any(windows, test))]
fn endpoint_guid_from_id(id: &str) -> Option<String> {
    let guid = id
        .rsplit('.')
        .next()?
        .trim_matches(|c| c == '{' || c == '}');
    let looks_like_guid = guid.len() == 36 && guid.chars().filter(|&c| c == '-').count() == 4;
    looks_like_guid.then(|| guid.to_string())
}

/// Decide whether a capture endpoint is VB-Cable's recording endpoint.
///
/// Matches the friendly name (`CABLE Output`, the driver default) **or** the
/// driver-set device description (`VB-Audio Virtual Cable`). The description
/// survives friendly-name renames, so a second `mic-name` run — or an
/// automatic rename on the next phone pairing — still finds the endpoint
/// after the first rename changed its friendly name to e.g. `QuicMic`.
/// Pure string logic, unit-tested.
#[cfg(any(windows, test))]
fn is_cable_capture_endpoint(friendly_name: &str, device_desc: &str) -> bool {
    friendly_name.to_lowercase().contains(CABLE_CAPTURE_MATCH)
        || device_desc.to_lowercase().contains(CABLE_DESC_MATCH)
}

#[cfg(windows)]
fn rename_mic_windows(name: &str) -> anyhow::Result<String> {
    use windows::core::HSTRING;
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
    };
    use windows::Win32::Foundation::{E_ACCESSDENIED, PROPERTYKEY};
    use windows::Win32::Media::Audio::{
        eCapture, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_SZ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    /// Read a string property (e.g. friendly name, device description) from an
    /// endpoint's property store. Missing/unreadable values become "".
    fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> anyhow::Result<String> {
        let mut pv = unsafe { store.GetValue(key)? };
        let s = unsafe {
            pv.Anonymous
                .Anonymous
                .Anonymous
                .pwszVal
                .to_string()
                .unwrap_or_default()
        };
        unsafe { PropVariantClear(&mut pv)? };
        Ok(s)
    }

    // 1. Find VB-Cable's recording endpoint among the active capture devices.
    let (current_name, endpoint_id) = unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        let mut matches = Vec::new();
        for i in 0..count {
            let dev: IMMDevice = collection.Item(i)?;
            let store: IPropertyStore = dev.OpenPropertyStore(STGM_READ)?;
            let dev_name = prop_string(&store, &PKEY_Device_FriendlyName)?;
            let dev_desc = prop_string(&store, &PKEY_Device_DeviceDesc)?;
            if is_cable_capture_endpoint(&dev_name, &dev_desc) {
                let id = dev.GetId()?.to_string().unwrap_or_default();
                matches.push((dev_name, id));
            }
        }
        match matches.len() {
            0 => anyhow::bail!(
                "No VB-Cable recording endpoint found. This command renames the \
                 \"CABLE Output\" device that carries the phone mic into your apps — \
                 install VB-Audio Virtual Cable first."
            ),
            1 => matches.into_iter().next().unwrap(),
            _ => {
                let list = matches
                    .iter()
                    .map(|(n, _)| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "Found several cable endpoints ({list}). Uninstall the extra \
                     VB-Cables, or rename the one you use by hand in \
                     Settings → System → Sound."
                );
            }
        }
    };

    // 2. Map the endpoint ID to its MMDevices registry key.
    let guid = endpoint_guid_from_id(&endpoint_id)
        .ok_or_else(|| anyhow::anyhow!("Could not parse the endpoint ID of \"{current_name}\"."))?;
    let subkey = HSTRING::from(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Capture\\{{{guid}}}\\Properties"
    ));

    // 3. Write the friendly name. The hive is machine-wide: without elevation
    //    this fails with E_ACCESSDENIED, which we translate into the fix.
    let mut hkey = HKEY::default();
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if let Err(e) = open.ok() {
        if e.code() == E_ACCESSDENIED {
            anyhow::bail!(
                "Windows refused the rename (access denied). Right-click the exe → \
                 \"Run as administrator\", then run mic-name again — one elevated run \
                 is enough, the name sticks afterwards."
            );
        }
        return Err(e.into());
    }
    let value_name = HSTRING::from(FRIENDLY_NAME_VALUE);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let set_result = unsafe { RegSetValueExW(hkey, &value_name, Some(0), REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    set_result.ok()?;

    tracing::info!(old = %current_name, new = %name, "renamed mic endpoint");
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_parses_from_endpoint_id() {
        let id = "{0.0.1.00000000}.{5f23ab69-6181-4f4a-81a4-45414013aac8}";
        assert_eq!(
            endpoint_guid_from_id(id).as_deref(),
            Some("5f23ab69-6181-4f4a-81a4-45414013aac8")
        );
    }

    #[test]
    fn guid_rejects_garbage() {
        assert_eq!(endpoint_guid_from_id("not-an-id"), None);
        assert_eq!(endpoint_guid_from_id("{0.0.1.00000000}"), None);
        assert_eq!(endpoint_guid_from_id(""), None);
    }

    #[test]
    fn empty_name_is_rejected() {
        assert!(rename_mic(Some("   ")).is_err());
    }

    #[test]
    fn matches_default_cable_output_name() {
        assert!(is_cable_capture_endpoint(
            "CABLE Output (VB-Audio Virtual Cable)",
            "VB-Audio Virtual Cable"
        ));
    }

    #[test]
    fn matches_by_description_after_rename() {
        // After the first rename the friendly name no longer contains
        // "cable output" — the driver description must still match.
        assert!(is_cable_capture_endpoint(
            "QuicMic",
            "VB-Audio Virtual Cable"
        ));
        assert!(is_cable_capture_endpoint(
            "iPhone",
            "VB-Audio Virtual Cable"
        ));
    }

    #[test]
    fn rejects_unrelated_endpoints() {
        assert!(!is_cable_capture_endpoint(
            "Microphone (Realtek Audio)",
            "Realtek High Definition Audio"
        ));
        assert!(!is_cable_capture_endpoint("", ""));
    }

    #[test]
    fn parse_rename_mic_modes() {
        assert_eq!(parse_rename_mic(Some("auto")), MicRenameMode::Auto);
        assert_eq!(parse_rename_mic(Some("AUTO")), MicRenameMode::Auto);
        assert_eq!(parse_rename_mic(Some("off")), MicRenameMode::Off);
        assert_eq!(
            parse_rename_mic(Some("Studio Mic")),
            MicRenameMode::Fixed("Studio Mic".to_string())
        );
    }

    #[test]
    fn parse_rename_mic_default_follows_platform() {
        #[cfg(windows)]
        assert_eq!(parse_rename_mic(None), MicRenameMode::Auto);
        #[cfg(not(windows))]
        assert_eq!(parse_rename_mic(None), MicRenameMode::Off);
    }
}
