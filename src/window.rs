//! Native window. The page talks to the daemon through a custom scheme, so no HTTP port is opened.

use muda::{Menu, PredefinedMenuItem, Submenu};
use rust_embed::Embed;
use std::rc::Rc;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::macos::WindowBuilderExtMacOS;
use tao::window::WindowBuilder;
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::WebViewBuilder;

#[derive(Embed)]
#[folder = "ui/"]
struct Ui;

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn respond(req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = req.uri().path().trim_start_matches('/');
    if path == "rpc" {
        let body = crate::call(&String::from_utf8_lossy(req.body())).unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e }).to_string());
        return Response::builder().header(CONTENT_TYPE, "application/json").body(body.into_bytes()).unwrap();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    match Ui::get(path) {
        Some(file) => Response::builder().header(CONTENT_TYPE, mime(path)).body(file.data.into_owned()).unwrap(),
        None => Response::builder().status(404).body(vec![]).unwrap(),
    }
}

// Without these, Cmd+Q and copy/paste do nothing in a macOS webview.
fn app_menu() -> Result<Menu, muda::Error> {
    let app = Submenu::with_items(
        "Hyperspeed",
        true,
        &[&PredefinedMenuItem::hide(None), &PredefinedMenuItem::separator(), &PredefinedMenuItem::quit(None)],
    )?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[&PredefinedMenuItem::cut(None), &PredefinedMenuItem::copy(None), &PredefinedMenuItem::paste(None), &PredefinedMenuItem::select_all(None)],
    )?;
    let window = Submenu::with_items("Window", true, &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::close_window(None)])?;
    Menu::with_items(&[&app, &edit, &window])
}

pub fn open() -> Result<(), String> {
    let event_loop = EventLoop::new();
    let menu = app_menu().map_err(|e| e.to_string())?;
    menu.init_for_nsapp();

    let window = Rc::new(
        WindowBuilder::new()
            .with_title("Hyperspeed")
            .with_inner_size(LogicalSize::new(1360.0, 900.0))
            .with_min_inner_size(LogicalSize::new(1080.0, 680.0))
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
            .build(&event_loop)
            .map_err(|e| e.to_string())?,
    );

    let drag_target = window.clone();
    let webview = WebViewBuilder::new()
        .with_asynchronous_custom_protocol("hyperspeed".into(), |_id, req, responder| {
            std::thread::spawn(move || responder.respond(respond(&req)));
        })
        .with_url("hyperspeed://app/index.html")
        .with_navigation_handler(|url| url.starts_with("hyperspeed://"))
        .with_ipc_handler(move |req| {
            if req.body() == "drag" {
                let _ = drag_target.drag_window();
            }
        })
        .with_background_color((8, 8, 8, 255))
        .with_accept_first_mouse(true)
        .build(&*window)
        .map_err(|e| e.to_string())?;

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _ = &webview;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *flow = ControlFlow::Exit; // servers keep running in the daemon
        }
    });
}
