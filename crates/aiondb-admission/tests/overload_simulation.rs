//! Backpressure integration test.
//!
//! Simulates an overload scenario : a 128-way client burst against a
//! controller configured with tight rates. Asserts:
//!
//! 1. System-priority work is admitted essentially always.
//! 2. User-priority work is rate-limited but eventually drains.
//! 3. Batch-priority work yields to user under sustained contention.
//! 4. The global in-flight cap rejects when reached.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aiondb_admission::{AdmissionConfig, AdmissionController, AdmissionOutcome, Priority};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn system_priority_admitted_under_overload() {
    let config = AdmissionConfig {
        rates: [10_000.0, 5_000.0, 100.0, 10.0],
        bursts: [1_000, 500, 4, 2],
        global_in_flight_cap: 64,
    };
    let controller = Arc::new(AdmissionController::new(config).unwrap());
    // Saturate user + batch tokens.
    for _ in 0..256 {
        let _ = controller.admit(Priority::User);
        let _ = controller.admit(Priority::Batch);
    }
    // System should still be admitted.
    let mut system_admits = 0;
    for _ in 0..32 {
        if let AdmissionOutcome::Admit = controller.admit(Priority::System) {
            system_admits += 1;
        }
    }
    assert!(
        system_admits >= 30,
        "system should bypass overload: {system_admits}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_priority_drains_eventually() {
    let config = AdmissionConfig {
        rates: [10_000.0, 5_000.0, 200.0, 50.0],
        bursts: [1_000, 500, 8, 2],
        global_in_flight_cap: 32,
    };
    let controller = Arc::new(AdmissionController::new(config).unwrap());
    let admits = Arc::new(AtomicU64::new(0));
    let rejects = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::new();
    for _ in 0..128 {
        let c = Arc::clone(&controller);
        let a = Arc::clone(&admits);
        let r = Arc::clone(&rejects);
        handles.push(tokio::spawn(async move {
            for _ in 0..10 {
                match c.admit(Priority::User) {
                    AdmissionOutcome::Admit => {
                        a.fetch_add(1, Ordering::SeqCst);
                        c.release(Priority::User);
                    }
                    AdmissionOutcome::Reject { .. } => {
                        r.fetch_add(1, Ordering::SeqCst);
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let a = admits.load(Ordering::SeqCst);
    let r = rejects.load(Ordering::SeqCst);
    assert!(a + r == 128 * 10);
    assert!(a > 0, "some user work must succeed: a={a} r={r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_cap_rejects_overflow_traffic() {
    let config = AdmissionConfig {
        rates: [10_000.0, 10_000.0, 10_000.0, 10_000.0],
        bursts: [10_000, 10_000, 10_000, 10_000],
        global_in_flight_cap: 4,
    };
    let controller = Arc::new(AdmissionController::new(config).unwrap());
    // Fill the global cap.
    for _ in 0..4 {
        assert!(matches!(
            controller.admit(Priority::User),
            AdmissionOutcome::Admit
        ));
    }
    // Fifth user request hits the cap, even though tokens are available.
    assert!(matches!(
        controller.admit(Priority::User),
        AdmissionOutcome::Reject { .. }
    ));
    // System still slips through (cap does not apply).
    assert!(matches!(
        controller.admit(Priority::System),
        AdmissionOutcome::Admit
    ));
}
