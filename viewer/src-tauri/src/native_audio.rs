//! Desktop audio: plays the container's Opus stream (`audio-stream`, TCP
//! 5902) on the host's default output with GStreamer. Output only; nothing
//! from the host is sent back. Absent or failing audio never affects the
//! session: the thread logs and stops.

use std::{
  io::Read,
  net::{SocketAddr, TcpStream},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;

/// The service starts with the desktop; give it as long as video gets.
const CONNECT_WINDOW: Duration = Duration::from_secs(30);
const MAX_PACKET: usize = 64 << 10;

#[derive(Default)]
pub struct NativeAudio {
  stop: Arc<AtomicBool>,
  started: AtomicBool,
}

impl NativeAudio {
  pub fn start(&self, address: Option<SocketAddr>) {
    let Some(address) = address else { return };
    if self.started.swap(true, Ordering::AcqRel) { return; }
    let stop = Arc::clone(&self.stop);
    thread::spawn(move || {
      match play(address, &stop) {
        Ok(()) => eprintln!("audio: stream ended"),
        Err(reason) => eprintln!("audio unavailable: {reason}"),
      }
    });
  }

  pub fn stop(&self) { self.stop.store(true, Ordering::Release); }
}

fn connect(address: SocketAddr, stop: &AtomicBool) -> Result<TcpStream, String> {
  let deadline = Instant::now() + CONNECT_WINDOW;
  loop {
    match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
      Ok(stream) => return Ok(stream),
      Err(error) if Instant::now() >= deadline || stop.load(Ordering::Acquire) => return Err(format!("connect: {error}")),
      Err(_) => thread::sleep(Duration::from_millis(250)),
    }
  }
}

fn play(address: SocketAddr, stop: &AtomicBool) -> Result<(), String> {
  gst::init().map_err(|error| format!("GStreamer: {error}"))?;
  let mut stream = connect(address, stop)?;
  stream.set_nodelay(true).ok();
  stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
  // Low latency over smoothness: no clock sync, a small sink buffer.
  let sink = if gst::ElementFactory::find("pulsesink").is_some() {
    "pulsesink buffer-time=60000 latency-time=10000 sync=false"
  } else {
    "autoaudiosink sync=false"
  };
  let pipeline = gst::parse_launch(&format!(
    "appsrc name=src is-live=true format=time do-timestamp=true \
       caps=audio/x-opus,channels=2,rate=48000,channel-mapping-family=0 ! \
     opusdec ! audioconvert ! audioresample ! {sink}"))
    .map_err(|error| format!("pipeline: {error}"))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "pipeline is not a pipeline")?;
  let source = pipeline.by_name("src").ok_or("no appsrc")?
    .downcast::<gst_app::AppSrc>().map_err(|_| "appsrc type")?;
  pipeline.set_state(gst::State::Playing).map_err(|error| format!("play: {error}"))?;
  eprintln!("audio: playing");
  let result = pump(&mut stream, &source, &pipeline, stop);
  let _ = pipeline.set_state(gst::State::Null);
  result
}

fn pump(stream: &mut TcpStream, source: &gst_app::AppSrc, pipeline: &gst::Pipeline, stop: &AtomicBool) -> Result<(), String> {
  let bus = pipeline.bus().ok_or("no bus")?;
  let mut length = [0u8; 4];
  while !stop.load(Ordering::Acquire) {
    if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error])
      && let gst::MessageView::Error(error) = message.view() {
      return Err(format!("GStreamer: {}", error.error()));
    }
    if !read_full(stream, &mut length, stop)? { return Ok(()); }
    let size = u32::from_be_bytes(length) as usize;
    if size > MAX_PACKET { return Err("oversized packet".into()); }
    let mut packet = vec![0u8; size];
    if !read_full(stream, &mut packet, stop)? { return Ok(()); }
    source.push_buffer(gst::Buffer::from_mut_slice(packet)).map_err(|error| format!("push: {error:?}"))?;
  }
  Ok(())
}

/// Ok(false): the stream ended or we were stopped.
fn read_full(stream: &mut TcpStream, buffer: &mut [u8], stop: &AtomicBool) -> Result<bool, String> {
  let mut filled = 0;
  while filled < buffer.len() {
    if stop.load(Ordering::Acquire) { return Ok(false); }
    match stream.read(&mut buffer[filled..]) {
      Ok(0) => return Ok(false),
      Ok(count) => filled += count,
      Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
      Err(error) => return Err(format!("read: {error}")),
    }
  }
  Ok(true)
}
