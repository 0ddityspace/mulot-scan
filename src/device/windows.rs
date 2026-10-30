//! Windows backend: Media Foundation capture + `IKsControl` for the vendor
//! extension unit (works with the in-box `usbvideo.sys` driver, the original
//! `OVCamUVC_Ext.dll` plug-in is not required).

use std::ffi::c_void;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsTopologyInfo, KSIDENTIFIER, KSNODETYPE_DEV_SPECIFIC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, Interface, PWSTR};

use super::{FrameSource, XuControl, protocol};

/// Property set of the vendor extension unit.
const XU_GUID: GUID = GUID::from_u128(0xdd880f8a_1cba_4954_8a25_f7875967f0f7);

const KSPROPERTY_TYPE_GET: u32 = 0x0000_0001;
const KSPROPERTY_TYPE_SET: u32 = 0x0000_0002;
const KSPROPERTY_TYPE_TOPOLOGY: u32 = 0x1000_0000;
const HRESULT_MORE_DATA: i32 = 0x8007_00EAu32 as i32;

/// `KSP_NODE` laid out manually (avoids the union in `KSIDENTIFIER`).
#[repr(C)]
struct KspNode {
    set: GUID,
    id: u32,
    flags: u32,
    node_id: u32,
    reserved: u32,
}

fn init_com_mf() -> Result<()> {
    unsafe {
        // Ignore RPC_E_CHANGED_MODE: COM already initialised by the UI toolkit.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;
    }
    Ok(())
}

pub struct WinXu {
    ks: IKsControl,
    node: u32,
}

// SAFETY: COM objects created in the multithreaded apartment (see
// `init_com_mf`) may be used from any thread.
unsafe impl Send for WinXu {}
unsafe impl Sync for WinXu {}

impl WinXu {
    fn property(&self, selector: u8, flags: u32, data: *mut c_void, len: u32) -> windows::core::Result<u32> {
        let prop = KspNode {
            set: XU_GUID,
            id: selector as u32,
            flags: flags | KSPROPERTY_TYPE_TOPOLOGY,
            node_id: self.node,
            reserved: 0,
        };
        let mut returned = 0u32;
        unsafe {
            self.ks.KsProperty(
                &prop as *const KspNode as *const KSIDENTIFIER,
                std::mem::size_of::<KspNode>() as u32,
                data,
                len,
                &mut returned,
            )?;
        }
        Ok(returned)
    }
}

impl XuControl for WinXu {
    fn len(&self, selector: u8) -> Result<u16> {
        let prop = KspNode {
            set: XU_GUID,
            id: selector as u32,
            flags: KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_TOPOLOGY,
            node_id: self.node,
            reserved: 0,
        };
        let mut returned = 0u32;
        let r = unsafe {
            self.ks.KsProperty(
                &prop as *const KspNode as *const KSIDENTIFIER,
                std::mem::size_of::<KspNode>() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
            )
        };
        match r {
            Ok(()) => Ok(returned as u16),
            Err(e) if e.code().0 == HRESULT_MORE_DATA => Ok(returned as u16),
            Err(e) => Err(anyhow!("XU length query for selector {selector} failed: {e}")),
        }
    }

    fn get(&self, selector: u8, buf: &mut [u8]) -> Result<()> {
        self.property(selector, KSPROPERTY_TYPE_GET, buf.as_mut_ptr() as *mut c_void, buf.len() as u32)
            .map(|_| ())
            .map_err(|e| anyhow!("XU GET selector {selector} failed: {e}"))
    }

    fn set(&self, selector: u8, buf: &[u8]) -> Result<()> {
        let mut tmp = buf.to_vec();
        self.property(selector, KSPROPERTY_TYPE_SET, tmp.as_mut_ptr() as *mut c_void, tmp.len() as u32)
            .map(|_| ())
            .map_err(|e| anyhow!("XU SET selector {selector} failed: {e}"))
    }
}

pub struct WindowsBackend {
    source: IMFMediaSource,
    symlink: String,
}

// SAFETY: as for `WinXu`, the media source lives in the multithreaded apartment.
unsafe impl Send for WindowsBackend {}
unsafe impl Sync for WindowsBackend {}

impl WindowsBackend {
    pub fn open() -> Result<Self> {
        init_com_mf()?;
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))?;
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;

            let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attrs, &mut devices, &mut count)?;
            let mut found: Option<(IMFActivate, String)> = None;
            for i in 0..count as usize {
                let slot = &mut *devices.add(i);
                let Some(act) = slot.take() else { continue };
                let mut s = PWSTR::null();
                let mut len = 0u32;
                if act
                    .GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &mut s, &mut len)
                    .is_ok()
                {
                    let link = s.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(s.0 as *const c_void));
                    let lower = link.to_ascii_lowercase();
                    if found.is_none()
                        && lower.contains(&format!("vid_{:04x}", super::VENDOR_ID))
                        && lower.contains(&format!("pid_{:04x}", super::PRODUCT_ID))
                    {
                        found = Some((act, link));
                    }
                }
            }
            if !devices.is_null() {
                CoTaskMemFree(Some(devices as *const c_void));
            }
            let (act, symlink) = found.ok_or_else(protocol::not_found)?;
            let source: IMFMediaSource = act.ActivateObject().context("activating media source")?;
            Ok(Self { source, symlink })
        }
    }

    pub fn description(&self) -> String {
        format!("Media Foundation {}", self.symlink)
    }

    pub fn xu(&self) -> Result<Arc<dyn XuControl>> {
        let ks: IKsControl = self.source.cast().context("IKsControl not available")?;
        let topo: IKsTopologyInfo = self.source.cast().context("IKsTopologyInfo not available")?;
        let n = unsafe { topo.NumNodes()? };
        let mut candidates = Vec::new();
        for i in 0..n {
            if let Ok(t) = unsafe { topo.get_NodeType(i) } {
                if t == KSNODETYPE_DEV_SPECIFIC {
                    candidates.push(i);
                }
            }
        }
        // Fall back to every node if the topology does not label it.
        let rest: Vec<u32> = (0..n).filter(|i| !candidates.contains(i)).collect();
        candidates.extend(rest);
        for node in candidates {
            let xu = WinXu { ks: ks.clone(), node };
            if xu.len(protocol::SEL_REGISTER).map(|l| l == 5).unwrap_or(false) {
                return Ok(Arc::new(xu));
            }
        }
        bail!("vendor extension unit not found in device topology")
    }

    pub fn open_stream(&self) -> Result<Box<dyn FrameSource>> {
        unsafe {
            let reader = MFCreateSourceReaderFromMediaSource(&self.source, None)?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let mut chosen = None;
            let mut i = 0;
            while let Ok(mt) = reader.GetNativeMediaType(stream, i) {
                i += 1;
                let subtype = mt.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default();
                let size = mt.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                let (w, h) = ((size >> 32) as u32, size as u32);
                if subtype == MFVideoFormat_MJPG {
                    let exact = w == super::frame::FRAME_WIDTH && h == super::frame::FRAME_HEIGHT;
                    if exact || chosen.is_none() {
                        chosen = Some(mt);
                    }
                    if exact {
                        break;
                    }
                }
            }
            let mt = chosen.ok_or_else(|| anyhow!("no MJPG media type exposed by the device"))?;
            reader.SetCurrentMediaType(stream, None, &mt)?;
            reader.SetStreamSelection(stream, true)?;
            Ok(Box::new(WinStream { reader, stream }))
        }
    }
}

pub struct WinStream {
    reader: IMFSourceReader,
    stream: u32,
}

// SAFETY: the source reader is created in the multithreaded apartment and is
// only used by the capture thread that owns the stream.
unsafe impl Send for WinStream {}

impl FrameSource for WinStream {
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        unsafe {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            self.reader
                .ReadSample(self.stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .context("ReadSample")?;
            if flags & (MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
                bail!("capture stream error");
            }
            let Some(sample) = sample else { return Ok(None) };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            Ok(Some(data))
        }
    }
}

impl Drop for WinStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.reader.Flush(self.stream);
        }
    }
}
