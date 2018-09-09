use super::*;
use super::hyper::header::{Authorization,Bearer};

#[derive(Default, PartialEq, Eq, Debug)]
pub struct Event {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

struct SSEBodyStream {
    body: hyper::Body,
    events: Vec<Event>,
    buf: Vec<u8>,
}

impl SSEBodyStream {
    fn new(body: hyper::Body) -> Self {
        Self {
            body,
            events: Vec::new(),
            buf: Vec::new(),
        }
    }
}

impl Stream for SSEBodyStream {
    type Item = Event;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if !self.events.is_empty() {
            task::current().notify();
            return Ok(Async::Ready(self.events.pop()));
        }

        match try_ready!(self.body.poll()) {
            None => Ok(Async::Ready(None)),
            Some(chunk) => {
                let mut buf = Vec::new();
                std::mem::swap(&mut buf, &mut self.buf);

                buf.extend_from_slice(&chunk);

                let (mut events, next_buf) = match parse::parse_sse_chunks(buf) {
                    Ok(tup) => tup,
                    Err(e) => {
                        bail!(error::ErrorKind::Protocol(e));
                    }
                };

                self.buf = next_buf;
                events.reverse();
                self.events = events;

                self.poll()
            }
        }
    }
}

pub struct SSEStream<C: hyper::client::Connect> {
    url: hyper::Uri,
    client: hyper::Client<C>,

    fut_req: Option<Box<Future<Item = hyper::Response, Error = hyper::Error>>>,
    inner: Option<SSEBodyStream>,

    last_event_id: Option<String>,
    auth_header: Option<Authorization<Bearer>>
}

impl<C: hyper::client::Connect> SSEStream<C> {
    pub fn new(url: hyper::Uri, client: hyper::Client<C>, auth_header: Option<Authorization<Bearer>>) -> Self {
        Self {
            url,
            client: client.clone(),

            fut_req: None,
            inner: None,

            last_event_id: None,
            auth_header
        }
    }
}

impl<C: hyper::client::Connect> Stream for SSEStream<C> {
    type Item = Event;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if let Some(mut fut_req) = self.fut_req.take() {
            match fut_req.poll() {
                Err(_e) => {
                    println!("{:?}", _e);
                    //error!("failed to connect, retry: {:?}", _e);
                    // fallthrough
                }
                Ok(Async::NotReady) => {
                    self.fut_req = Some(fut_req);
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(resp)) => {
                    print("connected!")
                    info!("sse stream connected: {}", self.url);
                    self.inner = Some(SSEBodyStream::new(resp.body()));
                }
            }
        }

        if let Some(ref mut s) = self.inner {
            match s.poll() {
                Err(_e) => {
                    println!("{:?}", _e);
                    //error!("failed to read body, trying to reconnect: {:?}", _e);
                    // fallthrough
                }
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Ok(Async::Ready(None)) => {
                    println!("got none!");
                    // server drops connection, try to reconnect
                    // fallthrough
                }
                Ok(Async::Ready(Some(ev))) => {
                    if let Some(ref event_id) = ev.id {
                        self.last_event_id = Some(event_id.clone());
                    }

                    return Ok(Async::Ready(Some(ev)));
                }
            }
        }

        println!("trying to connect!");
        // retry case
        self.inner = None;
        info!("trying to connect: {}", self.url);
        let mut req = Request::new(hyper::Method::Get, self.url.clone());

        // set LastEventId
        if let Some(ref last_event_id) = self.last_event_id {
            let headers = req.headers_mut();
            headers.set(LastEventId(last_event_id.clone()));
        }

        if let Some(ref auth_header) = self.auth_header {
            let headers = req.headers_mut();
            headers.set(auth_header.clone());
        }

        let client = self.client.clone();
        let req = tokio_timer::Delay::new(Instant::now() + Duration::from_millis(100))
            .then(move |_| client.request(req));

        self.fut_req = Some(Box::new(req));
        self.poll()
    }
}
