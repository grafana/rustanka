//! Lightweight function-call profiler.
//!
//! Profiling is fully opt-in and gated behind a single relaxed atomic load, so
//! the hot path is effectively free when disabled.
//!
//! While enabled, each thread accumulates per-function call counts and timings
//! in thread-local storage (no locking on the hot path). Since interned strings
//! ([`IStr`]) are not `Send`, names are converted to `String` only when a
//! thread flushes its accumulated data into the global aggregate via
//! [`flush_thread_local`]. The aggregate can then be read with [`collect`].

use std::{
	cell::RefCell,
	sync::{
		atomic::{AtomicBool, Ordering},
		Mutex,
	},
	time::Instant,
};

use jrsonnet_interner::IStr;
use jrsonnet_parser::SourcePath;
use rustc_hash::FxHashMap;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Per-function accumulated statistics.
#[derive(Default, Clone, Copy)]
pub struct FuncStat {
	/// Number of times the function was called.
	pub count: u64,
	/// Wall-time spent in the function itself, excluding callees (nanoseconds).
	pub self_ns: u128,
	/// Inclusive wall-time spent in the function including callees
	/// (nanoseconds).
	///
	/// For recursive functions, only the outermost frame on the stack
	/// contributes, so a given slice of wall-time is counted once rather than
	/// once per recursion level.
	pub total_ns: u128,
}

/// Thread-local map key: function name plus the file it is defined in (when
/// known; builtins have none). [`SourcePath`] is not `Send`, so it only lives
/// in thread-local storage and is converted to a string at flush time.
type ThreadKey = (IStr, Option<SourcePath>);

struct Frame {
	start: Instant,
	/// Inclusive time spent in direct callees, used to derive self time.
	child_ns: u128,
	key: ThreadKey,
	/// True when no ancestor frame is the same function (recursion guard for
	/// inclusive time).
	outermost: bool,
}

#[derive(Default)]
struct ThreadProfile {
	stats: FxHashMap<ThreadKey, FuncStat>,
	stack: Vec<Frame>,
	/// Active frame count per function, to detect recursion.
	depth: FxHashMap<ThreadKey, u32>,
}

thread_local! {
	static PROFILE: RefCell<ThreadProfile> = RefCell::new(ThreadProfile::default());
}

/// Aggregate key: function name and optional defining file path.
type AggKey = (String, Option<String>);

static AGGREGATE: Mutex<Option<FxHashMap<AggKey, FuncStat>>> = Mutex::new(None);

/// Enable or disable profiling. Also resets the global aggregate when enabling.
pub fn set_enabled(enabled: bool) {
	if enabled {
		*AGGREGATE.lock().expect("profile aggregate poisoned") = Some(FxHashMap::default());
	}
	ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether profiling is currently enabled.
#[inline(always)]
pub fn is_enabled() -> bool {
	ENABLED.load(Ordering::Relaxed)
}

/// Record entry into a function call named `name`, optionally defined in
/// `file`. Must be paired with [`exit`].
#[inline]
pub fn enter(name: IStr, file: Option<SourcePath>) {
	PROFILE.with(|p| {
		let mut p = p.borrow_mut();
		let key = (name, file);
		let depth = p.depth.entry(key.clone()).or_insert(0);
		let outermost = *depth == 0;
		*depth += 1;
		p.stack.push(Frame {
			start: Instant::now(),
			child_ns: 0,
			key,
			outermost,
		});
	});
}

/// Record exit from the most recently entered function call.
#[inline]
pub fn exit() {
	PROFILE.with(|p| {
		let mut p = p.borrow_mut();
		let Some(frame) = p.stack.pop() else {
			return;
		};
		let total_ns = frame.start.elapsed().as_nanos();
		let self_ns = total_ns.saturating_sub(frame.child_ns);
		if let Some(parent) = p.stack.last_mut() {
			parent.child_ns += total_ns;
		}
		if let Some(depth) = p.depth.get_mut(&frame.key) {
			*depth = depth.saturating_sub(1);
		}
		let entry = p.stats.entry(frame.key).or_default();
		entry.count += 1;
		entry.self_ns += self_ns;
		if frame.outermost {
			entry.total_ns += total_ns;
		}
	});
}

/// Flush this thread's accumulated stats into the global aggregate and clear
/// the thread-local state. Cheap to call when profiling is disabled.
pub fn flush_thread_local() {
	PROFILE.with(|p| {
		let mut p = p.borrow_mut();
		p.stack.clear();
		p.depth.clear();
		if p.stats.is_empty() {
			return;
		}
		let mut guard = AGGREGATE.lock().expect("profile aggregate poisoned");
		if let Some(agg) = guard.as_mut() {
			for ((name, file), stat) in p.stats.drain() {
				let file = file.and_then(|p| p.path().map(|p| p.display().to_string()));
				let entry = agg.entry((name.to_string(), file)).or_default();
				entry.count += stat.count;
				entry.self_ns += stat.self_ns;
				entry.total_ns += stat.total_ns;
			}
		} else {
			p.stats.clear();
		}
	});
}

/// A single profiled function: name, optional defining file, and stats.
pub struct ProfileEntry {
	pub name: String,
	pub file: Option<String>,
	pub stat: FuncStat,
}

/// Read the merged profiling results gathered so far.
pub fn collect() -> Vec<ProfileEntry> {
	let guard = AGGREGATE.lock().expect("profile aggregate poisoned");
	guard
		.as_ref()
		.map(|agg| {
			agg.iter()
				.map(|((name, file), stat)| ProfileEntry {
					name: name.clone(),
					file: file.clone(),
					stat: *stat,
				})
				.collect()
		})
		.unwrap_or_default()
}
