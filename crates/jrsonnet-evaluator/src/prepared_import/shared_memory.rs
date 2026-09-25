use std::{
	cell::UnsafeCell,
	env,
	ffi::CString,
	mem::size_of,
	os::fd::RawFd,
	ptr::NonNull,
	sync::{
		LazyLock,
		atomic::{AtomicU32, AtomicU64, Ordering},
	},
};

use bincode::Options as _;
use jrsonnet_ir::{Source, SourceFile, SourcePath, SourceVirtual, with_span_source};
use sha2::{Digest as _, Sha256};

use crate::{
	IStr,
	analyze::{LExpr, LocalId},
};

#[cfg(target_os = "macos")]
const CAPACITY: usize = 3 * 1024 * 1024;
#[cfg(not(target_os = "macos"))]
const CAPACITY: usize = 32 * 1024 * 1024;
const SLOTS: usize = 4096;
#[cfg(target_os = "macos")]
const MAX_ENTRY: usize = 2 * 1024 * 1024;
#[cfg(not(target_os = "macos"))]
const MAX_ENTRY: usize = 8 * 1024 * 1024;
const MAGIC: u64 = 0x52544b5f50495231;
const EMPTY: u32 = 0;
const WRITING: u32 = 1;
const READY: u32 = 2;
static TRACE: LazyLock<bool> =
	LazyLock::new(|| env::var_os("RTK_PREPARED_IMPORT_SHM_TRACE").is_some());

#[repr(C)]
struct Slot {
	state: AtomicU32,
	data: UnsafeCell<SlotData>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SlotData {
	key: [u8; 32],
	offset: u32,
	len: u32,
	checksum: [u8; 32],
}

#[repr(C)]
struct Header {
	magic: AtomicU64,
	build_id: UnsafeCell<[u8; 32]>,
	cursor: AtomicU64,
	slots: [Slot; SLOTS],
}

pub(super) struct SharedMemoryCache {
	base: NonNull<u8>,
}

impl Drop for SharedMemoryCache {
	fn drop(&mut self) {
		// SAFETY: This mapping was created by mmap with exactly CAPACITY bytes.
		unsafe {
			libc::munmap(self.base.as_ptr().cast(), CAPACITY);
		}
	}
}

impl SharedMemoryCache {
	pub(super) fn open() -> Option<Self> {
		if env::var_os("RTK_PREPARED_IMPORT_SHM_DISABLE").is_some() {
			return None;
		}
		let namespace = env::var("RTK_PREPARED_IMPORT_SHM")
			.ok()
			.filter(|value| !value.is_empty())
			.unwrap_or_else(|| "default".to_owned());
		Self::open_named(&namespace)
	}

	pub(super) fn open_named(namespace: &str) -> Option<Self> {
		if namespace.is_empty() {
			return None;
		}
		let name = Self::name(namespace)?;
		for _ in 0..2 {
			// SAFETY: name is a valid, NUL-terminated POSIX shared-memory name.
			let mut fd = unsafe {
				libc::shm_open(
					name.as_ptr(),
					libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
					0o600,
				)
			};
			let created = fd >= 0;
			if !created {
				// SAFETY: name remains alive for the call.
				fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0o600) };
				if fd < 0 {
					return None;
				}
			}
			let mapped = Self::map(fd, created);
			// SAFETY: fd came from shm_open and is no longer needed after mmap.
			unsafe {
				libc::close(fd);
			}
			if let Some(cache) = mapped {
				return Some(cache);
			}
			// SAFETY: stale mappings stay valid until their processes release them.
			unsafe {
				libc::shm_unlink(name.as_ptr());
			}
		}
		None
	}

	fn name(namespace: &str) -> Option<CString> {
		let mut hash = Sha256::new();
		hash.update(namespace);
		// SAFETY: getuid takes no pointers and has no preconditions.
		hash.update(unsafe { libc::getuid() }.to_le_bytes());
		let digest = hash.finalize();
		CString::new(format!("/rtk-pi-{}", hex_prefix(&digest[..10]))).ok()
	}

	#[cfg(test)]
	pub(super) fn unlink_named(namespace: &str) {
		if let Some(name) = Self::name(namespace) {
			// SAFETY: name is valid and unlinking keeps existing mappings alive.
			unsafe {
				libc::shm_unlink(name.as_ptr());
			}
		}
	}

	fn map(fd: RawFd, created: bool) -> Option<Self> {
		if created {
			// SAFETY: fd is a writable shared-memory descriptor.
			if unsafe { libc::ftruncate(fd, CAPACITY as libc::off_t) } != 0 {
				return None;
			}
		} else {
			let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
			// SAFETY: stat points to writable memory for one libc::stat.
			if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
				return None;
			}
			// SAFETY: fstat succeeded and initialized stat.
			if unsafe { stat.assume_init().st_size } != CAPACITY as libc::off_t {
				return None;
			}
		}
		// SAFETY: fd is valid and sized to CAPACITY. The mapping is removed in Drop.
		let ptr = unsafe {
			libc::mmap(
				std::ptr::null_mut(),
				CAPACITY,
				libc::PROT_READ | libc::PROT_WRITE,
				libc::MAP_SHARED,
				fd,
				0,
			)
		};
		if ptr == libc::MAP_FAILED {
			return None;
		}
		let cache = Self {
			base: NonNull::new(ptr.cast())?,
		};
		if created {
			// SAFETY: this process created the zeroed segment and has not published magic yet.
			unsafe {
				*cache.header().build_id.get() = Self::build_id();
			}
			cache
				.header()
				.cursor
				.store(size_of::<Header>() as u64, Ordering::Relaxed);
			cache.header().magic.store(MAGIC, Ordering::Release);
		} else if cache.header().magic.load(Ordering::Acquire) != MAGIC
			// SAFETY: magic publishes the initialized build ID.
			|| unsafe { *cache.header().build_id.get() } != Self::build_id()
		{
			return None;
		}
		Some(cache)
	}

	fn build_id() -> [u8; 32] {
		Sha256::digest(env!("JRSONNET_PREPARED_CACHE_BUILD_ID")).into()
	}

	fn header(&self) -> &Header {
		// SAFETY: mmap returns page-aligned memory, and the segment has at least Header bytes.
		unsafe { &*self.base.as_ptr().cast::<Header>() }
	}

	pub(super) fn get(
		&self,
		path: &SourcePath,
		code: &IStr,
		externals: &[(IStr, LocalId)],
		source: Source,
	) -> Option<LExpr> {
		let key = key(path, code, externals)?;
		let start = u32::from_le_bytes(key[..4].try_into().ok()?) as usize % SLOTS;
		for distance in 0..SLOTS {
			let slot = &self.header().slots[(start + distance) % SLOTS];
			match slot.state.load(Ordering::Acquire) {
				EMPTY => return None,
				READY => {
					// SAFETY: READY publishes a complete SlotData and writers never change it again.
					let data = unsafe { *slot.data.get() };
					if data.key != key {
						continue;
					}
					let offset = data.offset as usize;
					let len = data.len as usize;
					if offset < size_of::<Header>()
						|| len > MAX_ENTRY || offset.checked_add(len)? > CAPACITY
					{
						continue;
					}
					// SAFETY: bounds are checked and READY publishes the bytes before this read.
					let bytes =
						unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(offset), len) };
					if Sha256::digest(bytes)[..] != data.checksum {
						continue;
					}
					if let Ok(lir) = with_span_source(source.clone(), || {
						bincode::options()
							.with_limit(MAX_ENTRY as u64)
							.deserialize(bytes)
					}) {
						if *TRACE {
							eprintln!("prepared-import shared hit: {len} bytes");
						}
						return Some(lir);
					}
				}
				_ => {}
			}
		}
		None
	}

	pub(super) fn insert(
		&self,
		path: &SourcePath,
		code: &IStr,
		externals: &[(IStr, LocalId)],
		source: Source,
		lir: &LExpr,
	) {
		let Some(key) = key(path, code, externals) else {
			return;
		};
		// A full segment cannot accept another entry; avoid serializing it repeatedly.
		if self.header().cursor.load(Ordering::Acquire) >= CAPACITY as u64 {
			return;
		}
		let bytes = match with_span_source(source, || bincode::options().serialize(lir)) {
			Ok(bytes) => bytes,
			Err(error) => {
				if *TRACE {
					eprintln!("prepared-import shared encode skipped: {error}");
				}
				return;
			}
		};
		if bytes.len() > MAX_ENTRY {
			if *TRACE {
				eprintln!(
					"prepared-import shared entry too large: {} bytes",
					bytes.len()
				);
			}
			return;
		}
		let start = u32::from_le_bytes(key[..4].try_into().unwrap()) as usize % SLOTS;
		for distance in 0..SLOTS {
			let slot = &self.header().slots[(start + distance) % SLOTS];
			if slot.state.load(Ordering::Acquire) == READY {
				// SAFETY: READY publishes immutable SlotData.
				if unsafe { (*slot.data.get()).key } == key {
					return;
				}
			}
			if slot
				.state
				.compare_exchange(EMPTY, WRITING, Ordering::AcqRel, Ordering::Acquire)
				.is_err()
			{
				continue;
			}
			let offset = self
				.header()
				.cursor
				.fetch_add(bytes.len() as u64, Ordering::AcqRel) as usize;
			if offset
				.checked_add(bytes.len())
				.is_none_or(|end| end > CAPACITY)
			{
				if *TRACE {
					eprintln!("prepared-import shared segment full");
				}
				return;
			}
			// SAFETY: fetch_add gives this writer an exclusive in-bounds byte range.
			unsafe {
				std::ptr::copy_nonoverlapping(
					bytes.as_ptr(),
					self.base.as_ptr().add(offset),
					bytes.len(),
				);
			}
			// SAFETY: this writer alone owns the slot until READY is published.
			unsafe {
				*slot.data.get() = SlotData {
					key,
					offset: offset as u32,
					len: bytes.len() as u32,
					checksum: Sha256::digest(&bytes).into(),
				};
			}
			slot.state.store(READY, Ordering::Release);
			if *TRACE {
				eprintln!("prepared-import shared insert: {} bytes", bytes.len());
			}
			return;
		}
	}
}

fn key(path: &SourcePath, code: &IStr, externals: &[(IStr, LocalId)]) -> Option<[u8; 32]> {
	let mut hash = Sha256::new();
	if let Some(file) = path.downcast_ref::<SourceFile>() {
		hash.update(b"file:");
		hash.update(file.path().to_str()?.as_bytes());
	} else if let Some(virtual_path) = path.downcast_ref::<SourceVirtual>() {
		hash.update(b"virtual:");
		hash.update(virtual_path.0.as_bytes());
	} else {
		return None;
	}
	hash.update((code.len() as u64).to_le_bytes());
	hash.update(code.as_bytes());
	for (name, id) in externals {
		hash.update((name.len() as u64).to_le_bytes());
		hash.update(name.as_bytes());
		hash.update(id.0.to_le_bytes());
	}
	Some(hash.finalize().into())
}

fn hex_prefix(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
