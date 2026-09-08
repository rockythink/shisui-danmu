use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

struct Controlled {
    calls: AtomicUsize,
    entered: Semaphore,
    echoes: Semaphore,
    fail_at: Option<usize>,
}
impl Transport for Controlled {
    async fn send_confirm(&self, _: &str) -> Outcome {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        if self.fail_at == Some(index) {
            return Outcome::Uncertain;
        }
        self.echoes.acquire().await.unwrap().forget();
        Outcome::Confirmed
    }
}
#[tokio::test]
async fn next_segment_and_other_source_wait_for_echo_then_stop_on_uncertainty() {
    let queue = SendQueue::default();
    let transport = Arc::new(Controlled {
        calls: AtomicUsize::new(0),
        entered: Semaphore::new(0),
        echoes: Semaphore::new(0),
        fail_at: Some(1),
    });
    let task_queue = queue.clone();
    let task_transport = transport.clone();
    let task = tokio::spawn(async move {
        task_queue
            .send(
                &*task_transport,
                &["第一段".into(), "第二段".into(), "不得发送第三段".into()],
                None,
            )
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    let other_queue = queue.clone();
    let other_transport = transport.clone();
    let manual = tokio::spawn(async move {
        other_queue
            .send(&*other_transport, &["人工消息".into()], None)
            .await
    });
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            transport.entered.acquire()
        )
        .await
        .is_err()
    );
    transport.echoes.add_permits(1);
    assert_eq!(task.await.unwrap(), Outcome::Uncertain);
    assert_eq!(manual.await.unwrap(), Outcome::Cancelled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert!(queue.is_paused());
}
#[tokio::test]
async fn pause_or_expired_candidate_drops_all_unsent_segments_without_retry() {
    let queue = SendQueue::default();
    let transport = Arc::new(Controlled {
        calls: AtomicUsize::new(0),
        entered: Semaphore::new(0),
        echoes: Semaphore::new(0),
        fail_at: None,
    });
    let permit = Permit::new(Utc::now() + chrono::Duration::seconds(30));
    let task_queue = queue.clone();
    let task_transport = transport.clone();
    let task_permit = permit.clone();
    let task = tokio::spawn(async move {
        task_queue
            .send(
                &*task_transport,
                &["一".into(), "二".into()],
                Some(&task_permit),
            )
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    permit.cancel();
    transport.echoes.add_permits(1);
    assert_eq!(task.await.unwrap(), Outcome::Cancelled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    let expired = Permit::new(Utc::now() - chrono::Duration::seconds(1));
    assert_eq!(
        queue
            .send(&*transport, &["过期".into()], Some(&expired))
            .await,
        Outcome::Cancelled
    );
    queue.pause();
    assert_eq!(
        queue.send(&*transport, &["暂停".into()], None).await,
        Outcome::Cancelled
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pause_then_resume_cannot_revive_a_pre_pause_batch() {
    let queue = SendQueue::default();
    let transport = Arc::new(Controlled {
        calls: AtomicUsize::new(0),
        entered: Semaphore::new(0),
        echoes: Semaphore::new(0),
        fail_at: None,
    });
    let task_queue = queue.clone();
    let task_transport = transport.clone();
    let task = tokio::spawn(async move {
        task_queue
            .send(&*task_transport, &["首段".into(), "旧剩余段".into()], None)
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    queue.pause();
    queue.resume();
    transport.echoes.add_permits(1);
    assert_eq!(task.await.unwrap(), Outcome::Cancelled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}
