use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ahash::HashSet;
use alkahest_core::job::SCHEDULER;
use parking_lot::{Mutex, MutexGuard};

use crate::{
    Gpu,
    gpu::{command_list::CommandList, state::GpuState},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandListSetId(usize);

struct CommandListSet {
    command_lists: Vec<Mutex<CommandList>>,
}

pub struct CommandListPool {
    sets: Vec<CommandListSet>,
    next_set: AtomicUsize,
    sets_in_use: Mutex<HashSet<usize>>,
}

pub struct CommandListLease {
    pool: Arc<CommandListPool>,
    id: CommandListSetId,
    state: AtomicU8,
}

impl CommandListLease {
    pub fn id(&self) -> CommandListSetId {
        self.id
    }

    pub fn finish(&self, cmd: &mut CommandList) {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .expect("command-list lease finished more than once");
        self.pool.execute_set(cmd, self.id);
        self.pool.release_set(self.id);
        self.state.store(2, Ordering::Release);
    }
}

impl Drop for CommandListLease {
    fn drop(&mut self) {
        if *self.state.get_mut() != 2 {
            self.pool.release_set(self.id);
        }
    }
}

impl CommandListPool {
    const NUM_SETS: usize = 12;

    pub fn new(gpu: &Arc<Gpu>) -> Self {
        let worker_count = SCHEDULER.num_workers().max(1);

        let mut sets = Vec::with_capacity(Self::NUM_SETS);
        for _ in 0..Self::NUM_SETS {
            let command_lists = (0..worker_count)
                .map(|_| Mutex::new(gpu.create_command_list()))
                .collect::<Vec<_>>();
            sets.push(CommandListSet { command_lists });
        }

        Self {
            sets,
            next_set: AtomicUsize::new(0),
            sets_in_use: Mutex::new(HashSet::default()),
        }
    }

    /// Get a unique index for the current thread.
    fn thread_idx() -> usize {
        static IDX_COUNTER: AtomicUsize = AtomicUsize::new(0);
        thread_local! {
            static THREAD_IDX: usize = IDX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        THREAD_IDX.with(|idx| *idx)
    }

    #[profiling::function]
    pub fn get_command_list_manual(
        &self,
        set: CommandListSetId,
        index: usize,
    ) -> Option<MutexGuard<'_, CommandList>> {
        self.sets
            .get(set.0)?
            .command_lists
            .get(index)
            .map(Mutex::lock)
    }

    #[profiling::function]
    pub fn get_command_list(&self, set: CommandListSetId) -> MutexGuard<'_, CommandList> {
        let set = &self.sets[set.0];
        let idx = Self::thread_idx() % set.command_lists.len();
        set.command_lists[idx].lock()
    }

    fn acquire_set(&self) -> CommandListSetId {
        let mut sets_in_use = self.sets_in_use.lock();
        for _ in 0..self.sets.len() {
            let set_idx = self.next_set.fetch_add(1, Ordering::Relaxed) % self.sets.len();
            if sets_in_use.insert(set_idx) {
                return CommandListSetId(set_idx);
            }
        }

        panic!(
            "All command list sets are in use! Increase NUM_SETS in CommandListPool (currently \
             {}).",
            Self::NUM_SETS
        );
    }

    fn release_set(&self, set: CommandListSetId) {
        assert!(
            self.sets_in_use.lock().remove(&set.0),
            "command-list set {} released more than once",
            set.0
        );
    }

    /// Copies the initial state into an exclusively leased command-list set.
    #[profiling::function]
    pub fn begin(self: &Arc<Self>, cmd: &mut CommandList) -> CommandListLease {
        let initial_state = GpuState::backup(cmd);
        let lease = CommandListLease {
            pool: Arc::clone(self),
            id: self.acquire_set(),
            state: AtomicU8::new(0),
        };

        let set = &self.sets[lease.id.0];
        for cell in &set.command_lists {
            let mut worker_cmd = cell.lock();
            initial_state.restore(&mut worker_cmd);
            worker_cmd.flush_states();
        }

        lease
    }

    /// Execute the finalized command lists onto the given command list.
    #[profiling::function]
    fn execute_set(&self, cmd: &mut CommandList, set_id: CommandListSetId) {
        let set = &self.sets[set_id.0];
        for cell in &set.command_lists {
            let worker_cmd = cell.lock();
            let finished_cmd = worker_cmd
                .finish_command_list(false)
                .expect("Failed to finalize command list");

            cmd.execute_command_list(&finished_cmd, true);
        }
    }
}

pub fn parallel_iter<T>(slice: &mut [T], func: impl Fn(&mut T) + Send + Sync + 'static)
where
    T: Send + 'static,
{
    struct JobContext<T: Send> {
        chunk: *mut T,
        func: *const dyn Fn(&mut T),
    }

    unsafe impl<T: Send> Send for JobContext<T> {}

    let mut job_handles = Vec::with_capacity(slice.len());
    for item in slice.iter_mut() {
        let context = JobContext {
            chunk: item as *mut T,
            func: &raw const func as *const dyn Fn(&mut T),
        };

        let job_handle = SCHEDULER.job_builder("parallel_iter_chunk").spawn(move || {
            let context = &context;
            let chunk = unsafe { &mut *context.chunk };
            let func = unsafe { &*context.func };
            func(chunk);
        });

        job_handles.push(job_handle);
    }

    let sync_job = SCHEDULER
        .job_builder("parallel_iter_sync")
        .dependencies(job_handles)
        .spawn(|| {});

    sync_job.wait();
}

// pub fn parallel_iter<T>(slice: &mut [T], func: impl Fn(&mut [T]) + Send + Sync)
// where
//     T: Send + 'static,
// {
//     let num_workers = SCHEDULER.num_workers();
//     let chunk_size = slice.len().div_ceil(num_workers);

//     struct JobContext<T: Send> {
//         chunk: *mut [T],
//         func: *const dyn Fn(&mut [T]),
//     }

//     unsafe impl<T: Send> Send for JobContext<T> {}

//     let mut job_handles = Vec::with_capacity(num_workers);
//     for chunk in slice.chunks_mut(chunk_size) {
//         let context = JobContext {
//             chunk: chunk as *mut [T],
//             func: &raw const func as *const dyn Fn(&mut [T]),
//         };

//         let job_handle = SCHEDULER.job_builder("parallel_iter_chunk").spawn(move || {
//             let context = &context;
//             let chunk = unsafe { &mut *context.chunk };
//             let func = unsafe { &*context.func };
//             func(chunk);
//         });

//         job_handles.push(job_handle);
//     }

//     let sync_job = SCHEDULER
//         .job_builder("parallel_iter_sync")
//         .dependencies(job_handles)
//         .spawn(|| {});

//     sync_job.wait();
// }
