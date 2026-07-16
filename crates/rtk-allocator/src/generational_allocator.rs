use std::alloc::{GlobalAlloc, Layout};
use std::fmt::{Formatter, self};
use std::sync::atomic::{ AtomicBool, Ordering };
use std::sync::Mutex;
use std::time::{Duration, Instant};

use allocator_api2::boxed::Box;
use brie_tree::BTree;
use mimallocator::Mimalloc;
#[cfg(target_pointer_width = "32")]
use brie_tree::nonmax::NonMaxU32;
#[cfg(target_pointer_width = "64")]
use brie_tree::nonmax::NonMaxU64;

use crate::debug_ref_cell::DebugRefCell;
use crate::hints;
use crate::not_so_global_allocator::NotSoGlobalAllocator;

#[cfg(target_pointer_width = "32")]
type NonMaxPointer = NonMaxU32;
#[cfg(target_pointer_width = "64")]
type NonMaxPointer = NonMaxU64;

pub struct Generation {
    parent: Option<Box<DebugRefCell<Generation>, &'static NotSoGlobalAllocator<Mimalloc>>>,
    tagged: BTree<NonMaxPointer, Layout, &'static NotSoGlobalAllocator<Mimalloc>>,
    statistics: Option<GenerationStatistics>,
}

impl Generation {
    fn new_in(allocator: &'static NotSoGlobalAllocator<Mimalloc>) -> Generation {
        Generation {
            parent: None,
            tagged: BTree::new_in(allocator),
            statistics: None, 
        }
    }

    fn with_statistics(&mut self) {
        self.statistics = Some(GenerationStatistics::default());
    }

    fn with_parent(&mut self, parent: Box<DebugRefCell<Generation>, &'static NotSoGlobalAllocator<Mimalloc>>) {
        self.parent = Some(parent);
    }

    fn update_layout(&mut self, key: NonMaxPointer, layout: Layout, new_size: usize) {
        if let Some(expected_layout) = self.tagged.get_mut(key) {
            debug_assert_eq!(&*expected_layout, &layout, "attempt to reallocate with a different layout");

            let new_layout = Layout::from_size_align(new_size, expected_layout.align()).expect("attempt to reallocate with invalid layout");

            if let Some(generation_statistics) = &mut self.statistics {
                hints::unlikely();
                if layout.size() > new_layout.size() {
                    generation_statistics.bytes_freed += layout.size() - new_layout.size();
                } else {
                    generation_statistics.bytes_allocated += new_layout.size() - layout.size();
                }
            }

            *expected_layout = new_layout;
        } else if let Some(parent_generation) = &self.parent {
            let mut parent_generation = parent_generation.borrow_mut();
            parent_generation.update_layout(key, layout, new_size);
        }
    }
}

impl fmt::Debug for Generation {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Generation")
            .field("parent", &self.parent)
            .field("statistics", &self.statistics)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
struct GenerationStatistics {
    allocator_time: Duration,
    bytes_allocated: usize,
    bytes_freed: usize,
    generation_time: Duration,
}

pub struct GenerationalAllocator;

impl GenerationalAllocator {
    pub fn enable_statistics() {

    }

    pub fn with_generation<F, R>(f: F) -> (Generation, R)
    where
        F: FnOnce() -> R,
    {
        let not_so_global_allocator = NotSoGlobalAllocator::<Mimalloc>::get();
        let global_state = GenerationalAllocatorGlobalState::get();
        let collect_statistics = global_state.collect_statistics.load(Ordering::SeqCst);
        let start = collect_statistics.then(|| Instant::now());
        
        let (mut generation, output) = GenerationalAllocatorThreadState::with(|thread_state| {
            let current_generation =
                Some(Box::new_in(DebugRefCell::new(Generation::new_in(not_so_global_allocator)), not_so_global_allocator));
            let parent_generation = thread_state.generation.replace(current_generation);

            if let Some(current_generation) = thread_state.generation.borrow().as_ref() {
                let mut current_generation = current_generation.borrow_mut();
                
                if let Some(parent_generation) = parent_generation {
                    current_generation.with_parent(parent_generation);
                }
                if collect_statistics {
                    current_generation.with_statistics();
                }
            }

            let output = f();

            let Some(current_generation) = thread_state.generation.replace(None) else {
                panic!("allocator state corruption: generation was removed without being returned");
            };
            let mut current_generation = Box::into_inner(current_generation).into_inner();
            
            thread_state.generation.replace(current_generation.parent);
            current_generation.parent = None;

            (current_generation, output)
        });

        if let Some(start) = start {
            let elapsed = start.elapsed();

            let mut global_statistics =
                global_state.statistics.lock().expect("mutext should not be poisoned");
            
            match (global_statistics.as_mut(), generation.statistics.as_mut()) {
                (Some(global_statistics), Some(generation_statistics)) => {
                    generation_statistics.generation_time = elapsed;
                    global_statistics.merge(generation_statistics);
                },
                _ => (),
            }
        }

        (generation, output)
    }

    pub fn without_generation<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        GenerationalAllocatorThreadState::with(|thread_state| {
            let parent_generation = thread_state.generation.replace(None);
            let output = f();
            thread_state.generation.replace(parent_generation);
            output
        })
    }
}

unsafe impl GlobalAlloc for GenerationalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let global_state = GenerationalAllocatorGlobalState::get();
        let collect_statistics = global_state.collect_statistics.load(Ordering::SeqCst);
        let start = collect_statistics.then(|| Instant::now());

        GenerationalAllocatorThreadState::with(|thread_state| {
            if let Some(generation) = thread_state.generation.borrow().as_ref() {
                // SAFETY: we are allocating memory per the languages request
                let ptr = unsafe { Mimalloc.alloc(layout) };

                // SAFETY: if mimalloc gives us a pointer that is equal to
                // usize::MAX, we have bigger problems than our generation
                // tracker becoming corrupt 
                #[cfg(target_pointer_width = "32")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u32) };
                #[cfg(target_pointer_width = "64")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u64) };

                let mut generation = generation.borrow_mut();
                
                generation.tagged.insert(key, layout);

                match (&mut generation.statistics, start) {
                    (Some(generation_statistics), Some(start)) => {
                        hints::unlikely();
                        
                        generation_statistics.bytes_allocated += layout.size();
                        
                        let elapsed = start.elapsed();
                        generation_statistics.allocator_time += elapsed;
                    },
                    _ => (),
                }

                ptr
            } else {
                // SAFETY: we are allocating memory per the languages request
                unsafe { Mimalloc.alloc(layout) }
            }
        })
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let global_state = GenerationalAllocatorGlobalState::get();
        let collect_statistics = global_state.collect_statistics.load(Ordering::SeqCst);
        let start = collect_statistics.then(|| Instant::now());

        GenerationalAllocatorThreadState::with(|thread_state| {
            if let Some(generation) = thread_state.generation.borrow().as_ref() {
                // SAFETY: we are allocating memory per the languages request
                let ptr = unsafe { Mimalloc.alloc_zeroed(layout) };

                // SAFETY: if mimalloc gives us a pointer that is equal to
                // usize::MAX, we have bigger problems than our generation
                // tracker becoming corrupt 
                #[cfg(target_pointer_width = "32")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u32) };
                #[cfg(target_pointer_width = "64")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u64) };

                let mut generation = generation.borrow_mut();
                
                generation.tagged.insert(key, layout);
                
                match (&mut generation.statistics, start) {
                    (Some(generation_statistics), Some(start)) => {
                        hints::unlikely();
                        
                        generation_statistics.bytes_allocated += layout.size();
                        
                        let elapsed = start.elapsed();
                        generation_statistics.allocator_time += elapsed;
                    },
                    _ => (),
                }

                ptr
            } else {
                // SAFETY: we are allocating memory per the languages request
                unsafe { Mimalloc.alloc_zeroed(layout) }
            }
        })
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let global_state = GenerationalAllocatorGlobalState::get();
        let collect_statistics = global_state.collect_statistics.load(Ordering::SeqCst);
        let start = collect_statistics.then(|| Instant::now());

        GenerationalAllocatorThreadState::with(|thread_state| {
            if let Some(generation) = thread_state.generation.borrow().as_ref() {
                // SAFETY: we are allocating memory per the languages request
                let ptr = unsafe { Mimalloc.realloc(ptr, layout, new_size) };

                // SAFETY: if mimalloc gives us a pointer that is equal to
                // usize::MAX, we have bigger problems than our generation
                // tracker becoming corrupt 
                #[cfg(target_pointer_width = "32")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u32) };
                #[cfg(target_pointer_width = "64")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u64) };

                let mut generation = generation.borrow_mut();
                
                generation.update_layout(key, layout, new_size);
                
                match (&mut generation.statistics, start) {
                    (Some(generation_statistics), Some(start)) => {
                        hints::unlikely();
                        
                        generation_statistics.bytes_allocated += layout.size();
                        
                        let elapsed = start.elapsed();
                        generation_statistics.allocator_time += elapsed;
                    },
                    _ => (),
                }
                
                ptr
            } else {
                // SAFETY: we are allocating memory per the languages request
                unsafe { Mimalloc.realloc(ptr, layout, new_size) }
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let global_state = GenerationalAllocatorGlobalState::get();
        let collect_statistics = global_state.collect_statistics.load(Ordering::SeqCst);
        let start = collect_statistics.then(|| Instant::now());

        GenerationalAllocatorThreadState::with(|thread_state| {
            if let Some(generation) = thread_state.generation.borrow().as_ref() {
                // SAFETY: if mimalloc gives us a pointer that is equal to
                // usize::MAX, we have bigger problems than our generation
                // tracker becoming corrupt 
                #[cfg(target_pointer_width = "32")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u32) };
                #[cfg(target_pointer_width = "64")]
                let key = unsafe { NonMaxPointer::new_unchecked(ptr.addr() as u64) };

                let mut generation = generation.borrow_mut();
                
                generation.tagged.remove(key);

                // SAFETY: we are deallocating memory per the languages request
                unsafe { Mimalloc.dealloc(ptr, layout) }
                
                match (&mut generation.statistics, start) {
                    (Some(generation_statistics), Some(start)) => {
                        hints::unlikely();
                        
                        generation_statistics.bytes_freed += layout.size();
                        
                        let elapsed = start.elapsed();
                        generation_statistics.allocator_time += elapsed;
                    },
                    _ => (),
                }
            } else {
                // SAFETY: we are deallocating memory per the languages request
                unsafe { Mimalloc.dealloc(ptr, layout) }
            }
        })
    }
}

#[derive(Debug, Default)]
struct GenerationalAllocatorGlobalState {
    collect_statistics: AtomicBool,
    statistics: Mutex<Option<GenerationalAllocatorStatistics>>,
}

impl GenerationalAllocatorGlobalState {
    fn get() -> &'static GenerationalAllocatorGlobalState {
        static GLOBAL_STATE: GenerationalAllocatorGlobalState = GenerationalAllocatorGlobalState {
            collect_statistics: AtomicBool::new(false),
            statistics: Mutex::new(None),
        };
        &GLOBAL_STATE
    }
}


#[derive(Clone, Debug, Default)]
struct GenerationalAllocatorStatistics {
    allocator_time_total: Duration,
    bytes_allocated_total: usize,
    bytes_freed_total: usize,
    generations_ran: usize,
    generation_time_total: Duration,
}

impl GenerationalAllocatorStatistics {
    pub fn merge(&mut self, generation_statistics: &GenerationStatistics) {
        self.allocator_time_total += generation_statistics.allocator_time;
        self.bytes_allocated_total += generation_statistics.bytes_allocated;
        self.bytes_freed_total += generation_statistics.bytes_freed;
        self.generations_ran += 1;
        self.generation_time_total += generation_statistics.generation_time;
    }
}

#[derive(Debug, Default)]
struct GenerationalAllocatorThreadState {
    generation: DebugRefCell<Option<Box<DebugRefCell<Generation>, &'static NotSoGlobalAllocator<Mimalloc>>>>,
}

impl GenerationalAllocatorThreadState {
    fn with<F, R>(f: F) -> R
    where
        F: FnOnce(&GenerationalAllocatorThreadState) -> R,
    {
        thread_local! {
            static THREAD_STATE: GenerationalAllocatorThreadState = GenerationalAllocatorThreadState::default();
        }
        THREAD_STATE.with(f)
    }
}
