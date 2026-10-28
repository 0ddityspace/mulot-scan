//! Linux backend: V4L2 (uvcvideo) streaming + `UVCIOC_CTRL_QUERY` for the
//! vendor extension unit.

use std::fs;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use v4l::buffer::Type;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

use super::{FrameSource, PRODUCT_ID, VENDOR_ID, XuControl, protocol};

const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;

/// Extension unit id as declared in the USB descriptors of the mouse.
const DEFAULT_UNIT: u8 = 6;

#[repr(C)]
struct UvcXuControlQuery {
    unit: u8,
    selector: u8,
    query: u8,
    size: u16,
    data: *mut u8,
}

const fn iowr(ty: u8, nr: u8, size: usize) -> libc::c_ulong {
    ((3u64 << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr as u64) as libc::c_ulong
}

const UVCIOC_CTRL_QUERY: libc::c_ulong = iowr(b'u', 0x21, std::mem::size_of::<UvcXuControlQuery>());

pub struct LinuxXu {
    device: Arc<v4l::device::Handle>,
    unit: u8,
}

// The handle is only a file descriptor; ioctls on it are thread-safe.
unsafe impl Send for LinuxXu {}
unsafe impl Sync for LinuxXu {}

impl LinuxXu {
    fn query(&self, selector: u8, query: u8, buf: &mut [u8]) -> Result<()> {
        query_raw(self.device.fd(), self.unit, selector, query, buf)
    }
}

fn query_raw(fd: c_int, unit: u8, selector: u8, query: u8, buf: &mut [u8]) -> Result<()> {
    let mut q = UvcXuControlQuery { unit, selector, query, size: buf.len() as u16, data: buf.as_mut_ptr() };
    // SAFETY: `q` points to a live buffer of `size` bytes for the duration of the call.
    let r = unsafe { libc::ioctl(fd, UVCIOC_CTRL_QUERY as _, &mut q as *mut UvcXuControlQuery) };
    if r < 0 {
        let err = std::io::Error::last_os_error();
        bail!("UVC XU query (unit {unit}, selector {selector}, query {query:#x}) failed: {err}");
    }
    Ok(())
}

impl XuControl for LinuxXu {
    fn len(&self, selector: u8) -> Result<u16> {
        let mut b = [0u8; 2];
        self.query(selector, UVC_GET_LEN, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    fn get(&self, selector: u8, buf: &mut [u8]) -> Result<()> {
        self.query(selector, UVC_GET_CUR, buf)
    }

    fn set(&self, selector: u8, buf: &[u8]) -> Result<()> {
        let mut tmp = buf.to_vec();
        self.query(selector, UVC_SET_CUR, &mut tmp)
    }
}

/// Reads a sysfs attribute of the USB device a video node belongs to.
fn usb_attr(video_node: &Path, attr: &str) -> Option<String> {
    let name = video_node.file_name()?.to_str()?;
    let mut dir: PathBuf = fs::canonicalize(format!("/sys/class/video4linux/{name}/device")).ok()?;
    // Walk up from the interface to the USB device holding idVendor.
    for _ in 0..4 {
        let p = dir.join(attr);
        if let Ok(s) = fs::read_to_string(&p) {
            return Some(s.trim().to_string());
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// Finds the capture node of the scanner mouse.
pub fn find_device() -> Result<PathBuf> {
    let mut nodes: Vec<PathBuf> = fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("video")))
        .collect();
    nodes.sort();
    for node in nodes {
        let vid = usb_attr(&node, "idVendor");
        let pid = usb_attr(&node, "idProduct");
        let matches = vid.as_deref().and_then(|v| u16::from_str_radix(v, 16).ok()) == Some(VENDOR_ID)
            && pid.as_deref().and_then(|v| u16::from_str_radix(v, 16).ok()) == Some(PRODUCT_ID);
        if !matches {
            continue;
        }
        let Ok(dev) = Device::with_path(&node) else { continue };
        // Skip metadata nodes: they expose no capture format.
        if dev.enum_formats().map(|f| f.iter().any(|f| f.fourcc == FourCC::new(b"MJPG"))).unwrap_or(false) {
            return Ok(node);
        }
    }
    Err(protocol::not_found())
}

pub struct LinuxBackend {
    path: PathBuf,
    device: Device,
}

impl LinuxBackend {
    pub fn open() -> Result<Self> {
        let path = find_device()?;
        let device = Device::with_path(&path).with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { path, device })
    }

    pub fn description(&self) -> String {
        format!("V4L2 {}", self.path.display())
    }

    pub fn xu(&self) -> Result<Arc<dyn XuControl>> {
        let handle = self.device.handle();
        let fd = handle.fd();
        // Find the unit exposing our selectors (6 on all known units).
        let mut candidates = vec![DEFAULT_UNIT];
        candidates.extend((1..=16u8).filter(|&u| u != DEFAULT_UNIT));
        for unit in candidates {
            let mut info = [0u8; 1];
            let mut len = [0u8; 2];
            if query_raw(fd, unit, protocol::SEL_REGISTER, UVC_GET_INFO, &mut info).is_ok()
                && query_raw(fd, unit, protocol::SEL_REGISTER, UVC_GET_LEN, &mut len).is_ok()
                && u16::from_le_bytes(len) == 5
            {
                return Ok(Arc::new(LinuxXu { device: handle, unit }));
            }
        }
        Err(anyhow!("vendor extension unit not found on {}", self.path.display()))
    }

    pub fn open_stream(&self) -> Result<Box<dyn FrameSource>> {
        let mut fmt = self.device.format()?;
        fmt.width = super::frame::FRAME_WIDTH;
        fmt.height = super::frame::FRAME_HEIGHT;
        fmt.fourcc = FourCC::new(b"MJPG");
        self.device.set_format(&fmt)?;
        // The stream keeps its own reference to the device handle; the mmap'ed
        // buffers live as long as the stream itself.
        let mut stream: MmapStream<'static> = MmapStream::with_buffers(&self.device, Type::VideoCapture, 8)?;
        stream.set_timeout(Duration::from_millis(1000));
        Ok(Box::new(LinuxStream { stream: Some(stream) }))
    }
}

pub struct LinuxStream {
    stream: Option<MmapStream<'static>>,
}

unsafe impl Send for LinuxStream {}

impl FrameSource for LinuxStream {
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let stream = self.stream.as_mut().ok_or_else(|| anyhow!("stream closed"))?;
        match stream.next() {
            Ok((buf, meta)) => {
                let n = (meta.bytesused as usize).min(buf.len());
                Ok(Some(buf[..n].to_vec()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}
