//! Subscribes Cursor RunSSE clients to replayable Transport output.
use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode},
};
use bytes::Bytes;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    cursor::{
        protocol::connect::END_STREAM_FLAG,
        transport::{TransportHandle, TransportRegistry},
    },
    Result,
};

pub async fn stream(registry: &TransportRegistry, request_id: &str) -> Result<Response<Body>> {
    let handle = registry.get_or_create(request_id).await?;
    let receiver = handle.subscribe();
    let body_stream = local_body_stream(receiver, handle);
    let mut response = Response::new(Body::from_stream(body_stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("connect-protocol-version", HeaderValue::from_static("1"));
    Ok(response)
}

fn local_body_stream(
    mut receiver: mpsc::UnboundedReceiver<Bytes>,
    handle: TransportHandle,
) -> impl tokio_stream::Stream<Item = std::result::Result<Bytes, Infallible>> {
    async_stream::stream! {
        let mut guard = LocalRunGuard::new(handle);
        while let Some(chunk) = receiver.recv().await {
            let terminal = is_end_stream_frame(&chunk);
            if terminal {
                guard.complete();
            }
            yield Ok::<Bytes, Infallible>(chunk);
            if terminal {
                return;
            }
        }
        guard.complete();
    }
}

fn is_end_stream_frame(frame: &Bytes) -> bool {
    frame
        .first()
        .is_some_and(|flags| flags & END_STREAM_FLAG != 0)
}

struct LocalRunGuard {
    handle: TransportHandle,
    completed: bool,
}

impl LocalRunGuard {
    fn new(handle: TransportHandle) -> Self {
        Self {
            handle,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for LocalRunGuard {
    fn drop(&mut self) {
        if !self.completed {
            let handle = self.handle.clone();
            tokio::spawn(async move {
                handle.disconnect().await;
            });
        }
    }
}

pub async fn upstream(
    registry: TransportRegistry,
    request_id: String,
    generation: u64,
    response: Response<Body>,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let stream = async_stream::stream! {
        let _guard = UpstreamRunGuard {
            registry,
            request_id,
            generation,
        };
        let mut body = body.into_data_stream();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(chunk) => yield Ok::<Bytes, axum::Error>(chunk),
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

struct UpstreamRunGuard {
    registry: TransportRegistry,
    request_id: String,
    generation: u64,
}

impl Drop for UpstreamRunGuard {
    fn drop(&mut self) {
        self.registry
            .finish_upstream(self.request_id.clone(), self.generation);
    }
}
