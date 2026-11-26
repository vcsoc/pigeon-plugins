//! Resource writer (section 6): pixels, encoded images and frames live in files under the session
//! directory; `resource.put` references them by path.
//!
//! Each `rid` cycles through a ring of [`RING_SLOTS`] files (`<rid>.<seq % 3>.<ext>`). A slot is
//! rewritten only after the host acknowledged the sequence number that used it (`resource.ack`),
//! so the host never sees a file change under its feet. Files are written to a temporary name and
//! renamed into place, so a reader never sees a partial frame.
//!
//! ```no_run
//! # fn demo(ctx: &pigeon_plugin_sdk::runtime::Ctx) -> Result<(), Box<dyn std::error::Error>> {
//! let pixels = vec![0u8; 64 * 36 * 4];              // RGBA8, straight alpha
//! let seq = ctx.resources().put_rgba("thumb:42", 64, 36, &pixels, false)?;
//! # let _ = seq; Ok(()) }
//! ```

use std::fmt;
use std::fs;
use std::io::{self};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::protocol::{Outgoing, ResourceKind, ResourcePut};
use crate::runtime::{Ctx, ServiceError};
use crate::transport::lock;

/// Files per resource id. The host must have acknowledged `seq - RING_SLOTS` before `seq` is written.
pub const RING_SLOTS: u64 = 3;
/// How long a put waits for the host to free a slot before failing with [`ResourceError::Busy`].
pub const DEFAULT_ACK_WAIT: Duration = Duration::from_secs(2);

/// Why a resource could not be published.
#[derive(Debug)]
pub enum ResourceError {
    /// The host did not acknowledge an older frame of this id in time.
    Busy {
        rid: String,
    },
    /// The host gave no session directory.
    NoSessionDir,
    /// The pixel buffer does not match `w * h * 4`.
    BadSize {
        expected: usize,
        actual: usize,
    },
    /// A bad resource id (empty, or longer than 256 bytes).
    BadRid(String),
    Io(io::Error),
    Service(ServiceError),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::Busy { rid } => {
                write!(f, "host has not released an older frame of {rid}")
            }
            ResourceError::NoSessionDir => {
                write!(f, "the host did not provide a session directory")
            }
            ResourceError::BadSize { expected, actual } => {
                write!(f, "pixel buffer is {actual} bytes, expected {expected}")
            }
            ResourceError::BadRid(r) => write!(f, "invalid resource id {r:?}"),
            ResourceError::Io(e) => write!(f, "cannot write resource file: {e}"),
            ResourceError::Service(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResourceError {}

impl From<io::Error> for ResourceError {
    fn from(e: io::Error) -> ResourceError {
        ResourceError::Io(e)
    }
}

impl From<ServiceError> for ResourceError {
    fn from(e: ServiceError) -> ResourceError {
        ResourceError::Service(e)
    }
}

/// Publishes resources for one connection. Obtain it with [`Ctx::resources`].
pub struct Resources {
    ctx: Ctx,
    ack_wait: Duration,
}

impl Resources {
    pub(crate) fn new(ctx: Ctx) -> Resources {
        Resources {
            ctx,
            ack_wait: DEFAULT_ACK_WAIT,
        }
    }

    /// Change how long puts wait for the host to release a slot.
    pub fn with_ack_wait(mut self, wait: Duration) -> Resources {
        self.ack_wait = wait;
        self
    }

    /// Publish straight-alpha RGBA8 pixels (`w * h * 4` bytes, row stride `w * 4`). Returns the sequence number.
    pub fn put_rgba(
        &self,
        rid: &str,
        w: u32,
        h: u32,
        pixels: &[u8],
        premultiplied: bool,
    ) -> Result<u64, ResourceError> {
        let expected = w as usize * h as usize * 4;
        if pixels.len() != expected {
            return Err(ResourceError::BadSize {
                expected,
                actual: pixels.len(),
            });
        }
        self.publish(
            rid,
            ResourceKind::Rgba,
            "rgba",
            pixels,
            w,
            h,
            Some(w * 4),
            premultiplied,
        )
    }

    /// Publish an encoded image (`png`, `jpeg`, `webp` or `svg`). `w`/`h` are the decoded size (0 for svg).
    pub fn put_encoded(
        &self,
        rid: &str,
        kind: ResourceKind,
        bytes: &[u8],
        w: u32,
        h: u32,
    ) -> Result<u64, ResourceError> {
        let ext = match kind {
            ResourceKind::Rgba => {
                return Err(ResourceError::BadSize {
                    expected: 0,
                    actual: bytes.len(),
                })
            }
            ResourceKind::Png => "png",
            ResourceKind::Jpeg => "jpg",
            ResourceKind::Webp => "webp",
            ResourceKind::Svg => "svg",
        };
        self.publish(rid, kind, ext, bytes, w, h, None, false)
    }

    /// Tell the host to forget `rid`. Its files are removed when the ring is next used or at exit.
    pub fn drop_resource(&self, rid: &str) -> Result<(), ResourceError> {
        check_rid(rid)?;
        {
            let mut acks = lock(&self.ctx.shared.acks);
            acks.remove(rid);
        }
        lock(&self.ctx.shared.rings).remove(rid);
        self.ctx.send(&Outgoing::ResourceDrop { rid: rid.into() })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        rid: &str,
        kind: ResourceKind,
        ext: &str,
        bytes: &[u8],
        w: u32,
        h: u32,
        stride: Option<u32>,
        premultiplied: bool,
    ) -> Result<u64, ResourceError> {
        check_rid(rid)?;
        let dir = self.ctx.session_dir();
        if dir.as_os_str().is_empty() {
            return Err(ResourceError::NoSessionDir);
        }
        // One publisher at a time keeps sequence numbers gap-free: a failed put (Busy, I/O) must
        // not consume a number the host will never acknowledge.
        let _publishing = lock(&self.ctx.shared.publish_lock);
        let seq = lock(&self.ctx.shared.rings).get(rid).copied().unwrap_or(0) + 1;
        self.wait_for_slot(rid, seq)?;
        let slot = seq % RING_SLOTS;
        let file: PathBuf = dir.join(format!("{}.{slot}.{ext}", sanitize(rid)));
        let tmp = dir.join(format!("{}.{slot}.{ext}.tmp", sanitize(rid)));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &file)?;
        let put = ResourcePut {
            rid: rid.into(),
            kind,
            file: file.to_string_lossy().into_owned(),
            w,
            h,
            stride,
            seq,
            premultiplied,
        };
        self.ctx.send(&Outgoing::ResourcePut(put))?;
        lock(&self.ctx.shared.rings).insert(rid.to_string(), seq);
        Ok(seq)
    }

    /// Block until the host acknowledged `seq - RING_SLOTS` (or the slot was never used).
    fn wait_for_slot(&self, rid: &str, seq: u64) -> Result<(), ResourceError> {
        let deadline = Instant::now() + self.ack_wait;
        let shared = &self.ctx.shared;
        let mut acks = lock(&shared.acks);
        loop {
            let acked = acks.get(rid).copied().unwrap_or(0);
            if seq <= acked + RING_SLOTS {
                return Ok(());
            }
            if !self.ctx.is_open() {
                return Err(ResourceError::Service(ServiceError::Closed));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ResourceError::Busy { rid: rid.into() });
            }
            let (guard, _) = match shared.acks_cv.wait_timeout(acks, deadline - now) {
                Ok(pair) => pair,
                Err(p) => p.into_inner(),
            };
            acks = guard;
        }
    }

    /// The last acknowledged sequence number for `rid` (0 when none).
    pub fn acked(&self, rid: &str) -> u64 {
        lock(&self.ctx.shared.acks).get(rid).copied().unwrap_or(0)
    }
}

fn check_rid(rid: &str) -> Result<(), ResourceError> {
    if rid.is_empty() || rid.len() > 256 {
        return Err(ResourceError::BadRid(rid.into()));
    }
    Ok(())
}

fn sanitize(rid: &str) -> String {
    rid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
