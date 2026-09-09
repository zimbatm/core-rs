//! Authenticated evidence that an immutable segment passed footer validation.
use std::{collections::BTreeMap, fs::File, io, os::fd::AsRawFd};

use super::{Error, SegmentSnapshot, corrupt};

/// The fs-verity SHA-256 digest of a segment whose footer was fully validated.
/// This proves index integrity, not payload validity, reachability, or completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedIndexDigest([u8; 32]);

impl ValidatedIndexDigest {
    /// Restores a digest from an authenticated index-validation checkpoint.
    ///
    /// The caller must verify the checkpoint's signature, purpose, and segment-ID
    /// bindings first. Never construct this from an untrusted digest or a digest
    /// obtained by measuring a file without validating its footer.
    pub fn from_authenticated_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(super) fn verify(&self, file: &File) -> Result<(), Error> {
        if measure(file)? != self.0 {
            return Err(corrupt("validated index digest mismatch"));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn measure(file: &File) -> Result<[u8; 32], Error> {
    #[repr(C)]
    struct Digest {
        algorithm: u16,
        size: u16,
        bytes: [u8; 32],
    }
    let mut digest = Digest {
        algorithm: 0,
        size: 32,
        bytes: [0; 32],
    };
    // Linux encodes only the flexible-array header in FS_IOC_MEASURE_VERITY.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0xc0046686 as libc::c_ulong, &mut digest) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if (digest.algorithm, digest.size) != (1, 32) {
        return Err(corrupt("unsupported index verity digest"));
    }
    Ok(digest.bytes)
}

#[cfg(not(target_os = "linux"))]
fn measure(_file: &File) -> Result<[u8; 32], Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fs-verity index proofs require Linux",
    )
    .into())
}

impl SegmentSnapshot {
    /// Validates every captured footer after filesystem immutability is enabled.
    ///
    /// The caller must enable fs-verity on the captured handles first.
    /// Checking the footer afterward binds successful validation to immutable
    /// bytes, including changes made before sealing. This does not enable verity.
    pub fn validated_index_digests(&self) -> Result<BTreeMap<u64, ValidatedIndexDigest>, Error> {
        let mut result = BTreeMap::new();
        for ((id, file), segment) in self.files().zip(&self.segments) {
            let digest = measure(file)?;
            super::footer::parse_footer(&segment.mm)?;
            result.insert(id, ValidatedIndexDigest(digest));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_file_cannot_restore_index_proof() {
        let file = tempfile::tempfile().unwrap();
        assert!(
            ValidatedIndexDigest::from_authenticated_bytes([0; 32])
                .verify(&file)
                .is_err()
        );
    }
}
