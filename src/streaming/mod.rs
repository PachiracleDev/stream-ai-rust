pub mod anthropic;
pub mod anthropic_convert;
pub mod log;
pub mod openai_compat;
pub mod openai_responses;

use axum::response::sse::Event;
use futures::stream::Stream;

pub type BoxedStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<Event, String>> + Send>>;
