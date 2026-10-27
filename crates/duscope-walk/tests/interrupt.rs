//! P1-WALK-08: the SIGINT handler wakes a waiting thread through its pipe,
//! and the stand-down wakes it too, without polling. In a binary of its own,
//! since it installs a process-wide handler and signals this process.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use duscope_testkit::TestEnv;
use duscope_walk::{cancel_on_interrupt, stand_down_interrupt, wait_for_interrupt};

#[test]
fn p1_walk_08_sigint_and_the_stand_down_wake_the_waiting_thread() {
    let env = TestEnv::new();
    let flag = Arc::new(AtomicBool::new(false));
    assert!(
        cancel_on_interrupt(&flag).unwrap(),
        "SIGINT is not ignored here"
    );
    // SIGINT: the flag is set and the waiter wakes with `true`.
    let waiter = thread::spawn(wait_for_interrupt);
    let pid = std::process::id().to_string();
    let killed = env.command("kill").args(["-INT", &pid]).status().unwrap();
    assert!(killed.success());
    assert!(waiter.join().unwrap(), "woken by SIGINT");
    assert!(flag.load(Ordering::SeqCst));
    // The stand-down: the next waiter wakes with `false`.
    let waiter = thread::spawn(wait_for_interrupt);
    stand_down_interrupt();
    assert!(!waiter.join().unwrap(), "woken by the stand-down");
    // A SIGINT no thread waited for does not wake the waiter of the next
    // installation: installing empties the pipe.
    let killed = env.command("kill").args(["-INT", &pid]).status().unwrap();
    assert!(killed.success());
    assert!(cancel_on_interrupt(&flag).unwrap());
    let waiter = thread::spawn(wait_for_interrupt);
    stand_down_interrupt();
    assert!(!waiter.join().unwrap(), "not woken by the old SIGINT");
}
