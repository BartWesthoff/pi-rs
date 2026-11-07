//! Async event streams (port of `utils/event-stream.ts`).
//!
//! A stream is split into a consumer half ([`EventStream`], which implements
//! [`futures::Stream`] and exposes [`EventStream::result`]) and a cloneable producer
//! half ([`EventSender`]) with `push`/`end`, mirroring the TypeScript class.
//!
//! The queue between them is unbounded like the TypeScript one, but a stream may coalesce: once
//! [`COALESCE_AFTER`] events wait for a slow consumer, a new event can merge into the last queued
//! one (assistant streams merge consecutive deltas of one content block), so the queue stops
//! growing while the consumer lags.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use web_time::Instant;

use futures::Stream;
use parking_lot::Mutex;
use tokio::sync::watch;

use crate::types::{AssistantMessage, AssistantMessageEvent, StopReason};

type CompleteFn<T> = Arc<dyn Fn(&T) -> bool + Send + Sync>;
type ExtractFn<T, R> = Arc<dyn Fn(&T) -> R + Send + Sync>;
type TransformFn<T> = Arc<dyn Fn(&mut T) + Send + Sync>;
/// Merges a new event into the last queued one; returns the event back when it cannot merge.
type CoalesceFn<T> = Arc<dyn Fn(&mut T, T) -> Option<T> + Send + Sync>;

/// Queued events after which a stream with a coalescing rule merges new events into the last one.
/// Each queued partial message can hold its own copy of the content, so this bounds the memory a
/// lagging consumer holds to about this many copies.
pub const COALESCE_AFTER: usize = 256;

struct QueueState<T> {
    items: VecDeque<T>,
    /// Every producer is gone or the stream ended: no more items after the queued ones.
    closed: bool,
    /// The consumer is gone: new items are dropped.
    abandoned: bool,
    waker: Option<Waker>,
}

/// Single-consumer event queue shared by the two halves.
struct Queue<T> {
    state: Mutex<QueueState<T>>,
    coalesce: Option<CoalesceFn<T>>,
}

impl<T> Queue<T> {
    fn push(&self, item: T) {
        let waker = {
            let mut state = self.state.lock();
            if state.closed || state.abandoned {
                return;
            }
            let item = match (&self.coalesce, state.items.len() >= COALESCE_AFTER) {
                (Some(coalesce), true) => match state.items.back_mut() {
                    Some(last) => coalesce(last, item),
                    None => Some(item),
                },
                _ => Some(item),
            };
            if let Some(item) = item {
                state.items.push_back(item);
            }
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn close(&self) {
        let waker = {
            let mut state = self.state.lock();
            state.closed = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll_pop(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut state = self.state.lock();
        if let Some(item) = state.items.pop_front() {
            return Poll::Ready(Some(item));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// The producers' handle on the queue; closes it when the last producer is dropped.
struct QueueSender<T>(Arc<Queue<T>>);

impl<T> Drop for QueueSender<T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct SenderInner<T, R> {
    tx: Option<QueueSender<T>>,
    done: bool,
    result_tx: watch::Sender<Option<R>>,
    is_complete: CompleteFn<T>,
    extract_result: ExtractFn<T, R>,
    /// Hook applied to events before delivery while the stream is not done.
    before_push: Option<TransformFn<T>>,
    /// Hook applied to `end()` results while the stream is not done.
    before_end: Option<TransformFn<R>>,
}

/// Producer half of an [`EventStream`].
pub struct EventSender<T, R> {
    inner: Arc<Mutex<SenderInner<T, R>>>,
}

impl<T, R> Clone for EventSender<T, R> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<T, R: Clone> EventSender<T, R> {
    /// Push an event. Ignored once the stream is done.
    pub fn push(&self, mut event: T) {
        let mut inner = self.inner.lock();
        if inner.done {
            return;
        }
        if let Some(hook) = inner.before_push.clone() {
            hook(&mut event);
        }
        let complete = (inner.is_complete)(&event);
        if complete {
            inner.done = true;
            let result = (inner.extract_result)(&event);
            inner.result_tx.send_replace(Some(result));
        }
        if let Some(tx) = &inner.tx {
            tx.0.push(event);
        }
        if complete {
            inner.tx = None;
        }
    }

    /// End the stream, optionally resolving the final result.
    pub fn end(&self, result: Option<R>) {
        let mut inner = self.inner.lock();
        let was_done = inner.done;
        inner.done = true;
        if let Some(mut result) = result {
            if !was_done
                && let Some(hook) = inner.before_end.clone() {
                    hook(&mut result);
                }
            inner.result_tx.send_replace(Some(result));
        }
        inner.tx = None;
    }

    pub fn is_done(&self) -> bool {
        self.inner.lock().done
    }
}

/// Consumer half: an async stream of events with a final result.
pub struct EventStream<T, R> {
    rx: Arc<Queue<T>>,
    result_rx: watch::Receiver<Option<R>>,
}

impl<T, R> Drop for EventStream<T, R> {
    fn drop(&mut self) {
        let mut state = self.rx.state.lock();
        state.abandoned = true;
        state.items.clear();
    }
}

impl<T, R: Clone> EventStream<T, R> {
    /// Create a stream. `is_complete` marks the terminal event; `extract_result` maps it to the result.
    pub fn new(
        is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
        extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
    ) -> (EventSender<T, R>, EventStream<T, R>) {
        Self::with_hooks(Arc::new(is_complete), Arc::new(extract_result), None, None, None)
    }

    fn with_hooks(
        is_complete: CompleteFn<T>,
        extract_result: ExtractFn<T, R>,
        before_push: Option<TransformFn<T>>,
        before_end: Option<TransformFn<R>>,
        coalesce: Option<CoalesceFn<T>>,
    ) -> (EventSender<T, R>, EventStream<T, R>) {
        let rx = Arc::new(Queue {
            state: Mutex::new(QueueState { items: VecDeque::new(), closed: false, abandoned: false, waker: None }),
            coalesce,
        });
        let (result_tx, result_rx) = watch::channel(None);
        let sender = EventSender {
            inner: Arc::new(Mutex::new(SenderInner {
                tx: Some(QueueSender(rx.clone())),
                done: false,
                result_tx,
                is_complete,
                extract_result,
                before_push,
                before_end,
            })),
        };
        (sender, EventStream { rx, result_rx })
    }

    /// Receive the next event; `None` once the stream has ended and drained.
    pub async fn next_event(&mut self) -> Option<T> {
        std::future::poll_fn(|cx| self.rx.poll_pop(cx)).await
    }

    /// Events waiting to be received.
    pub fn queued(&self) -> usize {
        self.rx.state.lock().items.len()
    }

    /// Wait for the final result. `None` when every producer was dropped without one.
    pub async fn try_result(&self) -> Option<R> {
        let mut rx = self.result_rx.clone();
        loop {
            if let Some(value) = rx.borrow_and_update().clone() {
                return Some(value);
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone();
            }
        }
    }
}

impl<T, R> Stream for EventStream<T, R> {
    type Item = T;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.rx.poll_pop(cx)
    }
}

impl<T, R> Unpin for EventStream<T, R> {}

// ---------------------------------------------------------------------------
// Assistant message streams
// ---------------------------------------------------------------------------

/// Producer half of an [`AssistantMessageEventStream`].
pub type AssistantMessageEventSender = EventSender<AssistantMessageEvent, AssistantMessage>;

/// Event stream of one assistant response.
///
/// It also times the response: the final message (`done` or `error` event, or the
/// result passed to `end()`) gets `duration_ms`, measured with a monotonic clock from
/// the stream's creation, unless the message already has one or its `timestamp`
/// predates the stream.
pub struct AssistantMessageEventStream {
    inner: EventStream<AssistantMessageEvent, AssistantMessage>,
}

struct StreamTimer {
    started_at: i64,
    started_at_monotonic: Instant,
}

impl StreamTimer {
    fn time(&self, message: &mut AssistantMessage) {
        if message.duration_ms.is_some() || message.timestamp < self.started_at {
            return;
        }
        message.duration_ms = Some(self.started_at_monotonic.elapsed().as_millis() as u64);
    }
}

impl AssistantMessageEventStream {
    /// Create a new stream and its sender.
    pub fn new() -> (AssistantMessageEventSender, AssistantMessageEventStream) {
        let timer = Arc::new(StreamTimer {
            started_at: crate::utils::now_ms(),
            started_at_monotonic: Instant::now(),
        });
        let push_timer = timer.clone();
        let hook: TransformFn<AssistantMessageEvent> = Arc::new(move |event| {
            match event {
                AssistantMessageEvent::Done { message, .. } => push_timer.time(message),
                AssistantMessageEvent::Error { error, .. } => push_timer.time(error),
                _ => {}
            }
        });
        let end_hook: TransformFn<AssistantMessage> = Arc::new(move |message| timer.time(message));
        let (sender, stream) = EventStream::with_hooks(
            Arc::new(|event: &AssistantMessageEvent| event.is_terminal()),
            Arc::new(|event: &AssistantMessageEvent| match event {
                AssistantMessageEvent::Done { message, .. } => message.clone(),
                AssistantMessageEvent::Error { error, .. } => error.clone(),
                _ => unreachable!("only terminal events complete the stream"),
            }),
            Some(hook),
            Some(end_hook),
            Some(Arc::new(coalesce_assistant_event)),
        );
        (sender, AssistantMessageEventStream { inner: stream })
    }

    /// Receive the next event; `None` once the stream has ended and drained.
    pub async fn next_event(&mut self) -> Option<AssistantMessageEvent> {
        self.inner.next_event().await
    }

    /// Events waiting to be received.
    pub fn queued(&self) -> usize {
        self.inner.queued()
    }

    /// Wait for the final assistant message. When every producer was dropped without a
    /// result, returns an error message.
    pub async fn result(&self) -> AssistantMessage {
        match self.inner.try_result().await {
            Some(message) => message,
            None => AssistantMessage {
                stop_reason: StopReason::Error,
                error_message: Some("Stream ended without a result".to_string()),
                timestamp: crate::utils::now_ms(),
                ..Default::default()
            },
        }
    }

    /// Wait for the final result, `None` if producers dropped without one.
    pub async fn try_result(&self) -> Option<AssistantMessage> {
        self.inner.try_result().await
    }

    /// Drain all events and return them with the final result.
    pub async fn collect_all(mut self) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
        let mut events = Vec::new();
        while let Some(event) = self.next_event().await {
            events.push(event);
        }
        let result = self.result().await;
        (events, result)
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Merge `next` into `last` when both are deltas of the same content block: the deltas are
/// concatenated and the newer partial message is kept, so consumers see the same text with fewer
/// events.
fn coalesce_assistant_event(
    last: &mut AssistantMessageEvent,
    next: AssistantMessageEvent,
) -> Option<AssistantMessageEvent> {
    use AssistantMessageEvent::{TextDelta, ThinkingDelta, ToolCallDelta};
    match (last, next) {
        (
            TextDelta { content_index, delta, partial },
            TextDelta { content_index: index, delta: more, partial: newer },
        )
        | (
            ThinkingDelta { content_index, delta, partial },
            ThinkingDelta { content_index: index, delta: more, partial: newer },
        )
        | (
            ToolCallDelta { content_index, delta, partial },
            ToolCallDelta { content_index: index, delta: more, partial: newer },
        ) if *content_index == index => {
            delta.push_str(&more);
            *partial = newer;
            None
        }
        (_, next) => Some(next),
    }
}

/// Factory function (for use in extensions).
pub fn create_assistant_message_event_stream() -> (AssistantMessageEventSender, AssistantMessageEventStream) {
    AssistantMessageEventStream::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantContent, TextContent};

    /// Pushes `count` one-character text deltas with growing partial messages; returns the text.
    fn push_text_deltas(sender: &AssistantMessageEventSender, count: usize) -> String {
        let mut output = AssistantMessage {
            content: vec![AssistantContent::Text(TextContent::new(""))].into(),
            ..Default::default()
        };
        sender.push(AssistantMessageEvent::Start { partial: output.clone() });
        sender.push(AssistantMessageEvent::TextStart { content_index: 0, partial: output.clone() });
        let mut text = String::new();
        for i in 0..count {
            let delta = char::from(b'a' + (i % 26) as u8).to_string();
            text.push_str(&delta);
            if let Some(AssistantContent::Text(block)) = output.content.get_mut(0) {
                block.text.push_str(&delta);
            }
            sender.push(AssistantMessageEvent::TextDelta { content_index: 0, delta, partial: output.clone() });
        }
        output.stop_reason = StopReason::Stop;
        sender.push(AssistantMessageEvent::Done { reason: StopReason::Stop, message: output });
        text
    }

    async fn drain(stream: &mut AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
        let mut events = Vec::new();
        while let Some(event) = stream.next_event().await {
            events.push(event);
        }
        events
    }

    fn deltas(events: &[AssistantMessageEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                AssistantMessageEvent::TextDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn delivers_every_event_while_the_consumer_keeps_up() {
        let (sender, mut stream) = AssistantMessageEventStream::new();
        let text = push_text_deltas(&sender, COALESCE_AFTER - 8);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), COALESCE_AFTER - 8 + 3);
        assert_eq!(deltas(&events), text);
    }

    #[tokio::test]
    async fn coalesces_deltas_for_a_slow_consumer() {
        let (sender, mut stream) = AssistantMessageEventStream::new();
        let text = push_text_deltas(&sender, 10_000);
        assert!(stream.queued() <= COALESCE_AFTER + 1, "queue grew to {}", stream.queued());
        let events = drain(&mut stream).await;
        assert_eq!(deltas(&events), text, "coalesced deltas still concatenate to the text");
        let last_delta = events
            .iter()
            .rev()
            .find_map(|event| match event {
                AssistantMessageEvent::TextDelta { partial, .. } => Some(partial),
                _ => None,
            })
            .expect("a text delta");
        assert_eq!(last_delta.content[0].as_text().map(|t| t.text.as_str()), Some(text.as_str()));
        assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));
        assert_eq!(stream.result().await.content[0].as_text().map(|t| t.text.len()), Some(10_000));
    }

    #[tokio::test]
    async fn drops_events_once_the_consumer_is_gone() {
        let (sender, stream) = AssistantMessageEventStream::new();
        let queue = stream.inner.rx.clone();
        drop(stream);
        push_text_deltas(&sender, 100);
        assert!(queue.state.lock().items.is_empty());
    }

    #[test]
    fn partial_messages_share_their_content() {
        let message = AssistantMessage {
            content: vec![AssistantContent::Text(TextContent::new("x".repeat(1 << 20)))].into(),
            ..Default::default()
        };
        let partial = message.clone();
        assert!(partial.content.ptr_eq(&message.content));
    }
}
