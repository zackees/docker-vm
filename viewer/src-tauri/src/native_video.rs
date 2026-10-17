//! Native presentation of the container's NVENC stream.
//!
//! WebKitGTK cannot hand a WebCodecs `VideoFrame` to WebGL without a CPU round
//! trip (~40-50 ms per 2560x1600 frame), so the webview path tops out near 12
//! fps. Here the viewer reads the stream itself and decodes and draws it with
//! GStreamer on the GPU (`nvh264dec` -> GL -> `gtkglsink`), in a GTK overlay
//! above the webview. The overlay takes no input: keyboard and pointer still
//! reach noVNC's canvas underneath, and the UI moves the overlay onto that
//! canvas's box. Any failure hides the overlay; the UI then restores RFB
//! frames.

use std::{
  io::Read,
  net::{SocketAddr, TcpStream},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
  },
  thread,
  time::Duration,
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use gtk::{glib::thread_guard::ThreadGuard, prelude::*};
use serde::Serialize;

/// u32 length | u8 flags (bit 0 = keyframe) | u64 pts (us), big-endian.
const HEADER_BYTES: usize = 13;

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum VideoState {
  /// No stream offered, or presentation unavailable on this host.
  Off { reason: String },
  Starting,
  Live { frames: u64 },
  Failed { reason: String },
}

/// Main-thread pieces: the GL sink and its widget in the window's overlay.
pub struct Presenter {
  sink: gst::Element,
  // GTK widgets are main-thread only; the guard enforces it.
  widget: ThreadGuard<gtk::Widget>,
}

/// Shared between the GTK main thread, commands and the stream thread.
pub struct NativeVideo {
  presenter: Mutex<Option<Presenter>>,
  unavailable: Mutex<Option<String>>,
  state: Mutex<VideoState>,
  frames: Arc<AtomicU64>,
  stop: Arc<AtomicBool>,
  started: AtomicBool,
  // Set while the settings panel is open over the video.
  hidden: Arc<AtomicBool>,
}

impl Default for NativeVideo {
  fn default() -> Self {
    Self {
      presenter: Mutex::new(None),
      unavailable: Mutex::new(None),
      state: Mutex::new(VideoState::Off { reason: "not started".into() }),
      frames: Arc::new(AtomicU64::new(0)),
      stop: Arc::new(AtomicBool::new(false)),
      started: AtomicBool::new(false),
      hidden: Arc::new(AtomicBool::new(false)),
    }
  }
}

/// Wraps the webview in an overlay holding the (hidden) video widget. Must run
/// on the GTK main thread, before any stream starts.
pub fn install(window: &tauri::WebviewWindow, video: &NativeVideo) {
  match build_presenter(window) {
    Ok(presenter) => *video.presenter.lock().expect("presenter lock") = Some(presenter),
    Err(reason) => {
      eprintln!("native video unavailable: {reason}");
      *video.unavailable.lock().expect("presenter lock") = Some(reason);
    }
  }
}

fn build_presenter(window: &tauri::WebviewWindow) -> Result<Presenter, String> {
  // On Wayland, gtkglsink can share GTK's GL context only as desktop GL 3;
  // with the default API choice its context creation fails. Under X11 the
  // same choice stalls the sink, so it is set only here.
  let wayland = window.gtk_window().ok()
    .is_some_and(|gtk_window| gtk_window.display().type_().name() == "GdkWaylandDisplay");
  if wayland && std::env::var_os("GST_GL_API").is_none() {
    // SAFETY: runs on the GTK main thread during setup, before GStreamer or
    // any thread of ours reads the environment.
    unsafe { std::env::set_var("GST_GL_API", "opengl3") };
  }
  gst::init().map_err(|error| format!("GStreamer: {error}"))?;
  let sink = gst::ElementFactory::make("gtkglsink").property("sync", false).build()
    .map_err(|_| "gtkglsink is not installed".to_string())?;
  let widget = sink.property::<gtk::Widget>("widget");
  let vbox = window.default_vbox().map_err(|error| format!("window box: {error}"))?;
  let webview = vbox.children().into_iter().next().ok_or("window has no webview")?;
  let overlay = gtk::Overlay::new();
  vbox.remove(&webview);
  overlay.add(&webview);
  overlay.add_overlay(&widget);
  // Input passes through to the webview (noVNC's canvas) underneath.
  overlay.set_overlay_pass_through(&widget, true);
  widget.set_halign(gtk::Align::Start);
  widget.set_valign(gtk::Align::Start);
  // gtkglsink creates its GL context when the widget is realized and cannot
  // preroll before that, so it is shown from the start - transparent until
  // the first frame, so it never covers the RFB canvas without video.
  widget.set_opacity(0.0);
  widget.set_size_request(1, 1);
  // Pass-through alone is not enough: the sink's GL area has its own event
  // window (it takes pointer input for navigation) and swallowed every click.
  // An empty input shape on the overlay child window and all below it sends
  // input to the webview.
  widget.connect_realize(|widget| {
    fn no_input(window: &gtk::gdk::Window) {
      window.input_shape_combine_region(&gtk::cairo::Region::create(), 0, 0);
      window.set_pass_through(true);
      for child in window.children() { no_input(&child); }
    }
    if let Some(window) = widget.window() { no_input(&window); }
  });
  widget.set_sensitive(false);
  // The settings button must sit above the video, so it is a GTK widget in
  // the same overlay; it opens the page's settings panel.
  let settings = gtk::Button::with_label("⚙");
  settings.set_tooltip_text(Some("Settings"));
  settings.set_halign(gtk::Align::End);
  settings.set_valign(gtk::Align::Start);
  settings.set_margin_top(6);
  settings.set_margin_end(6);
  settings.set_opacity(0.55);
  settings.set_relief(gtk::ReliefStyle::None);
  let page = window.clone();
  settings.connect_clicked(move |_| { let _ = page.eval("window.__toggleSettings?.()"); });
  overlay.add_overlay(&settings);
  vbox.pack_start(&overlay, true, true, 0);
  overlay.show_all();
  Ok(Presenter { sink, widget: ThreadGuard::new(widget) })
}

impl NativeVideo {
  pub fn state(&self) -> VideoState {
    let state = self.state.lock().map(|state| state.clone()).unwrap_or(VideoState::Off { reason: "unavailable".into() });
    match state {
      VideoState::Live { .. } => VideoState::Live { frames: self.frames.load(Ordering::Relaxed) },
      other => other,
    }
  }

  pub fn frames(&self) -> u64 { self.frames.load(Ordering::Relaxed) }

  /// Whether the native settings button is present (the page then hides its
  /// own, which the video layer would cover).
  pub fn has_controls(&self) -> bool {
    self.presenter.lock().is_ok_and(|presenter| presenter.is_some())
  }

  /// Hides (or restores) live video, e.g. under the settings panel.
  pub fn set_hidden(self: &Arc<Self>, hidden: bool) {
    self.hidden.store(hidden, Ordering::Release);
    let live = matches!(self.state(), VideoState::Live { .. });
    self.show(live && !hidden);
  }

  fn set(&self, state: VideoState) {
    if let Ok(mut current) = self.state.lock() { *current = state; }
  }

  /// Starts presenting the stream at `address`. Returns false (and the UI
  /// keeps its own path) when this host cannot present natively.
  pub fn start(self: &Arc<Self>, address: Option<SocketAddr>) -> bool {
    if let Some(reason) = self.unavailable.lock().ok().and_then(|reason| reason.clone()) {
      self.set(VideoState::Off { reason });
      return false;
    }
    let Some(address) = address else {
      self.set(VideoState::Off { reason: "no video stream offered".into() });
      return false;
    };
    let Some(sink) = self.presenter.lock().ok().and_then(|presenter| presenter.as_ref().map(|p| p.sink.clone())) else {
      return false;
    };
    if self.started.swap(true, Ordering::AcqRel) { return true; }
    self.set(VideoState::Starting);
    let video = Arc::clone(self);
    thread::spawn(move || {
      let reason = video.run(address, &sink).err().unwrap_or_else(|| "stream ended".into());
      eprintln!("native video stopped: {reason}");
      video.set(VideoState::Failed { reason });
      video.show(false);
    });
    true
  }

  pub fn stop(&self) { self.stop.store(true, Ordering::Release); }

  /// Places the overlay on noVNC's canvas (CSS px == GTK logical px here).
  pub fn place(&self, x: i32, y: i32, width: i32, height: i32) {
    if let Ok(presenter) = self.presenter.lock() && let Some(presenter) = presenter.as_ref() {
      let widget = presenter.widget.get_ref();
      widget.set_margin_start(x.max(0));
      widget.set_margin_top(y.max(0));
      widget.set_size_request(width.max(1), height.max(1));
    }
  }

  fn show(self: &Arc<Self>, visible: bool) {
    let video = Arc::clone(self);
    // Widgets belong to the GTK main thread.
    gtk::glib::MainContext::default().invoke(move || {
      if let Ok(presenter) = video.presenter.lock() && let Some(presenter) = presenter.as_ref() {
        let widget = presenter.widget.get_ref();
        // Hidden under the settings panel keeps its place; stopped does not.
        let hidden = video.hidden.load(Ordering::Acquire);
        widget.set_opacity(if visible && !hidden { 1.0 } else { 0.0 });
        if !visible { widget.set_size_request(1, 1); }
      }
    });
  }

  fn run(self: &Arc<Self>, address: SocketAddr, sink: &gst::Element) -> Result<(), String> {
    let decoder = if gst::ElementFactory::find("nvh264dec").is_some() { "nvh264dec" } else { "avdec_h264" };
    let pipeline = gst::parse_launch(&format!(
      "appsrc name=src is-live=true format=time do-timestamp=true \
         caps=video/x-h264,stream-format=byte-stream,alignment=au ! \
       h264parse ! {decoder} ! glsinkbin name=out"))
      .map_err(|error| format!("pipeline: {error}"))?
      .downcast::<gst::Pipeline>()
      .map_err(|_| "pipeline is not a pipeline")?;
    pipeline.by_name("out").ok_or("no glsinkbin")?.set_property("sink", sink);
    let source = pipeline.by_name("src").ok_or("no appsrc")?
      .downcast::<gst_app::AppSrc>().map_err(|_| "appsrc type")?;
    // Count frames as they reach the sink; the first one reveals the overlay.
    let frames = Arc::clone(&self.frames);
    let video = Arc::clone(self);
    sink.static_pad("sink").ok_or("sink has no pad")?.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
      let count = frames.fetch_add(1, Ordering::Relaxed) + 1;
      if count == 1 {
        eprintln!("native video: first frame presented");
        video.set(VideoState::Live { frames: 1 });
        video.show(true);
      } else if count % 300 == 0 {
        eprintln!("native video: {count} frames presented");
      }
      gst::PadProbeReturn::Ok
    });
    pipeline.set_state(gst::State::Playing).map_err(|error| format!("play: {error}"))?;
    let result = self.pump(address, &source, &pipeline);
    let _ = pipeline.set_state(gst::State::Null);
    result
  }

  fn pump(&self, address: SocketAddr, source: &gst_app::AppSrc, pipeline: &gst::Pipeline) -> Result<(), String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|error| format!("connect: {error}"))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
    let bus = pipeline.bus().ok_or("no bus")?;
    let mut header = [0u8; HEADER_BYTES];
    while !self.stop.load(Ordering::Acquire) {
      if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
        if let gst::MessageView::Error(error) = message.view() {
          return Err(format!("GStreamer: {}", error.error()));
        }
      }
      match read_full(&mut stream, &mut header, &self.stop) {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(error) => return Err(error),
      }
      let length = u32::from_be_bytes(header[..4].try_into().unwrap_or_default()) as usize;
      if length > 64 << 20 { return Err("oversized frame".into()); }
      let mut payload = vec![0u8; length];
      match read_full(&mut stream, &mut payload, &self.stop) {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(error) => return Err(error),
      }
      source.push_buffer(gst::Buffer::from_mut_slice(payload)).map_err(|error| format!("push: {error:?}"))?;
    }
    Ok(())
  }
}

/// Fills `buffer`, tolerating read timeouts (so `stop` is noticed). Ok(false)
/// means the stream ended or we were stopped.
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
