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
    async fn send_confirm(&self, _: &str, _: Option<&str>) -> Delivery {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        if self.fail_at == Some(index) {
            return Outcome::Uncertain.into();
        }
        self.echoes.acquire().await.unwrap().forget();
        Outcome::Confirmed.into()
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
                None,
            )
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    let other_queue = queue.clone();
    let other_transport = transport.clone();
    let manual = tokio::spawn(async move {
        other_queue
            .send(&*other_transport, &["人工消息".into()], None, None)
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
    let failed = task.await.unwrap();
    assert_eq!(failed.outcome, Outcome::Uncertain);
    assert_eq!(failed.confirmed_segments, ["第一段"]);
    assert_eq!(failed.unconfirmed_segments, ["第二段", "不得发送第三段"]);
    assert_eq!(manual.await.unwrap().outcome, Outcome::Cancelled);
    transport.echoes.add_permits(1);
    let fresh = queue
        .send(&*transport, &["失败后新输入".into()], None, None)
        .await;
    assert_eq!(fresh.outcome, Outcome::Confirmed);
    assert_eq!(fresh.confirmed_segments, ["失败后新输入"]);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
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
                None,
            )
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    permit.cancel();
    transport.echoes.add_permits(1);
    assert_eq!(task.await.unwrap().outcome, Outcome::Cancelled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    let expired = Permit::new(Utc::now() - chrono::Duration::seconds(1));
    assert_eq!(
        queue
            .send(&*transport, &["过期".into()], Some(&expired), Some("42"))
            .await
            .outcome,
        Outcome::Cancelled
    );
    queue.pause();
    assert_eq!(
        queue
            .send(&*transport, &["暂停".into()], None, Some("42"))
            .await
            .outcome,
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
            .send(
                &*task_transport,
                &["首段".into(), "旧剩余段".into()],
                None,
                None,
            )
            .await
    });
    transport.entered.acquire().await.unwrap().forget();
    queue.pause();
    queue.resume();
    let fresh_segments = ["恢复后新输入".into()];
    let fresh = queue.send(&*transport, &fresh_segments, None, None);
    tokio::pin!(fresh);
    std::future::poll_fn(|cx| {
        assert!(fresh.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    transport.echoes.add_permits(2);
    assert_eq!(task.await.unwrap().outcome, Outcome::Cancelled);
    assert_eq!(fresh.await.outcome, Outcome::Confirmed);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pause_during_response_wait_invalidates_even_repairable_authority() {
    struct ContentRejected {
        entered: Semaphore,
        release: Semaphore,
    }
    impl Transport for ContentRejected {
        async fn send_confirm(&self, _: &str, _: Option<&str>) -> Delivery {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Delivery::new(Outcome::Rejected, Cause::ContentRejected, "明确内容拒绝")
        }
    }
    let queue = SendQueue::default();
    let permit = Permit::new(Utc::now() + chrono::Duration::minutes(1));
    let transport = Arc::new(ContentRejected {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let (q, p, t) = (queue.clone(), permit.clone(), transport.clone());
    let task = tokio::spawn(async move { q.send(&*t, &["失败段".into()], Some(&p), None).await });
    transport.entered.acquire().await.unwrap().forget();
    queue.pause();
    queue.resume();
    transport.release.add_permits(1);
    let result = task.await.unwrap();
    assert_eq!(result.diagnosis.cause, Cause::ContentRejected);
    assert!(!permit.valid(), "暂停过的原许可不能因可改写失败而复活");
    assert_eq!(result.unconfirmed_segments, ["失败段"]);
}
