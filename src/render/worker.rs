//! CPU 密集的图像工作共用两条执行槽，避免阻塞池无限扩张。
use std::sync::{Arc, LazyLock};
use tokio::{sync::Semaphore, task::JoinError};

static GATE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub async fn run<F, T>(work: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_gate(GATE.clone(), work).await
}

async fn run_with_gate<F, T>(gate: Arc<Semaphore>, work: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = gate.acquire_owned().await.expect("渲染闸门不关闭");
    tokio::task::spawn_blocking(move || {
        // spawn_blocking 开始后无法取消，许可必须跟着实际工作一起释放。
        let _permit = permit;
        work()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_keeps_the_slot_until_work_finishes() {
        let gate = Arc::new(Semaphore::new(1));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_with_gate(gate.clone(), move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let held = gate.available_permits() == 0;
        release.send(()).unwrap();
        assert!(held, "调用方取消不能提前释放阻塞工作的许可");
        let _permit = tokio::time::timeout(std::time::Duration::from_secs(2), gate.acquire())
            .await
            .unwrap()
            .unwrap();
    }
}
