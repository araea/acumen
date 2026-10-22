//! 一次请求失败了要不要再发一次。
//!
//! 2026-10-22 的现场：DeepSeek 官方接口回 200、响应体传到一半连接被掐断，reqwest 报
//! `error decoding response body`，整轮对话就这么判死。这类失败换条连接多半就成，
//! 不该原样摔给用户。
//!
//! 只重试「再发有意义」的那种：传输层断开、响应被截断、限流、服务端 5xx。密钥错、
//! 模型名错、上下文超长这些，重发多少次都一样，原样往上抛。

use std::future::Future;
use std::time::{Duration, SystemTime};

use rig_core::error::ProviderError;

use super::LOG_TARGET;

/// 一次调用最多发几次（含首次）。
const ATTEMPTS: u32 = 3;
/// 首次重试前等多久；之后每次翻倍。
const BACKOFF: Duration = Duration::from_millis(500);
/// 服务端点名要等多久也不超过这个数：一轮对话的预算有限，等不来就先认输。
const MAX_WAIT: Duration = Duration::from_secs(8);

/// 值得再发一次的失败。
///
/// 只回答两个问题：这次失败是不是可能下次就好，服务端点名要等多久。怎么重发、
/// 退避多长由 `call` 说了算。
pub(crate) trait Transient: std::error::Error {
    /// 换一条连接重发一次，有没有可能成。
    fn transient(&self) -> bool;

    /// 服务端点名的等待时长（`retry-after` 或正文里的提示），没有就返回 `None`。
    fn asked(&self) -> Option<Duration> {
        None
    }
}

impl Transient for ProviderError {
    /// 传输层实例错误（连接断开、响应体没读完）与 408 / 425 / 429 / 5xx 由 rig
    /// 分类，与这里要的口径完全一致。
    fn transient(&self) -> bool {
        self.is_retryable()
    }

    fn asked(&self) -> Option<Duration> {
        self.report().retry_after(SystemTime::now())
    }
}

/// 发一次请求；失败且值得再来时按退避重发，最多 `ATTEMPTS` 次。
///
/// `attempt` 每次都会被调用，所以请求得由它自己带一份可重复使用的副本。
pub(crate) async fn call<T, E, F, Fut>(label: &str, mut attempt: F) -> Result<T, E>
where
    E: Transient,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut tries = 0;
    loop {
        tries += 1;
        let error = match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if tries >= ATTEMPTS || !error.transient() {
            return Err(error);
        }
        let wait = error
            .asked()
            .unwrap_or(BACKOFF * 2u32.pow(tries - 1))
            .min(MAX_WAIT);
        warn!(
            target: LOG_TARGET,
            "{label}第 {tries} 次没成（{}），{:?} 后再发一次",
            detail(&error),
            wait
        );
        tokio::time::sleep(wait).await;
    }
}

/// 一条错误连同它的 cause 链，用「: 」串起来。
///
/// 真正的病因只在下层：`error decoding response body` 之上看不出是连接被掐断、
/// 还是响应体只收到一半，看到这句话的人无从判断该不该重试。
pub(crate) fn detail<E>(error: &E) -> String
where
    E: std::error::Error + ?Sized,
{
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条带 cause 的错误：`detail` 要把下层接上，`Transient` 的默认值是不重试。
    #[derive(Debug)]
    struct Outer(Inner);

    #[derive(Debug)]
    struct Inner;

    impl std::fmt::Display for Outer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("外层")
        }
    }

    impl std::fmt::Display for Inner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("内层")
        }
    }

    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    impl std::error::Error for Inner {}

    impl Transient for Outer {
        fn transient(&self) -> bool {
            true
        }
    }

    #[test]
    fn detail_joins_the_cause_chain() {
        assert_eq!(detail(&Outer(Inner)), "外层: 内层");
    }

    /// 第一次失败、第二次成功：只该发出两次，第二次的返回值原样带回。
    #[tokio::test]
    async fn retries_a_transient_failure() {
        let mut calls = 0;
        let value = call("试试", || {
            calls += 1;
            let attempt = calls;
            async move {
                if attempt == 1 {
                    Err(Outer(Inner))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(value.expect("第二次应当成功"), 7);
        assert_eq!(calls, 2);
    }

    /// 不可重试的错误一次就抛，不浪费时间。
    #[tokio::test]
    async fn gives_up_on_a_permanent_failure() {
        #[derive(Debug)]
        struct Permanent;

        impl std::fmt::Display for Permanent {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("永久失败")
            }
        }

        impl std::error::Error for Permanent {}

        impl Transient for Permanent {
            fn transient(&self) -> bool {
                false
            }
        }

        let mut calls = 0;
        let value: Result<u32, Permanent> = call("试试", || {
            calls += 1;
            async { Err(Permanent) }
        })
        .await;
        assert!(value.is_err());
        assert_eq!(calls, 1);
    }
}
