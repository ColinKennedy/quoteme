#[cfg(windows)]
mod win {
    use anyhow::{Context, Result};
    use windows::{
        Win32::Media::Audio::{
            eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        },
        Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
        },
    };

    fn get_endpoint_volume() -> Result<IAudioEndpointVolume> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("Failed to create IMMDeviceEnumerator")?;

            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .context("Failed to get default audio endpoint")?;

            // Generic Activate<T>: return type is inferred from the binding
            device
                .Activate(CLSCTX_ALL, None)
                .context("Failed to activate IAudioEndpointVolume")
        }
    }

    pub fn mute() -> Result<()> {
        unsafe {
            get_endpoint_volume()?.SetMute(true, std::ptr::null())?;
        }
        tracing::debug!("System audio muted");
        Ok(())
    }

    pub fn unmute() -> Result<()> {
        unsafe {
            get_endpoint_volume()?.SetMute(false, std::ptr::null())?;
        }
        tracing::debug!("System audio unmuted");
        Ok(())
    }
}

pub fn mute_system_audio() {
    #[cfg(windows)]
    if let Err(e) = win::mute() {
        tracing::warn!("Failed to mute system audio: {}", e);
    }
    #[cfg(not(windows))]
    tracing::warn!("System audio muting is Windows-only");
}

pub fn unmute_system_audio() {
    #[cfg(windows)]
    if let Err(e) = win::unmute() {
        tracing::warn!("Failed to unmute system audio: {}", e);
    }
    #[cfg(not(windows))]
    tracing::warn!("System audio unmuting is Windows-only");
}
