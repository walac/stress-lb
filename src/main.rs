#![feature(thread_id_value)]

use affinity::*;
use clap::Parser;
use core::mem;
use errno::errno;
use libc::c_void;
use signal_hook::iterator::Signals;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
use std::{error::Error, ops::Drop, ptr, thread, time::Duration};
use volatile::Volatile;

struct TimerId(*mut c_void);

impl Drop for TimerId {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::timer_delete(self.0) };
        }
    }
}

struct Timer(TimerId);

impl Timer {
    pub fn new(thr: &thread::Thread, dur: &Duration) -> Result<Self, Box<dyn Error>> {
        let mut timerid = TimerId(ptr::null_mut());
        let mut sigev: libc::sigevent = unsafe { mem::zeroed() };

        sigev.sigev_notify = libc::SIGEV_THREAD_ID;
        sigev.sigev_signo = libc::SIGALRM;
        sigev.sigev_notify_thread_id = thr.id().as_u64().get() as i32;

        let mut ret;
        unsafe {
            ret = libc::timer_create(
                libc::CLOCK_MONOTONIC,
                &mut sigev as *mut libc::sigevent,
                &mut timerid.0 as *mut *mut libc::c_void,
            );
        }

        if ret < 0 {
            return Err(Box::new(errno()));
        }

        let mut tmspec: libc::itimerspec = unsafe { mem::zeroed() };
        tmspec.it_interval.tv_sec = dur.as_secs() as i64;
        tmspec.it_interval.tv_nsec = dur.subsec_nanos() as i64;
        tmspec.it_value.tv_sec = dur.as_secs() as i64;
        tmspec.it_value.tv_nsec = dur.subsec_nanos() as i64;

        unsafe {
            ret = libc::timer_settime(timerid.0, 0, &tmspec, ptr::null_mut());
        }

        if ret < 0 {
            Err(Box::new(errno()))
        } else {
            Ok(Timer(timerid))
        }
    }
}

struct TimerThread {
    timer: Option<Timer>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl TimerThread {
    pub fn new(interval: &Duration, quit: Arc<AtomicBool>) -> Result<Self, Box<dyn Error>> {
        let mut signals = Signals::new(&[signal_hook::consts::SIGALRM])?;

        let handle = thread::spawn(move || {
            let core_mask: Vec<usize> = (0..1).collect();
            set_thread_affinity(&core_mask).unwrap();
            for _ in signals.forever() {
                if quit.load(Ordering::Acquire) {
                    return;
                }
            }
        });

        let timer = Timer::new(&handle.thread(), interval)?;

        Ok(TimerThread {
            timer: Some(timer),
            thread_handle: Some(handle),
        })
    }

    pub fn join(&mut self) -> thread::Result<()> {
        mem::replace(&mut self.thread_handle, None).unwrap().join()
    }
}

fn run_worker_threads(
    quit: Arc<AtomicBool>,
    threads_per_core: usize,
) -> Vec<thread::JoinHandle<()>> {
    let num_threads = (get_core_num() - 1) * threads_per_core;

    (0..num_threads)
        .map(move |_| {
            let myquit = quit.clone();
            thread::spawn(move || {
                let core_mask: Vec<usize> = (1..get_core_num()).collect();
                set_thread_affinity(&core_mask).unwrap();

                let mut dummy: u64 = 0;
                let mut volatile_dummy = Volatile::new(&mut dummy);

                while !myquit.load(Ordering::Acquire) {
                    // just useless computation
                    volatile_dummy.write(volatile_dummy.read().wrapping_add(1));
                }
            })
        })
        .collect()
}

#[derive(Parser, Debug)]
#[clap(name = "stress-lb")]
#[clap(author = "Wander Lairson Costa <wcosta@redhat.com>")]
#[clap(about = "Stress the kernel scheduler load-balancer for worst case scenario")]
#[clap(long_about = None)]
struct Args {
    #[clap(short, long, default_value_t = 3)]
    threads_per_core: usize,

    #[clap(short, long)]
    duration: Option<String>,

    #[clap(short, long)]
    interval: Option<String>,
}

fn main() {
    let args = Args::parse();
}
