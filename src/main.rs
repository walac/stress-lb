#![feature(thread_id_value)]

use affinity::*;
use clap::Parser;
use core::mem;
use duration_str;
use errno::errno;
use libc::c_void;
use scheduler::{set_self_policy, Policy};
use signal_hook::iterator::Signals;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
use std::{error::Error, ops::Drop, ptr, sync::mpsc, thread, time::Duration};
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
    pub fn new(thread_id: i32, dur: &Duration) -> Result<Self, Box<dyn Error>> {
        let mut timerid = TimerId(ptr::null_mut());
        let mut sigev: libc::sigevent = unsafe { mem::zeroed() };

        sigev.sigev_notify = libc::SIGEV_THREAD_ID;
        sigev.sigev_signo = libc::SIGALRM;
        sigev.sigev_notify_thread_id = thread_id;

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
    pub fn new(
        interval: &Duration,
        priority: u32,
        quit: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut signals = Signals::new(&[signal_hook::consts::SIGALRM])?;

        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            tx.send(unsafe { libc::gettid() }).unwrap();
            let core_mask: Vec<usize> = (0..1).collect();
            set_thread_affinity(&core_mask).unwrap();
            set_self_policy(Policy::Fifo, priority as i32).unwrap();
            for _ in signals.forever() {
                if quit.load(Ordering::Acquire) {
                    return;
                }
            }
        });

        let thread_id = rx.recv()?;
        let timer = Timer::new(thread_id, interval)?;

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

    #[clap(short, long, default_value_t = 1)]
    priority: u32,
}

fn main() {
    let args = Args::parse();

    let interval =
        duration_str::parse(&args.interval.or(Some("1ms".to_string())).unwrap()).unwrap();

    let quit = Arc::new(AtomicBool::new(false));

    let threads = run_worker_threads(quit.clone(), args.threads_per_core);

    let mut timer = TimerThread::new(&interval, args.priority, quit.clone()).unwrap();

    let dur = match args.duration {
        Some(d) => duration_str::parse(&d).unwrap(),
        None => Duration::MAX,
    };

    thread::sleep(dur);
    quit.store(true, Ordering::Release);
    timer.join().unwrap();

    for t in threads {
        t.join().unwrap();
    }
}
