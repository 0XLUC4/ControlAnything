//! Audio endpoints through Windows Core Audio: render (output) and capture (input) devices.

/// One active audio device.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub input: bool,
    /// Headphones or headset form factor (icon only).
    pub headphones: bool,
    pub is_default: bool,
    /// Master volume, 0.0 to 1.0.
    pub volume: f32,
    pub muted: bool,
}

/// Active output devices first, then input devices.
pub fn list() -> Result<Vec<Endpoint>, String> {
    backend::list().map_err(|err| err.to_string())
}

pub fn set_volume(id: &str, volume: f32) -> Result<(), String> {
    backend::set_volume(id, volume.clamp(0.0, 1.0)).map_err(|err| err.to_string())
}

pub fn set_mute(id: &str, muted: bool) -> Result<(), String> {
    backend::set_mute(id, muted).map_err(|err| err.to_string())
}

/// Makes the device the default for every role (console, multimedia, communications).
pub fn set_default(id: &str) -> Result<(), String> {
    backend::set_default(id).map_err(|err| err.to_string())
}

#[cfg(not(windows))]
mod backend {
    use super::Endpoint;
    use crate::system::UNAVAILABLE;

    pub fn list() -> Result<Vec<Endpoint>, &'static str> {
        Err(UNAVAILABLE)
    }
    pub fn set_volume(_: &str, _: f32) -> Result<(), &'static str> {
        Err(UNAVAILABLE)
    }
    pub fn set_mute(_: &str, _: bool) -> Result<(), &'static str> {
        Err(UNAVAILABLE)
    }
    pub fn set_default(_: &str) -> Result<(), &'static str> {
        Err(UNAVAILABLE)
    }
}

// Every call below is unsafe only because it is FFI: the COM smart pointers are valid
// for their lifetime, and the strings and out-pointers passed live across each call.
// IPolicyConfig keeps its COM method names (the interface macro takes no attributes).
#[cfg(windows)]
#[allow(non_snake_case)]
mod backend {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, EDataFlow, ERole, Headphones, Headset, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, PKEY_AudioEndpoint_FormFactor, eCapture, eCommunications, eConsole,
        eMultimedia, eRender,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PropVariantToStringAlloc, PropVariantToUInt32WithDefault,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ,
    };
    use windows::core::{GUID, HRESULT, HSTRING, PCWSTR, Result};

    use super::Endpoint;

    /// COM for the duration of one call, on any thread (UI or worker pool).
    /// Declare it before any COM object so it drops last. RPC_E_CHANGED_MODE (the
    /// thread is already in another apartment) is usable but must not be uninitialized.
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            // SAFETY: no reserved pointer.
            Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the successful CoInitializeEx above on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn enumerator() -> Result<IMMDeviceEnumerator> {
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
    }

    fn device_id(device: &IMMDevice) -> Result<String> {
        unsafe {
            let raw = device.GetId()?;
            let id = raw.to_string().unwrap_or_default();
            // GetId allocates the string with the COM allocator: the caller frees it.
            CoTaskMemFree(Some(raw.0 as *const _));
            Ok(id)
        }
    }

    /// Friendly name and "headphones or headset" flag.
    fn name_and_form(device: &IMMDevice) -> Result<(String, bool)> {
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ)?;
            let mut name = store.GetValue(&PKEY_Device_FriendlyName)?;
            // Allocated copy: no length limit on the name.
            let read = PropVariantToStringAlloc(&name);
            // The PROPVARIANT owns its string: clear it whatever happened.
            let _ = PropVariantClear(&mut name);
            let raw = read?;
            let text = raw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw.0 as *const _));
            let mut form = store.GetValue(&PKEY_AudioEndpoint_FormFactor)?;
            let form_factor = PropVariantToUInt32WithDefault(&form, 0) as i32;
            let _ = PropVariantClear(&mut form);
            Ok((
                text,
                form_factor == Headphones.0 || form_factor == Headset.0,
            ))
        }
    }

    fn endpoint(device: &IMMDevice, flow: EDataFlow, default_id: Option<&str>) -> Result<Endpoint> {
        let id = device_id(device)?;
        let (name, headphones) = name_and_form(device)?;
        unsafe {
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            Ok(Endpoint {
                is_default: default_id == Some(id.as_str()),
                id,
                name,
                input: flow == eCapture,
                headphones,
                volume: volume.GetMasterVolumeLevelScalar()?,
                muted: volume.GetMute()?.as_bool(),
            })
        }
    }

    fn collect(
        enumerator: &IMMDeviceEnumerator,
        flow: EDataFlow,
        out: &mut Vec<Endpoint>,
    ) -> Result<()> {
        unsafe {
            // No default device (e.g. no microphone plugged) is not an error.
            let default_id = enumerator
                .GetDefaultAudioEndpoint(flow, eConsole)
                .ok()
                .and_then(|device| device_id(&device).ok());
            let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            for i in 0..devices.GetCount()? {
                // One broken device (driver error) must not hide the others.
                match devices
                    .Item(i)
                    .and_then(|device| endpoint(&device, flow, default_id.as_deref()))
                {
                    Ok(endpoint) => out.push(endpoint),
                    Err(err) => tracing::warn!("audio: device {i} skipped: {err}"),
                }
            }
        }
        Ok(())
    }

    pub fn list() -> Result<Vec<Endpoint>> {
        let _com = Com::init();
        let enumerator = enumerator()?;
        let mut endpoints = Vec::new();
        collect(&enumerator, eRender, &mut endpoints)?;
        collect(&enumerator, eCapture, &mut endpoints)?;
        Ok(endpoints)
    }

    fn endpoint_volume(id: &str) -> Result<IAudioEndpointVolume> {
        unsafe {
            enumerator()?
                .GetDevice(&HSTRING::from(id))?
                .Activate(CLSCTX_ALL, None)
        }
    }

    pub fn set_volume(id: &str, volume: f32) -> Result<()> {
        let _com = Com::init();
        unsafe { endpoint_volume(id)?.SetMasterVolumeLevelScalar(volume, std::ptr::null()) }
    }

    pub fn set_mute(id: &str, muted: bool) -> Result<()> {
        let _com = Com::init();
        unsafe { endpoint_volume(id)?.SetMute(muted, std::ptr::null()) }
    }

    /// Undocumented interface behind the Windows Sound settings, stable since Vista.
    /// Only `SetDefaultEndpoint` is called: the methods before it only fill their
    /// vtable slots, so their real parameters are omitted.
    #[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: windows_core::IUnknown {
        fn GetMixFormat(&self) -> HRESULT;
        fn GetDeviceFormat(&self) -> HRESULT;
        fn ResetDeviceFormat(&self) -> HRESULT;
        fn SetDeviceFormat(&self) -> HRESULT;
        fn GetProcessingPeriod(&self) -> HRESULT;
        fn SetProcessingPeriod(&self) -> HRESULT;
        fn GetShareMode(&self) -> HRESULT;
        fn SetShareMode(&self) -> HRESULT;
        fn GetPropertyValue(&self) -> HRESULT;
        fn SetPropertyValue(&self) -> HRESULT;
        fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
    }

    /// CLSID of CPolicyConfigClient.
    const POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    pub fn set_default(id: &str) -> Result<()> {
        let _com = Com::init();
        let id = HSTRING::from(id);
        unsafe {
            let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG, None, CLSCTX_ALL)?;
            for role in [eConsole, eMultimedia, eCommunications] {
                policy.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role).ok()?;
            }
        }
        Ok(())
    }
}
