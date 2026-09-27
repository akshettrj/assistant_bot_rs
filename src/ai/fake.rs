//! A scripted model for tests.

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};

use futures::future::BoxFuture;

use super::{AiError, Llm, Request};

/// Answers with the queued answers, in order, and remembers the requests.
#[derive(Debug, Default)]
pub struct FakeLlm {
    answers: Mutex<VecDeque<Result<serde_json::Value, AiError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeLlm {
    pub fn answering(
        answers: impl IntoIterator<Item = Result<serde_json::Value, AiError>>,
    ) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        lock(&self.requests).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Llm for FakeLlm {
    fn complete<'a>(
        &'a self,
        request: &'a Request,
    ) -> BoxFuture<'a, Result<serde_json::Value, AiError>> {
        lock(&self.requests).push(request.clone());
        let answer = lock(&self.answers)
            .pop_front()
            .unwrap_or_else(|| Err(AiError::Failed("no answer queued".into())));
        Box::pin(async move { answer })
    }
}
