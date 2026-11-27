//! The plain records shared by several entry points, laid out for C.

use crate::error::Error;

/// UTF-8 text. Text the managed caller lends lasts for the call; text this
/// library returns lasts until the record that holds it is released.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeText {
    /// The first byte, which may be null when `length` is zero.
    pub data: *const u8,
    /// Length in bytes.
    pub length: usize,
}

impl NativeText {
    /// Text that borrows `text`, for as long as `text` lives.
    pub(crate) fn borrowed(text: &str) -> Self {
        Self {
            data: text.as_ptr(),
            length: text.len(),
        }
    }

    /// Text that owns `text` until `release` is called.
    pub(crate) fn owned(text: String) -> Self {
        let text = Box::leak(text.into_boxed_str());
        Self::borrowed(text)
    }

    /// Releases text made by `owned`.
    ///
    /// # Safety
    /// `self` came from `owned` and is released once.
    pub(crate) unsafe fn release(self) {
        // SAFETY: `owned` leaked a boxed string of exactly this pointer and length.
        drop(unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                self.data.cast_mut(),
                self.length,
            ))
        });
    }

    /// The text the caller lends.
    ///
    /// # Safety
    /// A nonempty text points to readable memory of its length for `'a`.
    pub(crate) unsafe fn read<'a>(self) -> Result<&'a str, Error> {
        // SAFETY: forwarded from the caller.
        std::str::from_utf8(unsafe { lent(self.data, self.length)? }).map_err(|_| Error::Text)
    }

    /// The text the caller lends, or none when it is empty.
    ///
    /// # Safety
    /// As for `read`.
    pub(crate) unsafe fn optional<'a>(self) -> Result<Option<&'a str>, Error> {
        // SAFETY: forwarded from the caller.
        Ok(Some(unsafe { self.read()? }).filter(|text| !text.is_empty()))
    }
}

/// One named value: an application setting or an environment variable.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeSetting {
    pub name: NativeText,
    pub value: NativeText,
}

/// The named values the caller lends, by name.
///
/// # Safety
/// As for `lent`, and each setting's text as for `NativeText::read`.
pub(crate) unsafe fn settings(
    data: *const NativeSetting,
    length: usize,
) -> Result<std::collections::BTreeMap<String, String>, Error> {
    // SAFETY: forwarded from the caller.
    unsafe { lent(data, length)? }
        .iter()
        // SAFETY: forwarded from the caller.
        .map(|setting| unsafe { Ok((setting.name.read()?.into(), setting.value.read()?.into())) })
        .collect()
}

/// The manifest an application runs under locally.
#[repr(C)]
pub struct NativeManifest {
    pub name: NativeText,
    pub limits: LimitOverrides,
}

impl NativeManifest {
    /// # Safety
    /// As for `NativeText::read`.
    pub(crate) unsafe fn read(&self) -> Result<isoplane_abi::config::AppManifest, Error> {
        let limits = self.limits;
        Ok(isoplane_abi::config::AppManifest {
            // SAFETY: forwarded from the caller.
            name: unsafe { self.name.read()? }.into(),
            bindings: Default::default(),
            guest_memory_bytes: set(limits.guest_memory_bytes)
                .map(isoplane_abi::config::GuestMemoryBytes::new)
                .transpose()?,
            resource_limits: isoplane_abi::config::ResourceLimitOverrides {
                occupancy_ms_per_dispatch: set(limits.occupancy_ms_per_dispatch),
                instance_open_connections: set(limits.instance_open_connections),
                pending_futures_per_dispatch: set(limits.pending_futures_per_dispatch),
                ..Default::default()
            },
            validation_limits: Default::default(),
        })
    }
}

fn set<T: Default + PartialEq>(limit: T) -> Option<T> {
    (limit != T::default()).then_some(limit)
}

/// The limits that replace a local application's defaults. Zero keeps a
/// limit's default.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LimitOverrides {
    /// The instance soft memory limit in bytes, a multiple of the wasm page size.
    pub guest_memory_bytes: u64,
    /// The running time one request may use.
    pub occupancy_ms_per_dispatch: u32,
    /// The most outbound connections one instance may hold at once.
    pub instance_open_connections: u64,
    /// The most asynchronous operations one dispatch may have pending at once.
    pub pending_futures_per_dispatch: u32,
}

/// The `length` records at `data` the caller lends.
///
/// # Safety
/// A nonempty buffer points to `length` readable records that stay unchanged for `'a`.
pub(crate) unsafe fn lent<'a, T>(data: *const T, length: usize) -> Result<&'a [T], Error> {
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() || length > isize::MAX as usize / std::mem::size_of::<T>().max(1) {
        return Err(Error::Buffer);
    }
    // SAFETY: the caller guarantees readable, unchanged memory; the null and
    // addressable-length cases are checked above.
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

/// Moves `records` into an allocation `release` frees.
pub(crate) fn owned<T>(records: Vec<T>) -> (*mut T, usize) {
    let records = Box::leak(records.into_boxed_slice());
    (records.as_mut_ptr(), records.len())
}

/// Frees records made by `owned`.
///
/// # Safety
/// The pointer and length came from `owned` and are released once.
pub(crate) unsafe fn release<T>(data: *mut T, length: usize) -> Vec<T> {
    // SAFETY: `owned` leaked a boxed slice of exactly this pointer and length.
    unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(data, length)) }.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_buffer_descriptors_are_rejected_without_reading_memory() {
        for (pointer, length) in [
            (std::ptr::null(), 1),
            (
                std::ptr::NonNull::<u8>::dangling().as_ptr().cast_const(),
                usize::MAX,
            ),
        ] {
            // SAFETY: both descriptors fail the boundary checks before dereferencing.
            let result = unsafe { lent(pointer, length) };
            assert!(
                matches!(result, Err(Error::Buffer)),
                "unreadable descriptors must return a managed input error"
            );
        }
    }

    #[test]
    fn a_limit_left_at_zero_keeps_its_default() {
        let manifest = NativeManifest {
            name: NativeText::borrowed("app"),
            limits: LimitOverrides {
                occupancy_ms_per_dispatch: 17,
                ..Default::default()
            },
        };
        // SAFETY: the name borrows a static string.
        let manifest = unsafe { manifest.read() }.expect("a manifest");
        assert_eq!(
            (
                manifest.guest_memory_bytes.is_none(),
                manifest.resource_limits.occupancy_ms_per_dispatch,
                manifest.resource_limits.instance_open_connections,
            ),
            (true, Some(17), None),
            "only a nonzero limit replaces its default"
        );
    }
}
